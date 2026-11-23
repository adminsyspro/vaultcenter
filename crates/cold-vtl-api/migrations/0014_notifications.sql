-- All producers (including privileged Python helpers) share these transitions.
CREATE TABLE notification_settings (
 id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL,
 configuration TEXT NOT NULL CHECK(json_valid(configuration))
);
INSERT INTO notification_settings VALUES(1,0,'{"enabled":false}');
CREATE TABLE notification_events (
 id TEXT PRIMARY KEY, payload TEXT NOT NULL CHECK(json_valid(payload)),
 recipients TEXT NOT NULL CHECK(json_valid(recipients)),
 created_at INTEGER NOT NULL DEFAULT (unixepoch()), expanded INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE notification_deliveries (
 id TEXT PRIMARY KEY, event_id TEXT NOT NULL REFERENCES notification_events(id),
 recipient TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued'
 CHECK(status IN ('queued','sending','sent','failed','uncertain')),
 attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT (unixepoch()),
 category TEXT, updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
 UNIQUE(event_id,recipient)
);
CREATE INDEX notification_delivery_due ON notification_deliveries(status,next_attempt);
CREATE TRIGGER notification_job_terminal AFTER UPDATE OF status ON jobs
WHEN OLD.status<>NEW.status AND NEW.status IN ('completed','failed')
 AND NEW.kind IN ('export','restore','reintegrate','archive_prepare','archive_submit','recall_prepare','recall_submit')
 AND (NEW.status='failed' OR NEW.kind IN ('export','restore','reintegrate'))
 AND NOT (NEW.kind IN ('archive_submit','recall_submit') AND EXISTS(SELECT 1 FROM archive_requests WHERE job_id=NEW.id AND phase IN ('archived','restored')))
BEGIN
 INSERT OR IGNORE INTO notification_events(id,payload,recipients)
 SELECT CASE WHEN NEW.kind IN ('archive_submit','recall_submit') THEN 'provider:'||NEW.id||':'||CASE WHEN EXISTS(SELECT 1 FROM archive_requests WHERE job_id=NEW.id AND phase IN ('submitting','accepted','uncertain')) THEN 'uncertain' ELSE 'failed' END ELSE 'job:'||NEW.id||':'||NEW.status END,
 json_object('job_id',NEW.id,'vtl_id',NEW.vtl_id,
 'vtl_name',(SELECT name FROM vtls WHERE id=NEW.vtl_id),
 'destination_id',NEW.destination_id,'bucket',(SELECT json_extract(configuration,'$.bucket') FROM storage_destination_revisions WHERE destination_id=NEW.destination_id AND revision=NEW.configuration_revision),'subject',NEW.subject,
 'kind',NEW.kind,'outcome',CASE WHEN NEW.status='completed' THEN 'success' WHEN NEW.kind IN ('archive_submit','recall_submit') AND EXISTS(SELECT 1 FROM archive_requests WHERE job_id=NEW.id AND phase IN ('submitting','accepted','uncertain')) THEN 'uncertain' ELSE 'failure' END,
 'occurred_at',strftime('%Y-%m-%dT%H:%M:%SZ','now')),
 json_extract(configuration,'$.recipients') FROM notification_settings
 WHERE json_extract(configuration,'$.enabled')=1
 AND json_extract(configuration,'$.events.'||
 CASE WHEN NEW.kind LIKE 'archive%' THEN 'archive' WHEN NEW.kind LIKE 'recall%' THEN 'recall' ELSE NEW.kind END||'.'||
 CASE NEW.status WHEN 'completed' THEN 'success' ELSE 'failure' END)=1;
END;
CREATE TRIGGER notification_provider_terminal AFTER UPDATE OF phase ON archive_requests
WHEN OLD.phase<>NEW.phase AND NEW.phase IN ('archived','restored','uncertain','failed')
BEGIN
 INSERT OR IGNORE INTO notification_events(id,payload,recipients)
 SELECT 'provider:'||NEW.job_id||':'||NEW.phase,
 json_object('job_id',NEW.job_id,'vtl_id',NEW.vtl_id,
 'vtl_name',(SELECT name FROM vtls WHERE id=NEW.vtl_id),
 'destination_id',NEW.destination_id,'bucket',json_extract(NEW.request,'$.confirm_bucket'),'subject',COALESCE(json_extract(NEW.request,'$.confirm_bucket'),'Bucket'),
 'kind',NEW.operation,
 'outcome',CASE WHEN NEW.phase IN ('archived','restored') THEN 'success' WHEN NEW.phase='uncertain' THEN 'uncertain' ELSE 'failure' END,
 'occurred_at',strftime('%Y-%m-%dT%H:%M:%SZ','now')),
 json_extract(configuration,'$.recipients') FROM notification_settings
 WHERE json_extract(configuration,'$.enabled')=1
 AND json_extract(configuration,'$.events.'||NEW.operation||'.'||
 CASE WHEN NEW.phase IN ('archived','restored') THEN 'success' ELSE 'failure' END)=1;
END;
