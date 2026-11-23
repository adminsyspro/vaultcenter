-- Retain identities, export references and job history after local file removal.
CREATE TABLE cartridge_deletions (
    vtl_id TEXT NOT NULL,
    barcode TEXT NOT NULL,
    request_id TEXT NOT NULL,
    deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (vtl_id, barcode),
    FOREIGN KEY (vtl_id, barcode) REFERENCES cartridges(vtl_id, barcode)
);
