#!/usr/bin/env python3
"""mhVTL replacement under the maintenance lock, with durable, reversible renames.

The API supplies catalogue job IDs, never arbitrary source/target paths. The
root-owned configuration and the verified restore receipt define those paths.
"""
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import sys
import time
import recovery_install

helper = Path(__file__).with_name('cold-vtl-maintenance')
if not helper.exists():
    helper = Path(__file__).with_name('maintenance-controller.py')
loader = importlib.machinery.SourceFileLoader('maintenance', str(helper))
spec = importlib.util.spec_from_loader(loader.name, loader)
maintenance = importlib.util.module_from_spec(spec)
loader.exec_module(maintenance)


def sync_dir(path):
    fd = os.open(path, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def move(source, target):
    if target.exists() or target.is_symlink():
        raise RuntimeError('Switch-over destination already present; recovery required.')
    source.rename(target)
    sync_dir(source.parent)
    sync_dir(target.parent)


def directory(path):
    if path.is_symlink() or not path.is_dir() or path.resolve() != path.absolute():
        raise RuntimeError('Folder missing or containing a symbolic link: ' + str(path))


def inventory(path):
    """This adapter supports flat mhVTL media, never links or special files."""
    directory(path)
    result = []
    for file in sorted(path.iterdir()):
        info = file.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or file.name.startswith('.'):
            raise RuntimeError('Incomplete cartridge, link or unsupported mhVTL format.')
        digest = hashlib.sha256()
        with file.open('rb') as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(chunk)
        result.append({'path': file.name, 'size_bytes': info.st_size, 'checksum_sha256': digest.hexdigest()})
    if not {'mam', 'data.0', 'indx.0', 'meta.0', 'mhvtl_data'}.issubset({f['path'] for f in result}):
        raise RuntimeError('The required files of the mhVTL cartridge are missing.')
    return result


class Reintegration:
    def __init__(self, config, root=maintenance.ROOT, controller=None):
        self.root = root
        self.config = config
        self.vtl_id = config.get('vtl_id')
        self.media = Path(config.get('media_root', '/opt/mhvtl'))
        self.restores = Path(config.get('restore_root', str(root / 'restores')))
        self.database = Path(config.get('database', str(root / 'catalog.db')))
        self.journals = root / 'reintegrations'
        self.pending = root / 'reintegration-pending.json'
        self.controller = controller or maintenance.Controller(config, root)
        self.controller.reintegrating = True
        self.journals.mkdir(exist_ok=True, mode=0o700)

    def db(self):
        connection = sqlite3.connect(self.database, timeout=15)
        connection.row_factory = sqlite3.Row
        return connection

    def query(self, db, sql, args=()):
        return db.execute(sql+(' AND vtl_id=?' if self.vtl_id else ''),tuple(args)+((self.vtl_id,) if self.vtl_id else ()))

    def update_generation(self,connection,row,metadata):
        sql='UPDATE remote_generations SET metadata=? WHERE barcode=? AND generation=?'
        args=(json.dumps(metadata),row['barcode'],row['generation'])
        if 'destination_id' in row.keys():
            sql+=' AND destination_id=?';args+=(row['destination_id'],)
        self.query(connection,sql,args)

    def status(self):
        values = [json.loads(p.read_text()) for p in self.journals.glob('*.json')]
        values.sort(key=lambda v: v['order'], reverse=True)
        return {'configured': True, 'pending': json.loads(self.pending.read_text()) if self.pending.exists() else None, 'operations': values[:30]}

    def save(self, journal, phase=None):
        if phase:
            journal['phase'] = phase
        journal['updated_at'] = int(time.time())
        maintenance.atomic_json(self.journals / (journal['id'] + '.json'), journal)

    def job(self, connection, job_id):
        row = self.query(connection, 'SELECT * FROM jobs WHERE id=?', (job_id,)).fetchone()
        if row is None:
            raise RuntimeError('Job not found.')
        return dict(row)

    def no_other_jobs(self, connection, job_id):
        self.controller.no_storage()
        if self.query(connection, "SELECT COUNT(*) FROM jobs WHERE status IN ('queued','running') AND id<>?", (job_id,)).fetchone()[0]:
            raise RuntimeError('Wait for all VTL jobs to finish.')
        self.controller.no_workers()

    def hold(self, journal):
        controller = self.controller
        actor = journal.get('rollback_actor', journal['actor'])
        controller.expected_barcode = journal['barcode']
        state = controller.read()
        if state['phase'] == 'online':
            if set(controller.services().values()) != {'active'}:
                raise RuntimeError('Reintegration requires all four services to be active before maintenance.')
            controller.enter(actor)
        elif state['phase'] == 'maintenance':
            if set(state.get('original', {}).values()) != {'active'}:
                raise RuntimeError('Maintenance must allow a complete resumption of the services.')
            controller.check()
        else:
            # Recover an interrupted service transition without starting a potentially
            # absent/replaced tape. Only the matching, still-held PBS lease permits it.
            remote = controller.remote('status')
            if not remote.get('locked') or not state.get('lease') or remote.get('lease') != state['lease']:
                _, _, backup, current = self.paths(journal)
                if not remote.get('locked') and not controller.block.exists() and not backup.exists() and (current.is_dir() or journal.get('new_media')) and journal['phase'] == 'preparing':
                    # Acquisition was refused before any local mutation (e.g. PBS
                    # busy). Recover the failed maintenance intent and retry normally.
                    controller.leave(actor)
                    controller.enter(actor)
                    return
                raise RuntimeError('Maintenance interrupted without a confirmed lease; recover the lock before reintegration.' if controller.veeam() else 'Maintenance interrupted without a confirmed PBS lease; recover the lock before reintegration.')
            maintenance.atomic_json(controller.block, {'lease': state['lease']})
            for unit in reversed(self.controller.units):
                controller.run(['systemctl', 'stop', unit])
            controller.stopped()
            state['phase'], state['message'] = 'maintenance', None
            controller.save(state)
            controller.check()

    def begin(self, request):
        from cartridge_deletion import pending
        if pending(self.root):
            raise RuntimeError("Complete interrupted local deletion before reintegration.")
        job_id = request['job_id']
        if not re.fullmatch(r'[0-9a-f-]{36}', job_id):
            raise RuntimeError('Invalid job identifier.')
        with self.db() as connection:
            self.no_other_jobs(connection, job_id)
            job = self.job(connection, job_id)
            if job['kind'] != 'reintegrate' or job['status'] not in ('queued', 'running'):
                raise RuntimeError('Invalid reintegration job.')
            restored = self.job(connection, request['restore_job_id'])
            receipt = json.loads(restored['message'] or '{}')
            barcode = restored['subject']
            if not re.fullmatch(r'[A-Z0-9-]{6,16}', barcode) or job['subject'] != barcode:
                raise RuntimeError('Invalid cartridge identity.')
            if restored['kind'] != 'restore' or restored['status'] != 'completed' or receipt.get('status') != 'restored' or receipt.get('barcode') != barcode or receipt.get('generation') != restored['generation'] or not receipt.get('files'):
                raise RuntimeError('Run the restore again to obtain a complete integrity receipt.')
            remote_sql='SELECT metadata FROM remote_generations WHERE barcode=? AND generation=?'
            remote_args=(barcode,restored['generation'])
            if 'destination_id' in restored:
                if not restored['destination_id']:raise RuntimeError('Restore destination missing.')
                remote_sql+=' AND destination_id=?';remote_args+= (restored['destination_id'],)
            remote = self.query(connection,remote_sql,remote_args).fetchone()
            if not remote or json.loads(remote[0])['manifest_sha256'] != receipt.get('manifest_sha256'):
                raise RuntimeError('The manifest of this restore no longer matches the catalogue.')
            directory(self.media)
            directory(self.restores)
            if self.media in self.restores.parents or self.restores in self.media.parents or self.media == self.restores:
                raise RuntimeError('The media and restore roots must be separate.')
            source = Path(restored['destination'])
            if source.parent != self.restores or inventory(source) != sorted(receipt['files'], key=lambda f: f['path']):
                raise RuntimeError('The restored folder has changed since it was verified; reintegration refused.')
            current = self.media / barcode
            if request.get('replace') is not True:
                raise RuntimeError('Confirm the replacement of the local cartridge separately.')
            directory(current)
            inventory(current)
            original = dict(self.query(connection, 'SELECT * FROM cartridges WHERE barcode=?', (barcode,)).fetchone())
            generations = [dict(r) for r in self.query(connection, 'SELECT * FROM remote_generations WHERE barcode=?', (barcode,))]
        if self.pending.exists():
            raise RuntimeError('A reintegration requires recovery.')
        # A newer integration invalidates older automatic rollback entries. Backups
        # themselves remain untouched and are never garbage-collected here.
        journal = {'vtl_id':self.vtl_id,'id': job_id, 'barcode': barcode, 'generation': restored['generation'], 'restore_job_id': restored['id'], 'destination_id':restored.get('destination_id'), 'source': str(source), 'actor': request['actor'], 'created_at': int(time.time()), 'order': time.time_ns(), 'original': original, 'generations': generations, 'receipt': receipt, 'phase': 'preparing'}
        self.save(journal)
        maintenance.atomic_json(self.pending, {'id': job_id})
        return journal

    def paths(self, journal):
        if journal.get('vtl_id')!=self.vtl_id:raise RuntimeError('Reintegration journal belongs to another VTL.')
        folder = self.media / '.cold-vtl-reintegration' / journal['id']
        return folder, folder / 'replacement', folder / 'previous', self.media / journal['barcode']

    def install(self, journal):
        self.paths(journal)
        maintenance.mark_data(self.root,self.vtl_id)
        self.hold(journal)
        recovery_install.freeze_layout(self, journal)
        folder, staged, backup, current = self.paths(journal)
        folder.parent.mkdir(exist_ok=True, mode=0o700)
        directory(folder.parent)
        folder.mkdir(mode=0o700)
        sync_dir(folder.parent)
        self.save(journal, 'copying')
        if not journal.get('new_media'):
            journal['original_files'] = inventory(current)
            for file in current.iterdir():
                with file.open('rb') as stream:
                    os.fsync(stream.fileno())
            sync_dir(current)
        elif current.exists() or current.is_symlink():
            raise RuntimeError('A cartridge with the same barcode appeared; installation refused.')
        self.save(journal)
        required = sum(f['size_bytes'] for f in journal['receipt']['files'])
        if shutil.disk_usage(folder).free < required + 16 * 1024 * 1024:
            raise RuntimeError('Insufficient space to keep the previous cartridge and install the restore.')
        # Validate source immediately before and staged files immediately after copying.
        expected = sorted(journal['receipt']['files'], key=lambda f: f['path'])
        if inventory(Path(journal['source'])) != expected:
            raise RuntimeError('The restore was modified before the copy.')
        shutil.copytree(journal['source'], staged, symlinks=True)
        if inventory(staged) != expected:
            raise RuntimeError('SHA-256 check failed before installation.')
        owner = (self.media if journal.get('new_media') else current).stat()
        os.chown(staged, owner.st_uid, owner.st_gid)
        os.chmod(staged, stat.S_IMODE(owner.st_mode))
        for file in staged.iterdir():
            os.chown(file, owner.st_uid, owner.st_gid)
            os.chmod(file, 0o600)
            with file.open('rb') as stream:
                os.fsync(stream.fileno())
        sync_dir(staged)
        self.controller.check()
        self.save(journal, 'replacing')
        if not journal.get('new_media'):
            move(current, backup)
            if inventory(backup) != journal['original_files']:
                raise RuntimeError('The retained previous copy does not match the SHA-256 check.')
        move(staged, current)
        recovery_install.layout(self, journal)
        self.save(journal, 'installed')
        with self.db() as connection:
            recovery_install.catalogue(self, connection, journal, required)
            for row in self.query(connection, 'SELECT * FROM remote_generations WHERE barcode=?', (journal['barcode'],)).fetchall():
                metadata = json.loads(row['metadata'])
                metadata['local_match'] = metadata.get('manifest_sha256') == journal['receipt']['manifest_sha256']
                self.update_generation(connection,row,metadata)
        self.save(journal, 'resuming')
        self.controller.leave(journal['actor'])
        journal['backup'] = str(backup) if backup.exists() else None
        self.save(journal, 'completed')
        self.pending.unlink()
        sync_dir(self.root)
        return journal

    def rollback(self, request):
        operation_id = request['operation_id']
        if not re.fullmatch(r'[0-9a-f-]{36}', operation_id):
            raise RuntimeError('Invalid reintegration identifier.')
        journal = json.loads((self.journals / (operation_id + '.json')).read_text())
        self.paths(journal)
        pending = json.loads(self.pending.read_text()) if self.pending.exists() else None
        if pending and pending['id'] != operation_id:
            raise RuntimeError('Recover the interrupted reintegration first.')
        with self.db() as connection:
            self.no_other_jobs(connection, request['job_id'])
            job = self.job(connection, request['job_id'])
            if job['kind'] != 'rollback' or job['subject'] != journal['barcode'] or job['status'] not in ('queued', 'running'):
                raise RuntimeError('Invalid rollback job.')
        same_media = [json.loads(p.read_text()) for p in self.journals.glob('*.json')]
        if any(v['barcode'] == journal['barcode'] and v['order'] > journal['order'] and v['phase'] != 'rolled_back' for v in same_media):
            raise RuntimeError('A more recent reintegration exists for this cartridge.')
        if journal['phase'] == 'rolled_back' and pending:
            if self.controller.read()['phase'] != 'online' or set(self.controller.services().values()) != {'active'}:
                raise RuntimeError('Check that the services have resumed before completing the rollback.')
            self.pending.unlink()
            sync_dir(self.root)
            return journal
        if journal['phase'] == 'rolled_back':
            raise RuntimeError('The previous cartridge has already been restored.')
        maintenance.atomic_json(self.pending, {'id': operation_id})
        journal['rollback_actor'] = request['actor']
        self.save(journal)
        self.hold(journal)
        folder, staged, backup, current = self.paths(journal)
        retired = folder / 'replaced-on-rollback'
        self.save(journal, 'rolling_back')
        if journal.get('new_media'):
            recovery_install.layout(self, journal, rollback=True, prepare_only=True)
            if current.exists():
                if inventory(current) != sorted(journal['receipt']['files'], key=lambda f:f['path']):
                    raise RuntimeError('The installed cartridge has changed; automatic removal refused.')
                move(current, retired)
            recovery_install.layout(self, journal, rollback=True)
            with self.db() as connection:
                old = journal.get('original')
                if old:
                    self.query(connection, 'UPDATE cartridges SET generation=?,bytes_written=?,state=?,updated_at=CURRENT_TIMESTAMP WHERE barcode=?', (old['generation'],old['bytes_written'],old['state'],journal['barcode']))
                row = self.query(connection,'SELECT 1 FROM cartridges WHERE barcode=?',(journal['barcode'],)).fetchone()
                if row:
                    connection.execute('INSERT OR IGNORE INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,?,?)', (self.vtl_id or '00000000-0000-4000-8000-000000000001',journal['barcode'],journal['id']))
                recovery_install.restore_settings(self, connection, journal)
                for row in self.query(connection, 'SELECT * FROM remote_generations WHERE barcode=?', (journal['barcode'],)).fetchall():
                    metadata = json.loads(row['metadata'])
                    metadata['local_match'] = False
                    self.update_generation(connection, row, metadata)
            self.controller.expected_barcode = None
        elif backup.exists():
            directory(backup)
            if inventory(backup) != journal.get('original_files'):
                raise RuntimeError('The previous copy has changed; automatic rollback refused.')
            if current.exists():
                directory(current)
                move(current, retired)
            move(backup, current)
        if not journal.get('new_media'):
            directory(current)
        if journal.get('original_files') and inventory(current) != journal['original_files']:
            raise RuntimeError('The previous cartridge does not match the journal; keep the services stopped.')
        # If a crash preceded the first rename, current is still the original. If
        # it followed the rollback rename, previous is absent and current restored.
        if journal.get('original_files'):
            with self.db() as connection:
                old = journal['original']
                self.query(connection, 'UPDATE cartridges SET generation=?,bytes_written=?,state=?,updated_at=CURRENT_TIMESTAMP WHERE barcode=?', (old['generation'], old['bytes_written'], old['state'], journal['barcode']))
                recovery_install.restore_settings(self, connection, journal)
                old_generations = {(row.get('destination_id'),row['generation']): json.loads(row['metadata']) for row in journal['generations']}
                for row in self.query(connection, 'SELECT * FROM remote_generations WHERE barcode=?', (journal['barcode'],)).fetchall():
                    metadata = json.loads(row['metadata'])
                    previous = old_generations.get((dict(row).get('destination_id'),row['generation']), {})
                    metadata['local_match'] = bool(previous.get('local_match') and previous.get('manifest_sha256') == metadata.get('manifest_sha256'))
                    self.update_generation(connection,row,metadata)
        self.controller.leave(request['actor'])
        journal['retained_replacement'] = str(retired) if retired.exists() else None
        self.save(journal, 'rolled_back')
        self.pending.unlink()
        sync_dir(self.root)
        return journal


def main():
    os.umask(0o077)
    action = sys.argv[1] if len(sys.argv) == 2 else ''
    config=maintenance.load_config()
    instance = Reintegration(config,Path(config.get('state_root',str(maintenance.ROOT))))
    if action == 'status':
        return instance.status()
    if action not in ('reintegrate', 'rollback', 'recover', 'check'):
        raise RuntimeError('Unsupported action')
    request = json.loads(sys.stdin.buffer.read(8192))
    if action == 'check':
        return recovery_install.check(instance, request)
    with (instance.root / ('operation.lock' if instance.vtl_id else 'maintenance-controller.lock')).open('a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        maintenance.revalidate(config)
        if action == 'rollback':
            return instance.rollback(request)
        journal = (recovery_install.begin(instance, request, inventory, directory, maintenance.atomic_json)
                   if action == 'recover' else instance.begin(request))
        return instance.install(journal)


if __name__ == '__main__':
    try:
        print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'error': str(error)}))
        sys.exit(1)
