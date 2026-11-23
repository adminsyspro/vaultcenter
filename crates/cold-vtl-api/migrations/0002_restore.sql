ALTER TABLE jobs ADD COLUMN generation INTEGER;
ALTER TABLE jobs ADD COLUMN destination TEXT;

UPDATE jobs SET status = 'failed', message = 'daemon restarted while job was active', updated_at = CURRENT_TIMESTAMP
WHERE status IN ('queued', 'running');

CREATE UNIQUE INDEX jobs_one_active_per_cartridge ON jobs(subject)
WHERE status IN ('queued', 'running');

CREATE TABLE remote_generations (
    barcode TEXT NOT NULL REFERENCES cartridges(barcode),
    generation INTEGER NOT NULL CHECK (generation > 0),
    metadata TEXT NOT NULL,
    checked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (barcode, generation)
);
