-- Independent observations, not catalogue capacity or PBS job progress.
CREATE TABLE telemetry_samples (
    id INTEGER PRIMARY KEY,
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    drive_id TEXT NOT NULL CHECK(length(drive_id) BETWEEN 1 AND 120),
    barcode TEXT NOT NULL CHECK(length(barcode) BETWEEN 1 AND 32),
    at_ms INTEGER NOT NULL CHECK(at_ms >= 0),
    write_rate_bps REAL CHECK(write_rate_bps BETWEEN 0 AND 9007199254740991),
    local_growth_bps REAL CHECK(local_growth_bps BETWEEN 0 AND 9007199254740991),
    UNIQUE(vtl_id,drive_id,barcode,at_ms)
);
CREATE INDEX telemetry_samples_retention ON telemetry_samples(at_ms);
CREATE INDEX telemetry_samples_vtl_time ON telemetry_samples(vtl_id,at_ms);
