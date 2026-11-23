CREATE TABLE archive_requests (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id),
    prepare_job_id TEXT UNIQUE NOT NULL REFERENCES jobs(id),
    phase TEXT NOT NULL,
    request TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
