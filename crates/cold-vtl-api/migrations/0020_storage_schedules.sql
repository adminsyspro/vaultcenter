-- Per-VTL schedule. A missing row means: manual rotation, no automatic archiving.
-- rotation_served_at records when the rotation setting last changed: the period
-- running at that moment counts as served (no immediate second bucket).
CREATE TABLE storage_schedules (
    vtl_id TEXT PRIMARY KEY NOT NULL REFERENCES vtls(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    rotation TEXT NOT NULL CHECK(rotation IN ('manual','weekly','monthly')),
    weekday INTEGER CHECK(weekday BETWEEN 1 AND 7),
    month_day INTEGER CHECK(month_day BETWEEN 1 AND 31),
    rotation_served_at TEXT,
    archive_after_days INTEGER CHECK(archive_after_days BETWEEN 1 AND 365),
    archive_terms_accepted_at TEXT,
    archive_terms_accepted_by TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK((rotation='weekly') = (weekday IS NOT NULL)),
    CHECK((rotation='monthly') = (month_day IS NOT NULL)),
    CHECK(archive_after_days IS NULL OR archive_terms_accepted_at IS NOT NULL)
);
