-- When OVH made a recalled bucket readable: its read-only window lasts 30 days.
ALTER TABLE archive_requests ADD COLUMN restored_at TEXT;
UPDATE archive_requests SET restored_at=COALESCE(observed_at,updated_at) WHERE phase='restored';
CREATE TRIGGER archive_request_restored AFTER UPDATE OF phase ON archive_requests
WHEN NEW.phase='restored' AND OLD.phase<>'restored' AND NEW.restored_at IS NULL
BEGIN
 UPDATE archive_requests SET restored_at=CURRENT_TIMESTAMP WHERE job_id=NEW.job_id;
END;
