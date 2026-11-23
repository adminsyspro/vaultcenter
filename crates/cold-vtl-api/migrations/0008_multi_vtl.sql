-- Preserve the single existing library and its object keys. Hardware activation
-- is a separate operation: this migration never provisions or restarts services.
CREATE TABLE vtls (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
    media_root TEXT UNIQUE,
    restore_root TEXT UNIQUE,
    lifecycle TEXT NOT NULL DEFAULT 'draft' CHECK(lifecycle IN ('draft','ready','failed','disabled')),
    device_configuration TEXT NOT NULL DEFAULT '{}',
    provisioning_revision INTEGER NOT NULL DEFAULT 1 CHECK(provisioning_revision > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO vtls(id,name) VALUES('00000000-0000-4000-8000-000000000001','VTL principale');

CREATE TABLE storage_destinations (
    id TEXT PRIMARY KEY NOT NULL,
    vtl_id TEXT NOT NULL REFERENCES vtls(id),
    identity TEXT,
    active INTEGER NOT NULL CHECK(active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(vtl_id,id)
);
CREATE UNIQUE INDEX destination_active_vtl ON storage_destinations(vtl_id) WHERE active=1;
INSERT INTO storage_destinations(id,vtl_id,active)
VALUES('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000001',1);
CREATE TABLE storage_destination_revisions (
    destination_id TEXT NOT NULL REFERENCES storage_destinations(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    configuration TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(destination_id,revision)
);
INSERT INTO storage_destination_revisions(destination_id,revision,configuration)
SELECT '00000000-0000-4000-8000-000000000002',revision,configuration FROM s3_settings;

CREATE TABLE vtl_provisioning (
    vtl_id TEXT PRIMARY KEY NOT NULL REFERENCES vtls(id),
    revision INTEGER NOT NULL,
    manifest TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL,
    phase TEXT NOT NULL,
    journal TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE remote_generations RENAME TO remote_generations_legacy;
ALTER TABLE s3_reconcile_required RENAME TO s3_reconcile_required_legacy;
ALTER TABLE cartridges RENAME TO cartridges_legacy;
CREATE TABLE cartridges (
    id TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-8' || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6)))),
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001' REFERENCES vtls(id),
    barcode TEXT NOT NULL,
    capacity_bytes INTEGER NOT NULL CHECK(capacity_bytes > 0),
    bytes_written INTEGER NOT NULL DEFAULT 0 CHECK(bytes_written >= 0),
    generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
    state TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(vtl_id,barcode),
    UNIQUE(vtl_id,id),
    UNIQUE(vtl_id,id,barcode)
);
INSERT INTO cartridges(barcode,capacity_bytes,bytes_written,generation,state,created_at,updated_at)
SELECT barcode,capacity_bytes,bytes_written,generation,state,created_at,updated_at FROM cartridges_legacy;

CREATE TABLE remote_generations (
    cartridge_id TEXT NOT NULL,
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001',
    barcode TEXT NOT NULL,
    destination_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000002',
    generation INTEGER NOT NULL CHECK(generation > 0),
    metadata TEXT NOT NULL,
    checked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(cartridge_id,destination_id,generation),
    FOREIGN KEY(vtl_id,cartridge_id,barcode) REFERENCES cartridges(vtl_id,id,barcode),
    FOREIGN KEY(vtl_id,barcode) REFERENCES cartridges(vtl_id,barcode),
    FOREIGN KEY(vtl_id,destination_id) REFERENCES storage_destinations(vtl_id,id)
);
INSERT INTO remote_generations(cartridge_id,barcode,generation,metadata,checked_at)
SELECT c.id,r.barcode,r.generation,r.metadata,r.checked_at FROM remote_generations_legacy r JOIN cartridges c ON c.barcode=r.barcode;
CREATE TABLE s3_reconcile_required (
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001',
    barcode TEXT NOT NULL,
    PRIMARY KEY(vtl_id,barcode),
    FOREIGN KEY(vtl_id,barcode) REFERENCES cartridges(vtl_id,barcode)
);
INSERT INTO s3_reconcile_required(barcode) SELECT barcode FROM s3_reconcile_required_legacy;
DROP TABLE remote_generations_legacy;
DROP TABLE s3_reconcile_required_legacy;
DROP TABLE cartridges_legacy;

ALTER TABLE archive_requests RENAME TO archive_requests_legacy;
ALTER TABLE jobs RENAME TO jobs_legacy;
DROP INDEX jobs_one_active_per_cartridge;
DROP INDEX jobs_created_at_idx;
DROP INDEX recall_one_pending;
CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001' REFERENCES vtls(id),
    cartridge_id TEXT,
    destination_id TEXT,
    configuration_revision INTEGER,
    kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    status TEXT NOT NULL,
    progress INTEGER NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 100),
    message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    generation INTEGER,
    destination TEXT,
    UNIQUE(vtl_id,id),
    FOREIGN KEY(vtl_id,cartridge_id,subject) REFERENCES cartridges(vtl_id,id,barcode),
    FOREIGN KEY(vtl_id,destination_id) REFERENCES storage_destinations(vtl_id,id),
    FOREIGN KEY(destination_id,configuration_revision) REFERENCES storage_destination_revisions(destination_id,revision)
);
INSERT INTO jobs(id,cartridge_id,kind,subject,status,progress,message,created_at,updated_at,generation,destination)
SELECT j.id,c.id,j.kind,j.subject,j.status,j.progress,j.message,j.created_at,j.updated_at,j.generation,j.destination
FROM jobs_legacy j LEFT JOIN cartridges c ON c.barcode=j.subject;
CREATE INDEX jobs_created_at_idx ON jobs(vtl_id,created_at DESC);
CREATE UNIQUE INDEX jobs_active_cartridge ON jobs(cartridge_id)
WHERE cartridge_id IS NOT NULL AND status IN ('queued','running');
CREATE UNIQUE INDEX jobs_one_active_per_cartridge ON jobs(vtl_id,subject)
WHERE status IN ('queued','running');

CREATE TABLE archive_requests (
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001' REFERENCES vtls(id),
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id),
    prepare_job_id TEXT UNIQUE NOT NULL REFERENCES jobs(id),
    destination_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000002' REFERENCES storage_destinations(id),
    phase TEXT NOT NULL,
    request TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    operation TEXT NOT NULL DEFAULT 'archive' CHECK(operation IN ('archive','recall')),
    observed_status TEXT,
    observed_at TEXT,
    FOREIGN KEY(vtl_id,job_id) REFERENCES jobs(vtl_id,id),
    FOREIGN KEY(vtl_id,prepare_job_id) REFERENCES jobs(vtl_id,id),
    FOREIGN KEY(vtl_id,destination_id) REFERENCES storage_destinations(vtl_id,id)
);
INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,updated_at,operation,observed_status,observed_at)
SELECT job_id,prepare_job_id,phase,request,updated_at,operation,observed_status,observed_at FROM archive_requests_legacy;
CREATE UNIQUE INDEX recall_one_pending ON archive_requests(destination_id,operation)
WHERE operation='recall' AND phase IN ('queued','submitting','uncertain','accepted');
DROP TABLE archive_requests_legacy;
DROP TABLE jobs_legacy;

ALTER TABLE maintenance_events RENAME TO maintenance_events_legacy;
CREATE TABLE maintenance_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vtl_id TEXT NOT NULL DEFAULT '00000000-0000-4000-8000-000000000001' REFERENCES vtls(id),
    actor TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('enter','leave')),
    success INTEGER NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO maintenance_events(id,actor,action,success,message,created_at)
SELECT id,actor,action,success,message,created_at FROM maintenance_events_legacy;
DROP TABLE maintenance_events_legacy;
