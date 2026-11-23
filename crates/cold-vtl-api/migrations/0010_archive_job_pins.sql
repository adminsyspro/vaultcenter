-- Recover only archive/recall jobs whose historical preparation proves an exact
-- persisted configuration. Missing/ambiguous provenance remains unpinned so the
-- worker and helper fail closed; never infer the current or latest revision.
CREATE TEMP TABLE archive_pin_backfill (
    submit_id TEXT PRIMARY KEY,
    prepare_id TEXT NOT NULL,
    destination_id TEXT NOT NULL,
    revision INTEGER NOT NULL
);

WITH evidence AS (
    SELECT a.job_id, a.prepare_job_id, a.vtl_id, a.destination_id, a.operation,
        CASE WHEN json_valid(a.request) THEN a.request ELSE '{}' END AS request,
        CASE WHEN json_valid(p.message) THEN p.message ELSE '{}' END AS receipt
    FROM archive_requests a
    JOIN jobs p ON p.id=a.prepare_job_id AND p.vtl_id=a.vtl_id
    WHERE p.status='completed' AND p.kind=a.operation || '_prepare'
), proven AS (
    SELECT e.*, json_extract(e.receipt,'$.config_revision') AS revision
    FROM evidence e
    JOIN storage_destinations d ON d.id=e.destination_id AND d.vtl_id=e.vtl_id
    JOIN storage_destination_revisions r ON r.destination_id=d.id
        AND r.revision=json_extract(e.receipt,'$.config_revision')
    WHERE json_type(e.receipt,'$.config_revision')='integer'
        AND json_extract(e.receipt,'$.config_revision') > 0
        AND json_type(e.request,'$.confirm_bucket')='text'
        AND length(json_extract(e.request,'$.confirm_bucket')) > 0
        AND json_extract(e.receipt,'$.scope.bucket')=json_extract(e.request,'$.confirm_bucket')
        AND json_extract(CASE WHEN json_valid(r.configuration) THEN r.configuration ELSE '{}' END,'$.bucket')=json_extract(e.request,'$.confirm_bucket')
)
INSERT INTO archive_pin_backfill
SELECT e.job_id,e.prepare_job_id,e.destination_id,e.revision
FROM proven e
JOIN jobs j ON j.id=e.job_id AND j.vtl_id=e.vtl_id AND j.kind=e.operation || '_submit'
JOIN jobs p ON p.id=e.prepare_job_id AND p.vtl_id=e.vtl_id
WHERE (j.destination_id IS NULL OR j.destination_id=e.destination_id)
    AND (j.configuration_revision IS NULL OR j.configuration_revision=e.revision)
    AND (p.destination_id IS NULL OR p.destination_id=e.destination_id)
    AND (p.configuration_revision IS NULL OR p.configuration_revision=e.revision);

UPDATE jobs
SET destination_id=COALESCE(destination_id,(
        SELECT destination_id FROM archive_pin_backfill WHERE submit_id=jobs.id OR prepare_id=jobs.id)),
    configuration_revision=COALESCE(configuration_revision,(
        SELECT revision FROM archive_pin_backfill WHERE submit_id=jobs.id OR prepare_id=jobs.id))
WHERE (destination_id IS NULL OR configuration_revision IS NULL)
    AND EXISTS(SELECT 1 FROM archive_pin_backfill WHERE submit_id=jobs.id OR prepare_id=jobs.id);

DROP TABLE archive_pin_backfill;
