-- An uncertain creation is now settled by asking the provider about its name.
-- A bucket found with content is never adopted: the attempt ends `occupied`.
-- Nothing references bucket_creations, so it can be rebuilt in place.
CREATE TABLE bucket_creations_next (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id),
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    source_destination_id TEXT,
    next_destination_id TEXT,
    next_bucket TEXT NOT NULL,
    configuration TEXT NOT NULL,
    revision INTEGER NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('reserved','creating','switched','uncertain','abandoned','occupied')),
    checked_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(vtl_id,job_id) REFERENCES jobs(vtl_id,id),
    FOREIGN KEY(vtl_id,source_destination_id) REFERENCES storage_destinations(vtl_id,id),
    FOREIGN KEY(vtl_id,next_destination_id) REFERENCES storage_destinations(vtl_id,id)
);
INSERT INTO bucket_creations_next(job_id,vtl_id,source_destination_id,next_destination_id,next_bucket,configuration,revision,phase,created_at,updated_at)
SELECT job_id,vtl_id,source_destination_id,next_destination_id,next_bucket,configuration,revision,phase,created_at,updated_at FROM bucket_creations;
DROP TABLE bucket_creations;
ALTER TABLE bucket_creations_next RENAME TO bucket_creations;
