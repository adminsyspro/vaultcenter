CREATE TABLE bucket_rotations (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id),
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    source_destination_id TEXT NOT NULL,
    next_destination_id TEXT,
    next_bucket TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('reserved','creating','ready','switched','failed','uncertain')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(vtl_id,job_id) REFERENCES jobs(vtl_id,id),
    FOREIGN KEY(vtl_id,source_destination_id) REFERENCES storage_destinations(vtl_id,id),
    FOREIGN KEY(vtl_id,next_destination_id) REFERENCES storage_destinations(vtl_id,id)
);
