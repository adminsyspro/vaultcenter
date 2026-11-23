-- When a destination stopped being active through a manual change of bucket
-- (applied configuration or existing bucket made active). Automatic switches
-- keep their own record in bucket_creations / bucket_rotations.
ALTER TABLE storage_destinations ADD COLUMN deactivated_at TEXT;
