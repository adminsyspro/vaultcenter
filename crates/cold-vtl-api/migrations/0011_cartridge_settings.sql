ALTER TABLE cartridges ADD COLUMN media_type TEXT;
ALTER TABLE cartridges ADD COLUMN segment_size_mib INTEGER NOT NULL DEFAULT 256 CHECK (segment_size_mib BETWEEN 16 AND 1024);
