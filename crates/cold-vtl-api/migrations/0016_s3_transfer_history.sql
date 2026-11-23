-- Acknowledged payload upload rates, never estimates from job progress or media size.
CREATE TABLE s3_transfer_samples (
    id INTEGER PRIMARY KEY,
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    transfer_id TEXT NOT NULL,
    barcode TEXT NOT NULL CHECK(length(barcode) BETWEEN 1 AND 32),
    at_ms INTEGER NOT NULL CHECK(at_ms >= 0),
    elapsed_ms REAL NOT NULL DEFAULT 5000 CHECK(elapsed_ms > 0),
    write_rate_bps REAL CHECK(write_rate_bps BETWEEN 0 AND 9007199254740991),
    FOREIGN KEY(vtl_id,transfer_id) REFERENCES jobs(vtl_id,id),
    UNIQUE(vtl_id,transfer_id,barcode,at_ms)
);
CREATE INDEX s3_transfer_samples_retention ON s3_transfer_samples(at_ms);
CREATE INDEX s3_transfer_samples_scope ON s3_transfer_samples(vtl_id,barcode,transfer_id,at_ms);
