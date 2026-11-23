CREATE TABLE s3_settings (
    id INTEGER PRIMARY KEY CHECK (id=1),
    revision INTEGER NOT NULL,
    configuration TEXT NOT NULL,
    actor TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE s3_reconcile_required (
    barcode TEXT PRIMARY KEY REFERENCES cartridges(barcode)
);
