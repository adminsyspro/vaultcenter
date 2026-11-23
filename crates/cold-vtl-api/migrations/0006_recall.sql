ALTER TABLE archive_requests ADD COLUMN operation TEXT NOT NULL DEFAULT 'archive'
    CHECK(operation IN ('archive','recall'));
ALTER TABLE archive_requests ADD COLUMN observed_status TEXT;
ALTER TABLE archive_requests ADD COLUMN observed_at TEXT;
-- A recall remains unique until its availability has actually been observed.
CREATE UNIQUE INDEX recall_one_pending ON archive_requests(operation)
WHERE operation='recall' AND phase IN ('queued','submitting','uncertain','accepted');
