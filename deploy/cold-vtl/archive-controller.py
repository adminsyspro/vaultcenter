#!/usr/bin/env python3
"""Serialized archive and recall requests. A cloud PUT is never retried here."""
import fcntl
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import uuid

helper = Path(__file__).with_name('cold-vtl-maintenance')
if not helper.exists():
    helper = Path(__file__).with_name('maintenance-controller.py')
loader = importlib.machinery.SourceFileLoader('archive_maintenance', str(helper))
spec = importlib.util.spec_from_loader(loader.name, loader)
maintenance = importlib.util.module_from_spec(spec)
loader.exec_module(maintenance)


NOT_CREATED = {
    'credentials': 'The next bucket was not created. The provider rejected the credentials. Fix the connection, then start the archive again.',
    'denied': "The next bucket was not created. The provider denied access. Check the connection keys and the user's S3 policy.",
    'exists': 'The next bucket was not created. This name is already taken at the provider. Start the archive again to get a new name.',
    'name': 'The next bucket was not created. The provider rejected this name.',
    'unreachable': 'The next bucket was not created. The provider is unreachable. Check the endpoint, then start the archive again.',
}


class Archive:
    def __init__(self, config, root=maintenance.ROOT, controller=None):
        self.root = root
        self.database = Path(config.get('database', str(root / 'catalog.db')))
        self.worker = config.get('worker', '/usr/local/bin/cold-vtl-worker')
        self.controller = controller or maintenance.Controller(config, root)
        self.context = None
        self.vtl_id = config.get('vtl_id')
        self.runtime_revision = config.get('revision')

    def db(self):
        db = sqlite3.connect(self.database, timeout=15)
        db.row_factory = sqlite3.Row
        db.execute('PRAGMA synchronous=FULL')
        db.execute('PRAGMA foreign_keys=ON')
        return db

    def query(self, db, sql, args=(), alias=''):
        if self.vtl_id:
            sql += ' AND ' + alias + 'vtl_id=?'
            args = (*args, self.vtl_id)
        return db.execute(sql, args)

    def operation_lock(self):
        return self.root / ('operation.lock' if self.vtl_id else 'maintenance-controller.lock')

    def run_worker(self, args):
        # Never expose SDK errors containing headers/configuration in the job.
        env = os.environ.copy()
        env['COLD_VTL_DATABASE'] = str(self.database)
        for name in ('COLD_VTL_VTL_ID', 'COLD_VTL_DESTINATION_ID', 'COLD_VTL_CONFIGURATION_REVISION'):
            env.pop(name, None)
        if self.context:
            env.update(COLD_VTL_VTL_ID=self.context['vtl_id'],
                       COLD_VTL_DESTINATION_ID=self.context['destination_id'],
                       COLD_VTL_CONFIGURATION_REVISION=str(self.context['configuration_revision']))
        command=[self.worker] + args
        if self.vtl_id:
            env['COLD_VTL_RUNTIME_REVISION']=str(self.runtime_revision)
            command=['/usr/local/libexec/cold-vtl-worker-run']+args
        result = subprocess.run(command, capture_output=True, text=True, env=env,
                                timeout=20 if args[0] == 'bucket-status' else 86400)
        if result.returncode:
            if args[0] == 'create-bucket' and result.returncode == 3:
                # The provider refused before creating anything (worker exit 3).
                try:
                    refused = json.loads(result.stdout)
                except (ValueError, TypeError):
                    refused = None
                if isinstance(refused, dict) and refused.get('status') == 'not_created':
                    return refused
            if args[0] in ('archive-inspect','recall-inspect'):
                try:
                    message = json.loads(result.stdout).get('error')
                except (ValueError, TypeError):
                    message = None
                if isinstance(message, str):
                    raise RuntimeError(message)
            raise RuntimeError('S3 verification refused or request interrupted. Check the copies, the scope, the versions and S3 access.')
        return json.loads(result.stdout)

    def phase(self, job_id, phase):
        with self.db() as db:
            db.execute('UPDATE archive_requests SET phase=?,updated_at=CURRENT_TIMESTAMP WHERE job_id=?', (phase, job_id))

    def finish(self, job_id, status, message):
        with self.db() as db:
            db.execute('UPDATE jobs SET status=?,progress=100,message=?,updated_at=CURRENT_TIMESTAMP WHERE id=?',
                       (status, json.dumps(message) if isinstance(message, dict) else message, job_id))

    def idle(self, db, job_id):
        if self.query(db, "SELECT COUNT(*) FROM jobs WHERE status IN ('queued','running') AND id<>?", (job_id,)).fetchone()[0]:
            raise RuntimeError('Wait for all VTL jobs to finish.')
        self.controller.no_workers()

    def select_context(self, db, job, request=None):
        self.context = None
        if self.vtl_id and job['vtl_id'] != self.vtl_id:
            raise RuntimeError('Job belongs to another VTL.')
        if 'destination_id' not in job.keys():
            return  # Compatibility with pre-multi-VTL catalogues during upgrade.
        destination = job['destination_id']
        if not destination:
            raise RuntimeError('The S3 destination of the job is missing. Restart the operation from S3 Storage.')
        revision = job['configuration_revision']
        if not isinstance(revision, int) or revision <= 0:
            raise RuntimeError('The S3 revision of the job is missing. Restart the operation from S3 Storage.')
        row = db.execute('''SELECT r.revision FROM storage_destination_revisions r
                            JOIN storage_destinations d ON d.id=r.destination_id
                            WHERE d.id=? AND d.vtl_id=? AND r.revision=?
                            ORDER BY r.revision DESC LIMIT 1''',
                         (destination, job['vtl_id'], revision)).fetchone()
        if not row:
            raise RuntimeError('S3 configuration of the job not found.')
        self.context = dict(vtl_id=job['vtl_id'], destination_id=destination,
                            configuration_revision=row[0])
        if request:
            for name, value in self.context.items():
                if name in request and request[name] != value:
                    raise RuntimeError('Invalid S3 scope in the request.')

    def create_next_bucket(self, request):
        if not self.context:
            if request.get('next_bucket'):
                raise RuntimeError('Update the catalogue before creating a bucket.')
            return
        name = request.get('next_bucket', '')
        if not name:
            # An already rotated historical bucket has no successor to create.
            # Recheck the persisted source instead of trusting the HTTP request.
            with self.db() as db:
                source = db.execute('SELECT active FROM storage_destinations WHERE id=? AND vtl_id=?',
                                    (self.context['destination_id'], self.context['vtl_id'])).fetchone()
                if not source or source['active']:
                    raise RuntimeError('An active destination must have a confirmed next bucket.')
                if db.execute('SELECT 1 FROM bucket_rotations WHERE job_id=?', (request['job_id'],)).fetchone():
                    raise RuntimeError('Unexpected rotation for a historical bucket.')
            return
        # Same rule as cold_vtl_core::valid_managed_bucket_name.
        if not re.fullmatch(r'vc-[a-z0-9-]{0,59}[a-z0-9]', name):
            raise RuntimeError('Invalid next bucket name (vc- prefix required).')
        c = self.context
        next_id = str(uuid.uuid4())
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            source = db.execute('''SELECT d.active,d.identity,r.configuration FROM storage_destinations d
                                   JOIN storage_destination_revisions r ON r.destination_id=d.id
                                   WHERE d.id=? AND d.vtl_id=? AND r.revision=?''',
                                (c['destination_id'], c['vtl_id'], c['configuration_revision'])).fetchone()
            if not source or not source['active']:
                raise RuntimeError('Only the active bucket can be archived.')
            if db.execute("SELECT 1 FROM bucket_rotations WHERE vtl_id=? AND job_id<>? AND phase IN ('reserved','creating','ready','uncertain')", (c['vtl_id'], request['job_id'])).fetchone():
                raise RuntimeError('A previous rotation must be resolved before a new archive.')
            config = json.loads(source['configuration'])
            endpoint = config['endpoint'].rstrip('/').lower()
            for row in db.execute('SELECT identity FROM storage_destinations WHERE identity IS NOT NULL'):
                identity = json.loads(row[0])
                if identity['endpoint'].rstrip('/').lower() == endpoint and identity['bucket'] == name:
                    raise RuntimeError('This bucket is already registered: choose a new name.')
            config['bucket'] = name
            identity = json.loads(source['identity'])
            identity['bucket'] = name
            revision = db.execute('''SELECT COALESCE(MAX(r.revision),0)+1 FROM storage_destination_revisions r
                                     JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=?''', (c['vtl_id'],)).fetchone()[0]
            if db.execute("SELECT 1 FROM sqlite_master WHERE name='s3_connections'").fetchone():
                connection_revision = db.execute('SELECT revision FROM s3_connections WHERE vtl_id=?', (c['vtl_id'],)).fetchone()
                if connection_revision:
                    revision = max(revision, connection_revision[0] + 1)
            db.execute('INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES(?,?,?,0)',
                       (next_id, c['vtl_id'], json.dumps(identity)))
            db.execute('INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?)',
                       (next_id, revision, json.dumps(config)))
            reserved = db.execute('SELECT * FROM bucket_rotations WHERE job_id=?', (request['job_id'],)).fetchone()
            if reserved:
                if (reserved['phase'] != 'reserved' or reserved['source_destination_id'] != c['destination_id']
                        or reserved['vtl_id'] != c['vtl_id'] or reserved['next_bucket'] != name):
                    raise RuntimeError('Rotation missing, modified or already started.')
                db.execute("UPDATE bucket_rotations SET next_destination_id=?,phase='creating',updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (next_id, request['job_id']))
            else:
                db.execute('''INSERT INTO bucket_rotations(job_id,vtl_id,source_destination_id,next_destination_id,next_bucket,phase)
                              VALUES(?,?,?,?,?,'creating')''', (request['job_id'], c['vtl_id'], c['destination_id'], next_id, name))
        # Persist before creation. A lost response is not a reason to recreate or
        # adopt an existing bucket whose ownership cannot be proven.
        try:
            result = self.run_worker(['create-bucket', '--bucket', name])
        except Exception as error:
            # Kept for diagnosis; the outcome stays uncertain below (run_worker hides SDK details).
            print(f'create-bucket {name}: {error}', file=sys.stderr)
            result = None
        if isinstance(result, dict) and result.get('status') == 'not_created' and result.get('bucket') == name:
            # Definitive refusal: nothing was created, release the rotation now.
            with self.db() as db:
                db.execute('BEGIN IMMEDIATE')
                db.execute("UPDATE bucket_rotations SET phase='failed',next_destination_id=NULL,updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (request['job_id'],))
                db.execute('DELETE FROM storage_destination_revisions WHERE destination_id=?', (next_id,))
                db.execute('DELETE FROM storage_destinations WHERE id=? AND active=0', (next_id,))
            raise RuntimeError(NOT_CREATED.get(result.get('reason'), 'The next bucket was not created. The provider rejected the request.'))
        if not isinstance(result, dict) or result.get('status') != 'created' or result.get('bucket') != name:
            # Lost or ambiguous answer: never retried here; Task 3's monitor asks the provider.
            with self.db() as db:
                db.execute("UPDATE bucket_rotations SET phase='uncertain',updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (request['job_id'],))
            raise RuntimeError('The creation of the next bucket is not confirmed. The active bucket is kept. VaultCenter is checking the bucket at OVH.')
        with self.db() as db:
            db.execute("UPDATE bucket_rotations SET phase='ready',updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (request['job_id'],))

    def switch_bucket(self, job_id):
        if not self.context:
            return
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            rotation = db.execute('SELECT * FROM bucket_rotations WHERE job_id=?', (job_id,)).fetchone()
            if not rotation or rotation['phase'] == 'switched':
                return
            if rotation['phase'] != 'ready':
                raise RuntimeError('The next bucket is not ready: rotation suspended.')
            active = db.execute('SELECT id FROM storage_destinations WHERE vtl_id=? AND active=1', (rotation['vtl_id'],)).fetchone()
            if not active or active[0] != rotation['source_destination_id']:
                raise RuntimeError('The active bucket has changed: rotation suspended.')
            saved = db.execute('''SELECT revision,configuration FROM storage_destination_revisions
                                  WHERE destination_id=? ORDER BY revision DESC LIMIT 1''', (rotation['next_destination_id'],)).fetchone()
            if not saved:
                raise RuntimeError('Next bucket configuration missing.')
            db.execute('UPDATE storage_destinations SET active=0 WHERE id=?', (rotation['source_destination_id'],))
            db.execute('UPDATE storage_destinations SET active=1 WHERE id=? AND vtl_id=?', (rotation['next_destination_id'], rotation['vtl_id']))
            if rotation['vtl_id'] == '00000000-0000-4000-8000-000000000001':
                db.execute('''INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,?,?,'rotation')
                              ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,configuration=excluded.configuration,
                              actor=excluded.actor,updated_at=CURRENT_TIMESTAMP''', (saved['revision'], saved['configuration']))
            # The next bucket was created empty: nothing to synchronise before exporting.
            db.execute('DELETE FROM s3_reconcile_required WHERE vtl_id=?', (rotation['vtl_id'],))
            db.execute("UPDATE bucket_rotations SET phase='switched',updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (job_id,))


    def managed_destination(self, db):
        """A bucket VaultCenter created empty needs no exclusive-use confirmation."""
        if not self.context:
            return False
        destination = self.context['destination_id']
        if db.execute("SELECT 1 FROM sqlite_master WHERE name='bucket_rotations'").fetchone() and db.execute(
                "SELECT 1 FROM bucket_rotations WHERE next_destination_id=? AND phase IN ('ready','switched')",
                (destination,)).fetchone():
            return True
        return bool(db.execute("SELECT 1 FROM sqlite_master WHERE name='bucket_creations'").fetchone() and db.execute(
            "SELECT 1 FROM bucket_creations WHERE next_destination_id=? AND phase='switched'", (destination,)).fetchone())

    def inspect(self):
        # Only the bucket is verified; local cartridges are neither read nor locked.
        receipt = self.run_worker(['archive-inspect'])
        if self.context:
            receipt['destination_id'] = self.context['destination_id']
        return receipt

    def execute(self, action, request):
        operation = 'recall' if action.startswith('recall-') else 'archive'
        action = action.removeprefix('recall-')
        job_id = request.get('job_id', '')
        if not re.fullmatch(r'[0-9a-f-]{36}', job_id):
            raise RuntimeError('Invalid job identifier.')
        # Reject foreign identifiers before any failure bookkeeping.
        if self.vtl_id:
            with self.db() as db:
                if not self.query(db, 'SELECT 1 FROM jobs WHERE id=?', (job_id,)).fetchone():
                    raise RuntimeError('Job not found in this VTL.')
        try:
            with self.operation_lock().open('a+') as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    raise RuntimeError('An operation already holds the maintenance lock.')
                maintenance.revalidate(self.controller.config)
                from cartridge_deletion import pending
                if pending(self.root):
                    raise RuntimeError("Complete interrupted local deletion before an S3 operation.")
                with self.db() as db:
                    job = self.query(db, 'SELECT * FROM jobs WHERE id=?', (job_id,)).fetchone()
                    if not job or job['kind'] != operation + '_' + action or job['status'] not in ('queued', 'running'):
                        raise RuntimeError('Archive job invalid or already finished.')
                    self.select_context(db, job, request)
                    self.idle(db, job_id)
                    if action != 'refresh' and self.query(db, "SELECT COUNT(*) FROM archive_requests WHERE phase IN ('queued','submitting','uncertain') AND job_id<>?", (job_id,)).fetchone()[0]:
                        raise RuntimeError('A previous request must be resolved in tracking.')
                    if operation == 'recall' and self.query(db, "SELECT COUNT(*) FROM archive_requests WHERE operation='recall' AND phase='accepted'").fetchone()[0]:
                        raise RuntimeError('A recall has already been accepted: wait for availability in tracking.')
                    db.execute("UPDATE jobs SET status='running',progress=10,message='Checking the bucket copies',updated_at=CURRENT_TIMESTAMP WHERE id=?", (job_id,))
                if action == 'submit':
                    result = self.recall_submit(request) if operation == 'recall' else self.submit(request)
                elif action == 'refresh':
                    result = self.refresh()
                else:
                    raise RuntimeError('Unknown action.')
                self.finish(job_id, 'completed', result)
                return result
        except Exception as error:
            with self.db() as db:
                # Before the durable submitting boundary, no cloud PUT can have run.
                db.execute("UPDATE archive_requests SET phase=CASE WHEN phase='queued' THEN 'failed' WHEN phase='submitting' THEN 'uncertain' ELSE phase END,updated_at=CURRENT_TIMESTAMP WHERE job_id=?", (job_id,))
                phase = db.execute('SELECT phase FROM archive_requests WHERE job_id=?', (job_id,)).fetchone()
                if phase and phase[0] == 'failed' and db.execute("SELECT 1 FROM sqlite_master WHERE name='bucket_rotations'").fetchone():
                    db.execute("UPDATE bucket_rotations SET phase='failed',updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase IN ('reserved','ready')", (job_id,))
            message = str(error)
            if phase and phase[0] == 'uncertain':
                message = 'Uncertain OVH result: do not resend the request. Use Check request with OVH. ' + message
            self.finish(job_id, 'failed', message)
            raise

    def submit(self, request):
        job_id = request['job_id']
        with self.db() as db:
            intent = self.query(db, 'SELECT * FROM archive_requests WHERE job_id=?', (job_id,)).fetchone()
            if (not intent or intent['phase'] != 'queued' or intent['prepare_job_id'] != job_id
                    or json.loads(intent['request']) != request):
                raise RuntimeError('Confirmation missing, modified or already used.')
            managed = self.managed_destination(db)
        if not request.get('acknowledge_billing') or not (request.get('exclusive_bucket') or managed):
            raise RuntimeError('Confirmation of the OVH conditions required.')
        receipt = self.inspect()
        if request.get('confirm_bucket') != receipt['scope']['bucket']:
            raise RuntimeError('Confirmation belongs to another bucket.')
        self.create_next_bucket(request)
        # Durable boundary precedes the process launch. Even a lost response or a
        # power failure can only lead to observation, never an automatic retry.
        self.phase(job_id, 'submitting')
        value = self.run_worker(['archive-bucket', '--confirm-bucket', request['confirm_bucket'],
                                 '--acknowledge-minimum-1-tib-billing', '--expected-receipt', receipt['digest']])
        if value.get('status') != 'request_accepted' or value.get('bucket') != request['confirm_bucket']:
            raise RuntimeError('OVH acknowledgement not confirmed.')
        self.phase(job_id, 'accepted')
        self.switch_bucket(job_id)
        inventory = receipt['scope']['inventory']
        return {'status': 'request_accepted', 'bucket': request['confirm_bucket'],
                'verified': {'cartridges': len(inventory['cartridges']), 'bytes': inventory['bytes'],
                             'copies': len(receipt['scope'].get('generations', []))},
                'message': 'Request accepted by OVH. Archive tracking continues automatically.'}

    def recall_inspect(self):
        receipt = self.run_worker(['recall-inspect'])
        if receipt['scope'].get('raw_status') != 'Archived':
            raise RuntimeError('The bucket must be archived before a recall.')
        return receipt

    def recall_submit(self, request):
        job_id = request['job_id']
        with self.db() as db:
            intent = self.query(db, 'SELECT * FROM archive_requests WHERE job_id=?', (job_id,)).fetchone()
            if (not intent or intent['operation'] != 'recall' or intent['phase'] != 'queued'
                    or intent['prepare_job_id'] != job_id or json.loads(intent['request']) != request):
                raise RuntimeError('Recall confirmation missing, modified or already used.')
        if not request.get('acknowledge_retrieval_cost'):
            raise RuntimeError('Confirm the recall charges for the entire bucket.')
        receipt = self.recall_inspect()
        if request.get('confirm_bucket') != receipt['scope']['bucket']:
            raise RuntimeError('Confirm the recall of the displayed bucket.')
        self.phase(job_id, 'submitting')
        value = self.run_worker(['request-bucket-restore', '--confirm-bucket', request['confirm_bucket'],
                                 '--acknowledge-retrieval-cost', '--expected-receipt', receipt['digest']])
        if value.get('status') != 'request_accepted' or value.get('bucket') != request['confirm_bucket']:
            raise RuntimeError('OVH recall acknowledgement not confirmed.')
        self.phase(job_id, 'accepted')
        return {'status': 'request_accepted', 'bucket': request['confirm_bucket'],
                'message': 'Recall accepted by OVH. The bucket will be readable as soon as tracking shows “Recalled”.'}

    def refresh(self, recover_queued=True):
        observed = self.run_worker(['bucket-status'])
        with self.db() as db:
            rows = self.query(db, "SELECT job_id,phase,operation,request FROM archive_requests WHERE phase IN ('queued','submitting','uncertain','accepted','archived','restored')").fetchall()
            for row in rows:
                if self.context:
                    owner = db.execute('SELECT vtl_id,destination_id FROM archive_requests WHERE job_id=?', (row['job_id'],)).fetchone()
                    if owner['vtl_id'] != self.context['vtl_id'] or owner['destination_id'] != self.context['destination_id']:
                        continue
                request = json.loads(row['request'])
                if request['confirm_bucket'] != observed.get('bucket'):
                    continue
                phase = row['phase']
                raw = observed.get('status')
                if phase == 'queued':
                    if not recover_queued:
                        continue
                    phase = 'failed'  # Only explicit recovery after confirming no live jobs.
                elif phase not in ('archived','restored'):
                    if row['operation'] == 'recall':
                        if raw == 'Restored': phase = 'restored'
                        elif raw == 'Restoring': phase = 'accepted'
                        elif phase in ('submitting','uncertain'): phase = 'uncertain'
                    else:
                        if raw in ('Archived','Restoring','Restored'): phase = 'archived'
                        elif raw == 'Archiving': phase = 'accepted'
                        elif phase in ('submitting','uncertain'): phase = 'uncertain'
                db.execute('UPDATE archive_requests SET phase=?,observed_status=?,observed_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP WHERE job_id=?', (phase,raw,row['job_id']))
                if self.context and phase == 'failed':
                    db.execute("UPDATE bucket_rotations SET phase='failed',updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase IN ('reserved','ready')", (row['job_id'],))
        if self.context and observed.get('status') in ('Archiving','Archived','Restoring','Restored'):
            with self.db() as db:
                rotations = db.execute("SELECT r.job_id FROM bucket_rotations r JOIN archive_requests a ON a.job_id=r.job_id WHERE r.source_destination_id=? AND r.phase='ready' AND a.phase IN ('accepted','archived')", (self.context['destination_id'],)).fetchall()
            for rotation in rotations:
                self.switch_bucket(rotation['job_id'])
        return {'status':'observed','bucket':observed.get('bucket'),'raw_status':observed.get('status'),
                'message':'OVH status observed. No tiering request resent.'}

    def observe(self):
        with self.operation_lock().open('a+') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return {'status':'busy'}
            maintenance.revalidate(self.controller.config)
            with self.db() as db:
                modern = db.execute("SELECT 1 FROM sqlite_master WHERE name='storage_destinations'").fetchone()
                if not modern:
                    self.context = None
                    return self.refresh(recover_queued=False)
                rows = self.query(db, '''SELECT j.* FROM jobs j JOIN archive_requests a ON a.job_id=j.id
                                     WHERE (a.phase IN ('queued','submitting','uncertain','accepted')
                                     OR EXISTS(SELECT 1 FROM bucket_rotations r WHERE r.job_id=j.id AND r.phase='ready'))''', alias='j.').fetchall()
            results = []
            failure = None
            for row in rows:
                try:
                    with self.db() as db:
                        self.select_context(db, row)
                    results.append(self.refresh(recover_queued=False))
                except Exception as error:
                    failure = error
            if failure:
                raise failure
            return {'status':'observed','buckets':results}


def main():
    os.umask(0o077)
    action = sys.argv[1] if len(sys.argv) == 2 else ''
    if action not in ('submit', 'refresh', 'recall-submit', 'observe'):
        raise RuntimeError('Unknown action.')
    config = maintenance.load_config()
    instance = Archive(config, Path(config.get('state_root', str(maintenance.ROOT))))
    if action == 'observe':
        return instance.observe()
    request = json.loads(sys.stdin.buffer.read(8192))
    return instance.execute(action, request)


if __name__ == '__main__':
    try:
        print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'error': str(error)}))
        sys.exit(1)
