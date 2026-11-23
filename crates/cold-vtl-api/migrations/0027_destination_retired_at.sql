-- When a bucket that OVH no longer knows was removed from the bucket list. The
-- row stays for the history of its jobs and copies; activating it again
-- shows it again.
ALTER TABLE storage_destinations ADD COLUMN retired_at TEXT;
CREATE TRIGGER destination_unretired AFTER UPDATE OF active ON storage_destinations
WHEN NEW.active=1 AND NEW.retired_at IS NOT NULL
BEGIN
 UPDATE storage_destinations SET retired_at=NULL WHERE id=NEW.id;
END;
