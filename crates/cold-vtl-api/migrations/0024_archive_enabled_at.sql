-- When automatic archiving was last enabled (archive_after_days went from NULL
-- to a value); NULL while it is disabled. Only buckets that stopped being
-- active after this moment are archived automatically: older ones stay manual.
-- A schedule already enabled starts counting from this upgrade.
ALTER TABLE storage_schedules ADD COLUMN archive_enabled_at TEXT;
UPDATE storage_schedules SET archive_enabled_at=CURRENT_TIMESTAMP WHERE archive_after_days IS NOT NULL;
