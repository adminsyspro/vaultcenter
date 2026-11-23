-- Native column changes retain the parent table name, cartridge IDs, indexes,
-- and every composite foreign key from jobs and remote generations. DROP COLUMN
-- removes only the old inline CHECK; SQLite rewrites rows transactionally.
ALTER TABLE cartridges RENAME COLUMN segment_size_mib TO previous_segment_size_mib;
ALTER TABLE cartridges ADD COLUMN segment_size_mib INTEGER NOT NULL DEFAULT 1048576
    CHECK (segment_size_mib BETWEEN 16 AND 1048576);
UPDATE cartridges SET segment_size_mib=previous_segment_size_mib;
ALTER TABLE cartridges DROP COLUMN previous_segment_size_mib;
ALTER TABLE cartridges ADD COLUMN multipart_part_size_mib INTEGER NOT NULL DEFAULT 256
    CHECK (multipart_part_size_mib BETWEEN 16 AND 1024
        AND segment_size_mib <= multipart_part_size_mib * 10000);
