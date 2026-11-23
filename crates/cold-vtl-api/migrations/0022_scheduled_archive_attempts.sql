-- At most one automatic archive attempt per bucket and UTC day; a refused
-- attempt is retried the next day and never forced.
CREATE TABLE scheduled_archive_attempts (
    destination_id TEXT PRIMARY KEY NOT NULL,
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    attempted_on TEXT NOT NULL,
    job_id TEXT REFERENCES jobs(id),
    FOREIGN KEY(vtl_id,destination_id) REFERENCES storage_destinations(vtl_id,id)
);
