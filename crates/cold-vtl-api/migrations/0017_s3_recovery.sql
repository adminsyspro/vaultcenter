-- Recovery receipts are independent of cartridge rows. The source job pins S3,
-- while the target job reserves installation without inventing local media.
CREATE TABLE recovery_requests (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    target_job_id TEXT NOT NULL REFERENCES jobs(id),
    source_vtl_id TEXT NOT NULL REFERENCES vtls(id),
    target_vtl_id TEXT NOT NULL REFERENCES vtls(id),
    request TEXT NOT NULL CHECK(json_valid(request)),
    actor TEXT NOT NULL,
    phase TEXT NOT NULL DEFAULT 'queued',
    receipt TEXT CHECK(receipt IS NULL OR json_valid(receipt)),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
