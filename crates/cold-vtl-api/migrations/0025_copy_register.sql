-- Dated register of every S3 copy: remote_generations only holds the current
-- assurance (its rows are rewritten by a sync and deleted by an S3 deletion),
-- so the export date and the removal are kept here. Rows are never deleted.
CREATE TABLE copy_register (
    vtl_id TEXT NOT NULL,
    destination_id TEXT NOT NULL,
    barcode TEXT NOT NULL,
    generation INTEGER NOT NULL,
    recorded_at TEXT NOT NULL,
    bytes INTEGER,
    manifest_sha256 TEXT,
    removed_at TEXT,
    PRIMARY KEY(vtl_id,destination_id,barcode,generation),
    FOREIGN KEY(vtl_id,barcode) REFERENCES cartridges(vtl_id,barcode),
    FOREIGN KEY(vtl_id,destination_id) REFERENCES storage_destinations(vtl_id,id)
);
-- A sync deletes then reinserts in one transaction: the reinsertion clears
-- the removal and keeps the first export date.
CREATE TRIGGER copy_register_insert AFTER INSERT ON remote_generations
BEGIN
 INSERT INTO copy_register(vtl_id,destination_id,barcode,generation,recorded_at,bytes,manifest_sha256)
 VALUES(NEW.vtl_id,NEW.destination_id,NEW.barcode,NEW.generation,NEW.checked_at,
  json_extract(NEW.metadata,'$.logical_size_bytes'),json_extract(NEW.metadata,'$.manifest_sha256'))
 ON CONFLICT(vtl_id,destination_id,barcode,generation) DO UPDATE
 SET removed_at=NULL,bytes=excluded.bytes,manifest_sha256=excluded.manifest_sha256;
END;
CREATE TRIGGER copy_register_update AFTER UPDATE OF metadata ON remote_generations
BEGIN
 UPDATE copy_register SET bytes=json_extract(NEW.metadata,'$.logical_size_bytes'),
  manifest_sha256=json_extract(NEW.metadata,'$.manifest_sha256')
 WHERE vtl_id=NEW.vtl_id AND destination_id=NEW.destination_id AND barcode=NEW.barcode AND generation=NEW.generation;
END;
CREATE TRIGGER copy_register_delete AFTER DELETE ON remote_generations
BEGIN
 UPDATE copy_register SET removed_at=CURRENT_TIMESTAMP
 WHERE vtl_id=OLD.vtl_id AND destination_id=OLD.destination_id AND barcode=OLD.barcode AND generation=OLD.generation;
END;
INSERT INTO copy_register(vtl_id,destination_id,barcode,generation,recorded_at,bytes,manifest_sha256)
SELECT r.vtl_id,r.destination_id,r.barcode,r.generation,
 COALESCE((SELECT MIN(j.updated_at) FROM jobs j WHERE j.kind='export' AND j.status='completed' AND j.vtl_id=r.vtl_id
  AND j.destination_id=r.destination_id AND j.subject=r.barcode AND j.generation=r.generation),r.checked_at),
 json_extract(r.metadata,'$.logical_size_bytes'),json_extract(r.metadata,'$.manifest_sha256')
FROM remote_generations r;

-- When OVH archived a bucket, kept like restored_at (0021). Requests archived
-- before this upgrade only have their last observation.
ALTER TABLE archive_requests ADD COLUMN archived_at TEXT;
UPDATE archive_requests SET archived_at=COALESCE(observed_at,updated_at) WHERE phase='archived' AND operation='archive';
CREATE TRIGGER archive_request_archived AFTER UPDATE OF phase ON archive_requests
WHEN NEW.phase='archived' AND OLD.phase<>'archived' AND NEW.archived_at IS NULL
BEGIN
 UPDATE archive_requests SET archived_at=CURRENT_TIMESTAMP WHERE job_id=NEW.job_id;
END;
