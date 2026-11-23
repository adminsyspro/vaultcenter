#!/usr/bin/env python3
"""Fixed lab adapter. Persists intent before transitions; failure never silently resumes tape I/O."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time
import uuid
from vtl_runtime import command_timeout

ROOT = Path('/var/lib/cold-vtl')
CONFIG = Path('/etc/cold-vtl/maintenance.json')
UNITS = ['mhvtl.target', 'vtllibrary@10.service', 'vtltape@11.service', 'tgt.service']


def storage_available(root=ROOT, service=False):
    vtl_id=None
    if root.parent.name=='vtls' and (root.parent.parent/'storage-isolated').exists():
        vtl_id=root.name;root=root.parent.parent
    if not (root / 'storage-managed').exists():
        return
    spec = importlib.util.spec_from_file_location('vaultcenter_storage_check', Path(__file__).with_name('storage-check.py'))
    if spec is None or spec.loader is None:
        raise RuntimeError('The storage check is missing.')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.check(root, os.environ.get('COLD_VTL_STORAGE_SOCKET', module.SOCKET), service,vtl_id=vtl_id)


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as file:
        json.dump(value, file)
        file.flush()
        os.fsync(file.fileno())
    temporary.replace(path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Controller:
    def __init__(self, config, root=ROOT):
        self.config, self.root = config, root
        self.units = config.get('units',UNITS)
        self.vtl_id = config.get('vtl_id')
        self.target_unit = self.units[-1] if self.vtl_id else 'tgt.service'
        self.path = root / 'maintenance-state.json'
        self.block = root / 'maintenance-block'
        self.reintegrating = False
        self.expected_barcode = None

    def read(self):
        if not self.path.exists():
            return {'phase': 'online', 'lease': None, 'original': {}, 'message': None}
        return json.loads(self.path.read_text())

    def save(self, state):
        atomic_json(self.path, state)

    def run(self, args):
        result = subprocess.run(args, capture_output=True, text=True, timeout=command_timeout(args), check=False)
        if result.returncode:
            raise RuntimeError('Service command failed: ' + ' '.join(args[:3]))
        return result.stdout

    def veeam(self):
        return self.config.get('client') == 'veeam'

    def remote(self, action, lease=None):
        if self.config.get('client') == 'veeam':
            from client_lease import LocalLease
            try:
                return LocalLease(self.root, self.vtl_id).handle(action, lease, self.expected_barcode if action == 'verify' else None)
            except ValueError as error:
                raise RuntimeError(str(error)) from None
        args = ['ssh', '-F', '/dev/null', '-i', self.config['key_file'], '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=4', '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + self.config['known_hosts'], '-o', 'IdentitiesOnly=yes', '-o', 'ServerAliveInterval=5', '-o', 'ServerAliveCountMax=2', '--', self.config['pbs'], action]
        if self.vtl_id:args[-1:] = ['--vtl',self.vtl_id,action]
        if lease:
            args.append(lease)
        if action == 'verify' and self.expected_barcode:
            args.append(self.expected_barcode)
        # pbs, identity and host key paths come exclusively from root-owned configuration.
        result = subprocess.run(args, capture_output=True, text=True, timeout=40, check=False)
        try:
            value = json.loads(result.stdout)
        except ValueError:
            raise RuntimeError('PBS unreachable: the maintenance lock cannot be confirmed.')
        if self.vtl_id and value.get('vtl_id')!=self.vtl_id:raise RuntimeError('PBS response belongs to another VTL.')
        if result.returncode or not value.get('ok'):
            raise RuntimeError(value.get('error', 'PBS maintenance guard refused the request'))
        return value

    def services(self):
        if self.vtl_id:
            from vtl_runtime import Registry
            return Registry(self.root.parent.parent).services(self.vtl_id)
        states = {}
        for unit in self.units:
            values = self.run(['systemctl', 'show', '--property=LoadState,ActiveState', unit])
            values = dict(line.split('=', 1) for line in values.splitlines() if '=' in line)
            if values.get('LoadState') != 'loaded':
                raise RuntimeError('Missing service: ' + unit)
            states[unit] = values.get('ActiveState')
        return states

    def no_workers(self):
        if self.vtl_id:
            from vtl_runtime import Registry
            return Registry(self.root.parent.parent).no_workers(self.vtl_id)
        # Linux comm truncates this executable to 15 bytes. Any surviving worker
        # blocks service changes, including after catalogue recovery on API restart.
        result = subprocess.run(['pgrep', '-x', 'cold-vtl-worker'], capture_output=True, timeout=5)
        if result.returncode != 1:
            raise RuntimeError('A VTL worker is still active; wait for it to finish before changing maintenance.')

    def stopped(self):
        states = self.services()
        if any(value != 'inactive' for value in states.values()):
            raise RuntimeError('Not all VTL services are stopped.')
        if self.vtl_id:return
        for name in ['vtltape', 'vtllibrary', 'tgtd']:
            result = subprocess.run(['pgrep', '-x', name], capture_output=True, timeout=5)
            if result.returncode != 1:
                raise RuntimeError('A data plane process is still running: ' + name)

    def status(self):
        state = self.read()
        response = {**state, 'configured': True, 'blocked': self.block.exists(), 'pbs_locked': False, 'ready': False, 'client': self.config.get('client', 'pbs')}
        try:
            response['services'] = self.services()
            if (self.root / 'reintegration-pending.json').exists() and not self.reintegrating:
                response['message'] = 'A reintegration needs attention or a return to the previous cartridge.'
                response['reintegration_pending'] = True
                return response
            if state['phase'] in ['entering', 'leaving']:
                return response
            remote = self.remote('status')
            response['pbs_locked'] = bool(remote.get('locked'))
            if state['phase'] == 'maintenance':
                self.stopped()
                if not self.block.exists() or remote.get('lease') != state['lease'] or not remote.get('locked'):
                    raise RuntimeError('The maintenance lock must be verified before any operation.')
                response['ready'] = True
        except Exception as error:
            response['message'] = str(error)
        return response

    def enter(self, actor, confirm_idle=False):
        if self.config.get('client') == 'veeam' and confirm_idle is not True:
            raise RuntimeError('Confirm that no Veeam job is using this library before maintenance.')
        self.no_reintegration()
        state = self.read()
        if state['phase'] == 'maintenance':
            return self.check()
        if state['phase'] != 'online' or self.block.exists():
            raise RuntimeError('Maintenance interrupted: resume services to recover.')
        self.no_workers()
        original = self.services()
        if any(value not in ['active', 'inactive'] for value in original.values()):
            raise RuntimeError('A service has failed or is transitioning; correct its status before maintenance.')
        # Target dependencies would otherwise start services that were originally inactive.
        if len(set(original.values())) != 1:
            raise RuntimeError('Services are in a mixed state: start or stop them together before maintenance.')
        state = {'phase': 'entering', 'lease': str(uuid.uuid4()), 'original': original, 'actor': actor, 'since': int(time.time()), 'message': None}
        self.save(state)
        try:
            remote = self.remote('acquire', state['lease'])
            if not remote.get('locked') or remote.get('lease') != state['lease']:
                raise RuntimeError('Maintenance lease not confirmed for this VTL.' if self.veeam() else 'PBS did not confirm the requested lease')
            # Installed systemd conditions make this durable across daemon/appliance restarts.
            atomic_json(self.block, {'lease': state['lease']})
            for unit in reversed(self.units):
                self.run(['systemctl', 'stop', unit])
            self.stopped()
            state['phase'] = 'maintenance'
            self.save(state)
            return self.check()
        except Exception as error:
            state['phase'] = 'failed'
            state['message'] = str(error)
            self.save(state)
            raise

    def check_lease(self):
        state = self.read()
        phases=('maintenance','entering','leaving','failed') if self.vtl_id else ('maintenance',)
        if state.get('phase') not in phases or not self.block.exists():
            raise RuntimeError('Verified maintenance required.' if self.veeam() else 'Verified PBS maintenance is required.')
        block = json.loads(self.block.read_text())
        remote = self.remote('status')
        if not state.get('lease') or block.get('lease') != state['lease'] or remote.get('lease') != state['lease'] or not remote.get('locked'):
            raise RuntimeError('The maintenance lease does not match this maintenance.' if self.veeam() else 'The PBS lease does not match this maintenance.')
        return {'ready': True, 'pbs_locked': True, 'blocked': True, 'lease': state['lease']}

    def check(self):
        storage_available(self.root)
        status = self.status()
        if not status['ready']:
            raise RuntimeError(status.get('message') or 'Enter maintenance before this operation.')
        return status

    def verify_after_start(self, lease):
        # iscsid reconnects asynchronously after a longer maintenance window.
        # Keep the PBS lease while waiting for the existing changer to reappear.
        deadline = time.monotonic() + 30
        while True:
            try:
                return self.remote('verify', lease)
            except RuntimeError as error:
                transient = 'error opening' in str(error) and any(
                    text in str(error) for text in ('No such device or address', 'No such file or directory'))
                if not transient or time.monotonic() >= deadline:
                    raise
                time.sleep(1)

    def leave(self, actor):
        storage_available(self.root)
        self.no_reintegration()
        self.no_workers()
        state = self.read()
        if state['phase'] == 'online' and not self.block.exists():
            return self.status()
        # Verify remote ownership BEFORE restoring any services. If acquisition never
        # succeeded, an empty guard is enough only when the local block was never created.
        remote = self.remote('status')
        if remote.get('lease') not in [None, state['lease']]:
            raise RuntimeError('Another maintenance lease exists; resume refused.' if self.veeam() else 'Another PBS lock exists; resume refused.')
        if self.block.exists() and not remote.get('locked'):
            # Can recover after a guard reset while VTL stays disconnected. No automatic force.
            raise RuntimeError('Maintenance lock lost: administrator action required, VTL kept stopped.' if self.veeam() else 'PBS lock lost: administrator action required, VTL kept stopped.')
        blocked = self.block.exists()
        state['phase'], state['message'] = 'leaving', None
        self.save(state)
        try:
            self.block.unlink(missing_ok=True)
            (self.root / 'storage-services-held').unlink(missing_ok=True)
            (self.root / 'storage-bootstrap').unlink(missing_ok=True)
            for unit in self.units:
                if state['original'].get(unit) == 'active':
                    self.run(['systemctl', 'start', unit])
            if self.services() != state['original']:
                raise RuntimeError('The services did not return to their original state.')
            if remote.get('locked') and state['original'].get(self.target_unit) == 'active':
                self.verify_after_start(state['lease'])
            self.remote('release', state['lease'])
            state = {'phase': 'online', 'lease': None, 'original': {}, 'message': None, 'actor': actor, 'since': int(time.time())}
            self.save(state)
            return self.status()
        except Exception as error:
            if blocked:
                atomic_json(self.block, {'lease': state['lease']})
                for unit in reversed(self.units):
                    try:
                        self.run(['systemctl', 'stop', unit])
                    except Exception:
                        pass
            state['phase'], state['message'] = 'failed', str(error)
            self.save(state)
            raise

    def no_storage(self):
        database = Path(self.config.get('database', str(self.root / 'catalog.db')))
        if not database.exists():
            return
        with sqlite3.connect(database) as db:
            if self.vtl_id:
                pending=db.execute("SELECT COUNT(*) FROM archive_requests WHERE vtl_id=? AND phase IN ('queued','submitting','uncertain')",(self.vtl_id,)).fetchone()[0]
                active=db.execute("SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND (kind LIKE 'archive_%' OR kind LIKE 'recall_%') AND status IN ('queued','running')",(self.vtl_id,)).fetchone()[0]
                if pending or active:raise RuntimeError('A storage operation for this VTL is still active.')
                return
            if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='archive_requests'").fetchone():
                pending = db.execute("SELECT COUNT(*) FROM archive_requests WHERE phase IN ('queued','submitting','uncertain')").fetchone()[0]
                active = db.execute("SELECT COUNT(*) FROM jobs WHERE (kind LIKE 'archive_%' OR kind LIKE 'recall_%') AND status IN ('queued','running')").fetchone()[0]
                if pending or active:
                    raise RuntimeError('OVH operation in progress or uncertain result: check OVH Storage tracking.')

    def no_reintegration(self):
        from cartridge_deletion import pending
        if pending(self.root):
            raise RuntimeError('Local deletion interrupted: resume it from the library before starting services.')
        self.no_storage()
        from cartridge_batches import pending_batch
        if pending_batch(self.root):
            raise RuntimeError('Cartridge batch interrupted: complete its creation before resuming services.')
        if (self.root / 'cartridge-creation-pending.json').exists():
            raise RuntimeError('Cartridge creation interrupted: retry with the same settings before resuming services.')
        if (self.root / 'reintegration-pending.json').exists() and not self.reintegrating:
            raise RuntimeError('Reintegration in progress or interrupted: use its return to the previous cartridge.')


def mark_data(root, vtl_id):
    if vtl_id:
        # Caller holds this VTL's operation flock; the host reads this same marker.
        # No socket callback while holding the shared lock.
        atomic_json(root/'data-locked',{'vtl_id':vtl_id,'data_locked':True})


def load_config():
    if (Path('/var/lib/cold-vtl')/'storage-isolated').exists():
        from vtl_runtime import resolve_runtime, Registry
        m=resolve_runtime(os.environ.get('COLD_VTL_VTL_ID'),expected_revision=int(os.environ['COLD_VTL_RUNTIME_REVISION']) if 'COLD_VTL_RUNTIME_REVISION' in os.environ else None)
        return {**m,**m.get('pbs',{}),'client':m.get('client','pbs'),'vtl_id':m['vtl_id'],'runtime':m,'units':Registry().units(m),
                'database':'/var/lib/cold-vtl/catalog.db','mhvtl_config':m['config_root']}
    return json.loads(CONFIG.read_text())


def revalidate(config):
    if config.get('vtl_id') and config.get('runtime',{}).get('revision') is not None:
        from vtl_runtime import resolve_runtime
        m=resolve_runtime(config['vtl_id'],config['runtime']['revision'],Path(config['database']).parent)
        if m!=config['runtime']:raise RuntimeError('Hardware configuration changed during the request.')


def main():
    os.umask(0o077)
    action = sys.argv[1] if len(sys.argv) == 2 else ''
    if action not in ['status', 'enter', 'leave', 'check', 'check-lease']:
        raise RuntimeError('Unsupported maintenance action')
    config=load_config();root=Path(config.get('state_root',str(ROOT)))
    controller = Controller(config,root)
    if action == 'check-lease':
        return controller.check_lease()
    if action in ['status', 'check']:
        return getattr(controller, action)()
    # This flock also serializes a later request after an API timeout or daemon restart.
    with (root / ('operation.lock' if config.get('vtl_id') else 'maintenance-controller.lock')).open('a+') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('A maintenance transition is already in progress.')
        revalidate(config)
        request = json.loads(sys.stdin.buffer.read(4096) or b'{}')
        actor = str(request.get('actor', 'local operator'))[:128]
        if action == 'enter':
            return controller.enter(actor, request.get('confirm_idle') is True)
        return getattr(controller, action)(actor)


if __name__ == '__main__':
    try:
        print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'error': str(error)}))
        sys.exit(1)
