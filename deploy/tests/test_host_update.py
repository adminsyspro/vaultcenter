"""Tests of deploy/host/update.py.

Every external command goes through update.run, replaced here by FakeHost,
which simulates the container, the maintenance helper, systemd, DKMS and
modprobe on a fake root (VAULTCENTER_INSTALL_ROOT). Nothing real is run.
"""
import ast
from contextlib import closing, redirect_stderr, redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import uuid

REPO = Path(__file__).parents[2]
SPEC = importlib.util.spec_from_file_location('vaultcenter_update', REPO / 'deploy/host/update.py')
update = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update)

OLD_IMAGE = 'ghcr.io/adminsyspro/vaultcenter@sha256:' + 'a' * 64
NEW_IMAGE = 'ghcr.io/adminsyspro/vaultcenter@sha256:' + 'b' * 64
OLD_VERSION, NEW_VERSION = '0.1.0', '0.2.0'
VTL = '11111111-1111-4111-8111-111111111111'
OTHER_VTL = '22222222-2222-4222-8222-222222222222'
CONTAINER = 'vaultcenter-appliance-1'
STATE_DIR = '/var/lib/docker/volumes/vaultcenter_state/_data'
KERNEL = '6.1.0-28-amd64'
OTHER_KERNEL = '6.1.0-31-amd64'
HELPER = '/usr/local/libexec/cold-vtl-maintenance'
OLD_ARCHIVE, NEW_ARCHIVE = b'old mhvtl sources', b'new mhvtl sources'
CONTROLLERS = ['storage_controller.py', 'storage_paths.py', 'vtl_state.py', 'vtl_storage.py', 'storage_mounts.py']
UNITS = ['vaultcenter-storage-host.service', 'vaultcenter-storage-mounts.service']
MODULE_UNIT = 'vaultcenter-mhvtl.service'
RUNTIME = b'# new vtl_runtime.py\n'


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def dkms_version(data):
    return 'vc-' + sha256(data)[:12]


def units_listing(argv):
    """The listing of the VTL units: the check that ends step 8."""
    return argv[:2] == ['docker', 'exec'] and '--type=service' in argv


class FailingStream(io.StringIO):
    """A stderr or stdout whose reader went away: write and flush raise
    `error` whenever `broken(number of writes so far)` is true."""

    def __init__(self, broken, error=None):
        super().__init__()
        self.broken, self.writes = broken, 0
        self.error = error or BrokenPipeError(32, 'Broken pipe')

    def write(self, text):
        self.writes += 1
        if self.broken(self.writes):
            raise self.error
        return super().write(text)

    def flush(self):
        if self.broken(self.writes):
            raise self.error


class FakeHost:
    """Stands in for update.run: answers like the real host, records every call."""

    def __init__(self, root):
        self.root = root
        self.calls = []
        self.rules = []
        self.running = True
        self.image = OLD_IMAGE
        self.versions = {OLD_IMAGE: OLD_VERSION, NEW_IMAGE: NEW_VERSION}
        self.unhealthy = set()
        self.health_version = None
        self.workers = ''
        self.guards = {VTL: {'phase': 'online', 'client': 'pbs'}}
        self.status_fails = set()
        self.dkms_added = {dkms_version(OLD_ARCHIVE)}
        self.dkms_installed = {dkms_version(OLD_ARCHIVE): {KERNEL}}
        self.loaded = dkms_version(OLD_ARCHIVE)
        self.module_busy = False
        self.dkms_declines = False          # `dkms install` succeeds without installing anything
        self.remove_deletes_module = False  # removing a version deletes the installed module file
        self.reinstall_restores = True      # `dkms install --force` puts the module file back
        self.module_file_missing = False
        self.on_up = None
        self.on_call = None
        self.inputs = []
        self.raise_on = []
        self.violations = []
        self.listing = None
        self.health_sequence = []           # what the next `docker inspect` calls answer, before the real state
        self.refuses_catalog = False
        self.catalog = root / STATE_DIR.lstrip('/') / 'catalog.db'
        self.catalog_at_start = None
        self.catalog_checked = False
        (root / 'sys/module/mhvtl').mkdir(parents=True)

    def catalog_state(self):
        return (self.catalog.read_bytes(), sorted(p.name for p in self.catalog.parent.glob('catalog.db-*')))

    def catalog_migrated(self):
        with closing(sqlite3.connect(f'file:{self.catalog}?mode=ro', uri=True)) as db:
            return db.execute("SELECT 1 FROM sqlite_master WHERE name='migration_0028'").fetchone() is not None

    @staticmethod
    def modifying(argv):
        return (argv[:2] == ['docker', 'compose'] or argv[0] in ('modprobe', 'systemctl')
                or argv[0] == 'dkms' and argv[1] != 'status')

    def record_write(self, path):
        """Called for every file update.py replaces (atomic_write)."""
        path = Path(path)
        self.calls.append(['#write', str(path.relative_to(self.root))])
        self.inputs.append(None)
        if path == self.catalog and self.running:
            self.violations.append('catalog.db replaced while the container runs')

    def raise_when(self, predicate, error):
        """Raise `error` from the next call matching `predicate` (once)."""
        self.raise_on.append([predicate, error])

    def fail(self, predicate, times=1):
        """Make the next `times` calls matching `predicate` fail (None: always)."""
        self.rules.append([predicate, times])

    def __call__(self, argv, *, input=None, check=True, timeout=update.COMMAND_TIMEOUT):
        argv = [str(item) for item in argv]
        assert timeout is not None and 0 < timeout <= 3600, argv
        self.calls.append(argv)
        self.inputs.append(input)
        blocked = {signal.SIGINT, signal.SIGTERM, signal.SIGHUP} & signal.pthread_sigmask(signal.SIG_BLOCK, [])
        if blocked and not units_listing(argv):
            # Only the last check of step 8 runs with signals held.
            self.violations.append('external command while signals are blocked: %r' % argv)
        if self.catalog_at_start is None:
            self.catalog_at_start = self.catalog_state()
        if self.modifying(argv) and not self.catalog_checked:
            self.catalog_checked = True
            if self.catalog_state() != self.catalog_at_start:
                self.violations.append('catalog.db or its side files changed before the first modification')
        if self.on_call:
            self.on_call(argv)
        for rule in self.raise_on:
            if rule[0] is not None and rule[0](argv):
                error, rule[0] = rule[1], None
                raise error
        code, out = 0, ''
        for rule in self.rules:
            if rule[1] != 0 and rule[0](argv):
                if rule[1] is not None:
                    rule[1] -= 1
                code, out = 1, ''
                break
        else:
            code, out = self.handle(argv, input)
        result = subprocess.CompletedProcess(argv, code, out, 'simulated error' if code else '')
        if check and code:
            raise update.CommandError(argv, result)
        return result

    def handle(self, argv, input):
        if argv == ['uname', '-r']:
            return 0, KERNEL + '\n'
        if argv[0] == 'curl':
            assert argv[-1] == 'https://127.0.0.1:8443/health'
            if not self.running or self.image in self.unhealthy or self.refuses_catalog:
                return 7, ''
            version = self.health_version or self.versions[self.image]
            return 0, json.dumps({'status': 'ok', 'service': 'cold-vtld', 'version': version})
        if argv[:2] == ['docker', 'compose']:
            return self.compose(argv)
        if argv[:2] == ['docker', 'inspect']:
            assert argv[-1] == CONTAINER
            if self.health_sequence:
                return 0, self.health_sequence.pop(0) + '\n'
            healthy = self.running and self.image not in self.unhealthy and not self.refuses_catalog
            return 0, ('healthy' if healthy else 'unhealthy') + '\n'
        if argv[:2] == ['docker', 'exec']:
            return self.exec(argv, input)
        if argv[0] == 'systemctl':
            assert argv[1:] in (['daemon-reload'], ['restart', 'vaultcenter-storage-host'],
                                ['enable', MODULE_UNIT]), argv
            return 0, ''
        if argv[0] == 'modinfo':
            assert argv[1:] == ['-b', str(self.root), '-k', KERNEL, 'mhvtl'], argv
            installed = any(KERNEL in kernels for kernels in self.dkms_installed.values())
            return (0, 'filename: mhvtl.ko\n') if installed and not self.module_file_missing else (1, '')
        if argv[0] == 'dkms':
            return self.dkms(argv)
        if argv[0] == 'modprobe':
            return self.modprobe(argv)
        if argv[0] == 'bash' and argv[1].endswith('host/prepare-dkms-source.sh'):
            assert argv[3] == str(self.root), argv
            return 0, dkms_version(Path(argv[2]).read_bytes()) + '\n'
        raise AssertionError('unexpected command: %r' % argv)

    def compose(self, argv):
        assert argv[2] == '-f' and argv[3] == str(self.root / 'etc/vaultcenter-host/compose.json'), argv
        if argv[-2:] == ['stop', 'appliance']:
            self.running = False
            return 0, ''
        if argv[-4:] == ['up', '-d', '--force-recreate', 'appliance']:
            self.image = json.loads(Path(argv[3]).read_text())['services']['appliance']['image']
            self.running = True
            # The new version migrates the catalogue at first start; the old
            # one refuses a migrated catalogue, as the real cold-vtld does.
            self.refuses_catalog = self.image == OLD_IMAGE and self.catalog_migrated()
            if self.refuses_catalog:
                self.violations.append('old image started on a migrated catalogue')
            if self.image == NEW_IMAGE:
                with closing(sqlite3.connect(self.catalog)) as db:
                    db.execute('CREATE TABLE IF NOT EXISTS migration_0028 (id INTEGER)')
                    db.commit()
            if self.on_up:
                self.on_up(self.image)
            return 0, ''
        raise AssertionError('unexpected compose call: %r' % argv)

    def exec(self, argv, input):
        index, env = 2, {}
        while argv[index].startswith('-'):
            if argv[index] == '-e':
                key, value = argv[index + 1].split('=', 1)
                env[key] = value
                index += 2
            else:
                index += 1
        assert argv[index] == CONTAINER, argv
        command = argv[index + 1:]
        if not self.running:
            return 1, ''
        if command[:2] == ['systemctl', 'list-units']:
            if 'vaultcenter-worker-*' in command:
                return 0, self.workers
            if self.listing is not None and self.image == NEW_IMAGE:
                return 0, self.listing()
            lines = []
            for vtl, guard in sorted(self.guards.items()):
                state = 'inactive' if guard['phase'] == 'maintenance' else 'active'
                for unit in (f'vaultcenter-library-{vtl}.service', f'vaultcenter-drive-{vtl}-1.service',
                             f'vaultcenter-target-{vtl}.service'):
                    lines.append(f'{unit} loaded {state} {"running" if state == "active" else "dead"} VTL')
            return 0, '\n'.join(lines) + '\n'
        if command[:1] == [HELPER]:
            vtl = env.get('COLD_VTL_VTL_ID')
            action = command[1]
            if action == 'status' and vtl in self.status_fails:
                return 1, json.dumps({'error': 'Hardware manifest not confirmed by the catalogue.'})
            guard = self.guards[vtl]
            if action == 'status':
                return 0, json.dumps({'phase': guard['phase'], 'configured': True, 'client': guard['client'],
                                      'lease': guard.get('lease'), 'actor': guard.get('actor'),
                                      'ready': guard['phase'] == 'maintenance', 'services': {}})
            request = json.loads(input or '{}')
            if action == 'enter':
                if guard['client'] == 'veeam' and request.get('confirm_idle') is not True:
                    return 1, json.dumps({'error': 'Confirm that no Veeam job is using this library.'})
                self.operator_enter(vtl, request.get('actor'))
            elif action == 'leave':
                guard.update(phase='online', lease=None)
            else:
                raise AssertionError(argv)
            answer = {'phase': guard['phase'], 'configured': True}
            if not guard.get('no_lease'):
                answer['lease'] = guard.get('lease')
            return 0, json.dumps(answer)
        raise AssertionError('unexpected exec: %r' % argv)

    def dkms(self, argv):
        action = argv[1]
        version = argv[argv.index('-v') + 1]
        kernel = argv[argv.index('-k') + 1] if '-k' in argv else None
        if action == 'status':
            if version not in self.dkms_added:
                return 0, ''
            if kernel is None:
                kernels = sorted(self.dkms_installed.get(version, set()))
                return 0, ''.join(f'mhvtl/{version}, {name}, x86_64: installed\n' for name in kernels) or (
                    f'mhvtl/{version}: added\n')
            state = 'installed' if kernel in self.dkms_installed.get(version, set()) else 'added'
            return 0, f'mhvtl/{version}, {kernel}, x86_64: {state}\n'
        if action == 'add':
            self.dkms_added.add(version)
            return 0, ''
        if action == 'build':
            assert version in self.dkms_added
            return 0, ''
        if action == 'install':
            if self.dkms_declines:
                return 0, ''
            if '--force' in argv and self.reinstall_restores:
                self.module_file_missing = False
            for kernels in self.dkms_installed.values():
                kernels.discard(kernel)
            self.dkms_installed.setdefault(version, set()).add(kernel)
            self.dkms_installed = {v: k for v, k in self.dkms_installed.items() if k}
            return 0, ''
        if action == 'remove':
            assert '--all' in argv
            self.dkms_added.discard(version)
            self.dkms_installed.pop(version, None)
            if self.remove_deletes_module:
                self.module_file_missing = True
            return 0, ''
        raise AssertionError(argv)

    def modprobe(self, argv):
        assert not any(flag in argv for flag in ('-f', '--force', '--force-modversion', '--force-vermagic')), argv
        if argv == ['modprobe', '-r', 'mhvtl']:
            if self.running:
                self.violations.append('modprobe -r while the container runs')
            if self.module_busy:
                return 1, 'modprobe: FATAL: Module mhvtl is in use.'
            self.loaded = None
            (self.root / 'sys/module/mhvtl').rmdir()
            return 0, ''
        if argv == ['modprobe', 'mhvtl', 'opts=0']:
            installed = [v for v, kernels in self.dkms_installed.items() if KERNEL in kernels]
            if self.loaded or not installed:
                return 1, ''
            self.loaded = installed[0]
            (self.root / 'sys/module/mhvtl').mkdir()
            return 0, ''
        raise AssertionError(argv)

    def operator_enter(self, vtl, actor='admin'):
        """A maintenance session starts: a new lease, as maintenance-controller.py does."""
        self.guards[vtl].update(phase='maintenance', lease=str(uuid.uuid4()), actor=actor)

    def module_state(self):
        return (sorted(self.dkms_added), {v: sorted(k) for v, k in self.dkms_installed.items()}, self.loaded,
                (self.root / 'sys/module/mhvtl').exists())

    def index(self, predicate, start=0):
        for position, argv in enumerate(self.calls[start:], start):
            if predicate(argv):
                return position
        raise AssertionError('no matching call')

    def maintenance_calls(self, action):
        return [argv for argv in self.calls if argv[:2] == ['docker', 'exec'] and argv[-2:] == [HELPER, action]]


class UpdateTestCase(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)
        self.root = self.tmp / 'root'
        self.root.mkdir()
        self.state = self.root / STATE_DIR.lstrip('/')
        self.make_installation()
        self.payload = self.make_payload(OLD_ARCHIVE)
        self.host = FakeHost(self.root)
        patches = [
            mock.patch.dict(os.environ, {'VAULTCENTER_INSTALL_ROOT': str(self.root),
                                         'VAULTCENTER_UPDATE_HEALTH_INTERVAL': '0',
                                         'VAULTCENTER_UPDATE_HEALTH_TIMEOUT': '0'}),
            mock.patch.object(update, 'run', self.host),
            mock.patch.object(update, 'atomic_write', self.spy_write),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        host = self.host
        self.addCleanup(lambda: self.assertEqual(host.violations, []))

    REAL_ATOMIC_WRITE = staticmethod(update.atomic_write)

    fail_write = None
    on_write = None

    def spy_write(self, path, data, *args, **kwargs):
        self.host.record_write(path)
        if self.on_write:
            self.on_write(path, data)
        if self.fail_write and self.fail_write(path, data):
            raise OSError(28, 'No space left on device')
        return self.REAL_ATOMIC_WRITE(path, data, *args, **kwargs)

    # ----------------------------------------------------------- fixtures
    def make_installation(self):
        etc = self.root / 'etc/vaultcenter-host'
        (etc / 'config/cold-vtl').mkdir(parents=True)
        (etc / 'compose.json').write_text(json.dumps(
            {'name': 'vaultcenter', 'services': {'appliance': {'image': OLD_IMAGE}}}, indent=2) + '\n')
        (etc / 'compose.json').chmod(0o640)
        (etc / 'storage.json').write_text(json.dumps({
            'compose_file': '/etc/vaultcenter-host/compose.json', 'project': 'vaultcenter',
            'container': CONTAINER, 'state_dir': STATE_DIR, 'isolated': True,
            'host_state': '/var/lib/vaultcenter-host'}, indent=2) + '\n')
        (etc / 'config/cold-vtl/auth.env').write_text('SECRET=1\n')
        self.state.mkdir(parents=True)
        (self.state / 'storage-isolated').write_text('{}\n')
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.executescript('''
                CREATE TABLE jobs (id TEXT PRIMARY KEY, kind TEXT, subject TEXT, status TEXT);
                CREATE TABLE vtls (id TEXT PRIMARY KEY, lifecycle TEXT);
                CREATE TABLE vtl_provisioning (vtl_id TEXT PRIMARY KEY, phase TEXT);
                INSERT INTO jobs VALUES ('j1', 'export', 'A00001', 'succeeded');
            ''')
            db.execute("INSERT INTO vtls VALUES (?, 'ready')", (VTL,))
            db.execute("INSERT INTO vtl_provisioning VALUES (?, 'active')", (VTL,))
            db.execute("INSERT INTO vtls VALUES ('00000000-0000-4000-8000-000000000001', 'draft')")
            db.commit()
        self.write_receipt(self.state / 'isolation-qualified.json', sha256(RUNTIME))
        storage = self.root / 'var/lib/vaultcenter-host/vtls' / VTL / 'storage.json'
        storage.parent.mkdir(parents=True)
        storage.write_text(json.dumps({'mode': 'managed', 'vtl_id': VTL, 'phase': 'ready'}))
        install = self.root / 'var/lib/vaultcenter-install'
        install.mkdir(parents=True)
        (install / 'mhvtl.sha256').write_text(sha256(OLD_ARCHIVE) + '\n')
        libexec = self.root / 'usr/local/libexec/vaultcenter-host'
        libexec.mkdir(parents=True)
        for name in CONTROLLERS:
            (libexec / name).write_text(f'# old {name}\n')
        units = self.root / 'etc/systemd/system'
        units.mkdir(parents=True)
        for name in UNITS:
            (units / name).write_text(f'# old {name}\n')

    def make_payload(self, archive, receipt=None, runtime=RUNTIME):
        payload = self.tmp / 'payload' / NEW_VERSION
        if payload.exists():
            shutil.rmtree(payload)
        host = payload / 'host'
        host.mkdir(parents=True)
        for name in CONTROLLERS + UNITS + [MODULE_UNIT]:
            (host / name).write_text(f'# new {name}\n')
        shutil.copy(REPO / 'deploy/install/prepare-dkms-source.sh', host / 'prepare-dkms-source.sh')
        shutil.copy(REPO / 'deploy/install/dkms.conf', host / 'dkms.conf')
        if runtime is not None:
            (host / 'vtl_runtime.py').write_bytes(runtime)
        if receipt is not None:
            self.write_receipt(host / 'isolation-qualified.json', receipt)
        (payload / 'mhvtl-source.tar.gz').write_bytes(archive)
        return payload

    @staticmethod
    def write_receipt(path, digest):
        path.write_text(json.dumps({'runtime_sha256': digest, 'isolation_verified': True}) + '\n')

    # ------------------------------------------------------------ helpers
    def update(self, *extra, image=NEW_IMAGE, version=NEW_VERSION, argv=None, stderr=None):
        out, err = io.StringIO(), stderr if stderr is not None else io.StringIO()
        if argv is None:
            argv = ['--image', image, '--version', version, '--payload', str(self.payload), *extra]
        umask = os.umask(0o022)
        os.umask(umask)
        try:
            with redirect_stdout(out), redirect_stderr(err):
                code = update.main(argv)
        finally:
            os.umask(umask)
        lines = out.getvalue().splitlines()
        self.assertTrue(lines, err.getvalue())
        result = json.loads(lines[-1])
        self.stderr = err.getvalue()
        return code, result

    def compose_image(self):
        compose = json.loads((self.root / 'etc/vaultcenter-host/compose.json').read_text())
        return compose['services']['appliance']['image']

    def catalog_dump(self):
        with closing(sqlite3.connect(f'file:{self.state / "catalog.db"}?mode=ro', uri=True)) as db:
            return list(db.iterdump())

    def tree(self, *relative):
        files = {}
        for item in relative:
            base = self.root / item
            paths = [base] if base.is_file() else sorted(base.rglob('*'))
            for path in paths:
                if path.is_file() and '__pycache__' not in path.parts:
                    files[str(path.relative_to(self.root))] = (path.read_bytes(), path.stat().st_mode & 0o7777)
        return files

    def snapshot(self):
        return {
            'files': self.tree('etc/vaultcenter-host', 'usr/local/libexec/vaultcenter-host', 'etc/systemd/system',
                               'var/lib/vaultcenter-install/mhvtl.sha256',
                               STATE_DIR.lstrip('/') + '/isolation-qualified.json'),
            'catalog': self.catalog_dump(),
            'side_files': sorted(p.name for p in self.state.glob('catalog.db-*')),
            'module': self.host.module_state(),
        }

    def backups(self):
        directory = self.root / 'var/lib/vaultcenter-host/backups'
        return sorted(p.name for p in directory.iterdir()) if directory.exists() else []

    def modifying_calls(self):
        """Calls that change the host or the appliance (never made by a refused run)."""
        return [argv for argv in self.host.calls if argv[0] in ('dkms', 'modprobe', 'systemctl')
                and argv[1] != 'status'
                or argv[:2] == ['docker', 'compose']
                or argv[-1] in ('enter', 'leave')]


class VersionTests(UpdateTestCase):
    def test_same_digest_is_unchanged(self):
        before = self.snapshot()
        code, result = self.update(image=OLD_IMAGE)
        self.assertEqual(code, 0)
        self.assertEqual(result['result'], 'unchanged')
        self.assertIs(result['confirmable'], False)
        self.assertEqual(self.host.calls, [])
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(self.backups(), [])

    def test_lower_version_is_refused(self):
        self.host.versions[OLD_IMAGE] = '0.3.0'
        before = self.snapshot()
        code, result = self.update()
        self.assertEqual(code, 3)
        self.assertEqual(result['result'], 'refused')
        self.assertEqual(result['from'], '0.3.0')
        self.assertEqual(result['to'], NEW_VERSION)
        self.assertTrue(any('0.3.0' in reason for reason in result['reasons']), result)
        self.assertEqual(self.modifying_calls(), [])
        self.assertEqual(self.snapshot(), before)

    def test_versions_compare_as_integers(self):
        self.host.versions[OLD_IMAGE] = '0.9.0'
        self.host.versions[NEW_IMAGE] = '0.10.0'
        code, result = self.update(version='0.10.0')
        self.assertEqual((code, result['result']), (0, 'updated'), result)

    def test_versions_order_as_semver_does_with_pre_releases(self):
        ordered = ['1.1.9', '1.2.0-1', '1.2.0-2', '1.2.0-10', '1.2.0-alpha', '1.2.0-alpha.1', '1.2.0-alpha.beta',
                   '1.2.0-beta', '1.2.0-rc.1', '1.2.0-rc.2', '1.2.0-rc.10', '1.2.0', '1.2.1-rc.1', '1.2.1']
        keys = [update.parse_version(text) for text in ordered]
        self.assertEqual(sorted(keys), keys)
        self.assertEqual(len(set(keys)), len(keys))
        for text in ('1.2', '1.2.0-', '1.2.0-rc..1', '1.2.0+build', '1.2.0-rc_1', 'latest', '', 'v1.2.0', '1.2.0 '):
            with self.subTest(text):
                with self.assertRaises(ValueError):
                    update.parse_version(text)

    def test_pre_release_can_be_installed_and_is_lower_than_its_release(self):
        self.host.versions[NEW_IMAGE] = '0.2.0-rc.1'
        code, result = self.update(version='0.2.0-rc.1')
        self.assertEqual((code, result['result'], result['to']), (0, 'updated', '0.2.0-rc.1'), result)
        # The backup of a pre-release is pruned like any other.
        self.assertTrue(update.BACKUP_NAME.fullmatch('20261113-120000-0.2.0-rc.1'))
        self.assertFalse(update.BACKUP_NAME.fullmatch('20261113-120000-0.2.0-rc.1/..'))
        # From the release, its own pre-release is a downgrade.
        self.setUp()
        self.host.versions[OLD_IMAGE] = '0.2.0'
        self.host.versions[NEW_IMAGE] = '0.2.0-rc.1'
        code, result = self.update(version='0.2.0-rc.1')
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertTrue(any('older than the installed version' in reason for reason in result['reasons']), result)
        # And from a pre-release, its release is an update.
        self.setUp()
        self.host.versions[OLD_IMAGE] = '0.2.0-rc.1'
        self.host.versions[NEW_IMAGE] = '0.2.0'
        code, result = self.update(version='0.2.0')
        self.assertEqual((code, result['result'], result['from']), (0, 'updated', '0.2.0-rc.1'), result)
        self.assertRegex(self.backups()[0], r'^\d{8}-\d{6}-0\.2\.0-rc\.1$')

    def test_unpinned_image_is_refused(self):
        code, result = self.update(image='ghcr.io/adminsyspro/vaultcenter:0.2.0')
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.host.calls, [])

    def test_unreachable_api_is_refused(self):
        self.host.running = False
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.modifying_calls(), [])


class BlockerTests(UpdateTestCase):
    def set_job(self, status):
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute("INSERT INTO jobs VALUES (?, 'export', 'A00002', ?)", ('j-' + status, status))
            db.commit()

    def test_blockers(self):
        self.assertEqual(update.blockers(self.root), [])
        cases = {
            'queued': lambda: self.set_job('queued'),
            'running': lambda: self.set_job('running'),
            'worker': lambda: setattr(self.host, 'workers',
                                      f'vaultcenter-worker-{VTL}-0123abcd.service loaded active running Worker\n'),
            'storage': lambda: (self.root / 'var/lib/vaultcenter-host/vtls' / VTL / 'storage.json').write_text(
                json.dumps({'vtl_id': VTL, 'phase': 'applying'})),
        }
        for name, make in cases.items():
            with self.subTest(name):
                self.setUp()
                make()
                reasons = update.blockers(self.root)
                self.assertEqual(len(reasons), 1, reasons)
                code, result = self.update()
                self.assertEqual((code, result['result']), (3, 'refused'))
                self.assertEqual(result['reasons'], reasons)
                self.assertEqual(self.modifying_calls(), [])

    def test_unconfigured_storage_does_not_block_but_other_phases_do(self):
        storage = self.root / 'var/lib/vaultcenter-host/vtls' / OTHER_VTL / 'storage.json'
        storage.parent.mkdir(parents=True)
        storage.write_text(json.dumps({'vtl_id': OTHER_VTL, 'phase': 'unconfigured'}))
        self.assertEqual(update.blockers(self.root), [])
        for phase in ('applying', 'recovery_required', 'unavailable'):
            with self.subTest(phase):
                storage.write_text(json.dumps({'vtl_id': OTHER_VTL, 'phase': phase}))
                reasons = update.blockers(self.root)
                self.assertEqual(len(reasons), 1, reasons)
                self.assertIn(OTHER_VTL, reasons[0])
                self.assertIn(phase, reasons[0])

    def test_inactive_or_failed_worker_does_not_block(self):
        self.host.workers = f'vaultcenter-worker-{VTL}-0123abcd.service loaded failed failed Worker\n'
        self.assertEqual(update.blockers(self.root), [])

    def test_unverifiable_worker_state_blocks(self):
        self.host.fail(lambda argv: 'vaultcenter-worker-*' in argv, times=None)
        self.assertEqual(len(update.blockers(self.root)), 1)

    def test_catalog_is_read_read_only(self):
        catalog = self.state / 'catalog.db'
        before = catalog.read_bytes()
        catalog.chmod(0o400)
        self.addCleanup(catalog.chmod, 0o600)
        opened = []
        real_connect = sqlite3.connect

        def spy(database, *args, **kwargs):
            opened.append((str(database), kwargs.get('uri', False)))
            return real_connect(database, *args, **kwargs)

        with mock.patch.object(update.sqlite3, 'connect', spy):
            self.assertEqual(update.blockers(self.root), [])
            backup = update.backup(self.root, OLD_VERSION)
        on_catalog = [entry for entry in opened if str(catalog) in entry[0]]
        self.assertTrue(on_catalog)
        for database, uri in on_catalog:
            self.assertTrue(uri, database)
            self.assertIn('mode=ro', database)
        self.assertEqual(catalog.read_bytes(), before)
        self.assertEqual(sorted(p.name for p in self.state.glob('catalog.db*')), ['catalog.db'])
        self.assertTrue((backup / 'catalog.db').is_file())

    def test_wal_catalog_is_read_without_side_files(self):
        catalog = self.state / 'catalog.db'
        with closing(sqlite3.connect(catalog)) as db:
            db.execute('PRAGMA journal_mode=WAL')
            db.commit()
        self.assertFalse(catalog.with_name('catalog.db-wal').exists())
        self.assertEqual(update.blockers(self.root), [])
        update.backup(self.root, OLD_VERSION)
        self.assertEqual(sorted(p.name for p in self.state.glob('catalog.db*')), ['catalog.db'])
        # With the API running, the job sits in the WAL: it must be seen.
        writer = sqlite3.connect(catalog)
        self.addCleanup(writer.close)
        writer.execute("INSERT INTO jobs VALUES ('j9', 'export', 'A00009', 'queued')")
        writer.commit()
        self.assertEqual(len(update.blockers(self.root)), 1)


class MaintenanceTests(UpdateTestCase):
    def test_missing_space_refuses_before_maintenance(self):
        usage = shutil._ntuple_diskusage(10 ** 9, 10 ** 9 - 1024, 1024)
        with mock.patch.object(update.shutil, 'disk_usage', return_value=usage):
            code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertTrue(any('free space' in reason for reason in result['reasons']), result)
        self.assertEqual(self.modifying_calls(), [])
        self.assertEqual(self.backups(), [])

    def test_incomplete_backup_stops_and_leaves_maintenance(self):
        failures = {
            'tar': mock.patch.object(update, 'write_tar', side_effect=OSError('disque plein')),
            'integrity': mock.patch.object(update, 'catalog_integrity', return_value='*** page 3 corrupt'),
            'copy': mock.patch.object(update, 'copy_catalog', side_effect=sqlite3.DatabaseError('I/O')),
        }
        for name, patch in failures.items():
            with self.subTest(name):
                self.setUp()
                before = self.snapshot()
                with patch:
                    code, result = self.update()
                self.assertEqual((code, result['result']), (3, 'refused'))
                self.assertTrue(result['maintenance_left'])
                self.assertEqual(len(self.host.maintenance_calls('enter')), 1)
                self.assertEqual(len(self.host.maintenance_calls('leave')), 1)
                self.assertEqual(self.host.guards[VTL]['phase'], 'online')
                self.assertEqual(self.backups(), [])
                self.assertIsNone(result['backup'])
                self.assertFalse(any(argv[0] in ('dkms', 'modprobe') or argv[:2] == ['docker', 'compose']
                                     or argv[:2] == ['systemctl', 'restart'] for argv in self.host.calls))
                after = self.snapshot()
                self.assertEqual(after['files'], before['files'])
                self.assertEqual(after['catalog'], before['catalog'])

    def test_no_guard_requires_yes(self):
        self.host.status_fails.add(VTL)
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertTrue(any('guard' in reason for reason in result['reasons']), result)
        self.assertEqual(self.modifying_calls(), [])
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.maintenance_calls('enter'), [])
        self.assertEqual(self.host.maintenance_calls('leave'), [])

    def test_a_refusal_that_yes_alone_would_lift_is_confirmable(self):
        self.host.status_fails.add(VTL)
        code, result = self.update()
        self.assertEqual((code, result['result'], result['confirmable']), (3, 'refused', True), result)
        self.host.status_fails.clear()
        self.host.guards[VTL]['client'] = 'veeam'
        code, result = self.update()
        self.assertEqual((code, result['result'], result['confirmable']), (3, 'refused', True), result)

    def test_a_refusal_with_another_blocker_is_not_confirmable(self):
        self.host.guards[VTL]['phase'] = 'failed'
        code, result = self.update()
        self.assertEqual((code, result['result'], result['confirmable']), (3, 'refused', False), result)
        # A running job refuses before the maintenance plan is even made.
        self.setUp()
        self.host.status_fails.add(VTL)
        writer = sqlite3.connect(self.state / 'catalog.db')
        self.addCleanup(writer.close)
        writer.execute("INSERT INTO jobs VALUES ('j9', 'export', 'A00009', 'queued')")
        writer.commit()
        code, result = self.update()
        self.assertEqual((code, result['result'], result['confirmable']), (3, 'refused', False), result)

    def test_a_refusal_about_the_backup_is_not_confirmable(self):
        usage = shutil._ntuple_diskusage(10 ** 9, 10 ** 9 - 1024, 1024)
        with mock.patch.object(update.shutil, 'disk_usage', return_value=usage):
            code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertIn('backup', result['reasons'][0])
        self.assertIs(result['confirmable'], False)

    def test_a_refusal_is_never_confirmable_once_yes_was_given(self):
        self.host.guards[VTL]['phase'] = 'failed'
        code, result = self.update('--yes')
        self.assertEqual((code, result['result'], result['confirmable']), (3, 'refused', False), result)

    def test_an_updated_run_is_not_confirmable(self):
        code, result = self.update()
        self.assertEqual((result['result'], result['confirmable']), ('updated', False))

    def test_veeam_guard_requires_yes_and_confirms_idle(self):
        self.host.guards[VTL]['client'] = 'veeam'
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(len(result['reasons']), 1, result)
        reason = result['reasons'][0]
        for text in ('guard', 'VaultCenter cannot see', 'running Veeam job', '--yes asserts'):
            self.assertIn(text, reason)
        self.assertEqual(self.modifying_calls(), [])
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        enter = self.host.calls.index(self.host.maintenance_calls('enter')[0])
        self.assertEqual(json.loads(self.host.inputs[enter])['confirm_idle'], True)
        self.assertTrue(result['maintenance_left'])

    def test_enter_is_called_with_the_vtl_and_as_the_api_does(self):
        code, result = self.update()
        self.assertEqual(code, 0, result)
        enter = self.host.maintenance_calls('enter')[0]
        self.assertEqual(enter, ['docker', 'exec', '-i', '-e', f'COLD_VTL_VTL_ID={VTL}', CONTAINER, HELPER, 'enter'])
        request = json.loads(self.host.inputs[self.host.calls.index(enter)])
        self.assertEqual(request['confirm_idle'], False)
        self.assertTrue(request['actor'])

    def test_vtl_already_in_maintenance_is_neither_entered_nor_left(self):
        self.host.guards[VTL]['phase'] = 'maintenance'
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.maintenance_calls('enter'), [])
        self.assertEqual(self.host.maintenance_calls('leave'), [])
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')

    def test_interrupted_maintenance_transition_is_refused(self):
        self.host.guards[VTL]['phase'] = 'failed'
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.modifying_calls(), [])

    def test_failed_enter_leaves_the_vtls_already_entered(self):
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute("INSERT INTO vtls VALUES (?, 'ready')", (OTHER_VTL,))
            db.execute("INSERT INTO vtl_provisioning VALUES (?, 'active')", (OTHER_VTL,))
            db.commit()
        self.host.guards[OTHER_VTL] = {'phase': 'online', 'client': 'pbs'}
        self.host.fail(lambda argv: f'COLD_VTL_VTL_ID={OTHER_VTL}' in argv and argv[-1] == 'enter')
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertEqual(len(self.host.maintenance_calls('leave')), 1)
        self.assertEqual(self.backups(), [])

    def test_receipt_mismatch_without_shipped_receipt_is_refused(self):
        self.write_receipt(self.state / 'isolation-qualified.json', sha256(b'previous runtime'))
        self.assertFalse(update.receipt_ok(self.root, self.payload))
        # A shipped receipt for another runtime does not help.
        self.payload = self.make_payload(OLD_ARCHIVE, receipt=sha256(b'yet another runtime'))
        self.assertFalse(update.receipt_ok(self.root, self.payload))
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertTrue(any('receipt' in reason for reason in result['reasons']), result)
        self.assertEqual(self.modifying_calls(), [])

    def assert_receipt_warning(self, result):
        self.assertEqual(len(result['warnings']), 1, result)
        self.assertIn('isolation receipt', result['warnings'][0])
        self.assertIn('S3', result['warnings'][0])

    def test_unverified_receipt_proceeds_with_a_warning(self):
        (self.state / 'isolation-qualified.json').write_text(json.dumps({'runtime_sha256': sha256(b'other')}))
        self.assertTrue(update.receipt_ok(self.root, self.payload))
        self.assertTrue(update.receipt_warning(self.root, self.payload))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assert_receipt_warning(result)

    def test_missing_receipt_proceeds_with_a_warning(self):
        (self.state / 'isolation-qualified.json').unlink()
        self.assertTrue(update.receipt_ok(self.root, self.payload))
        self.assertTrue(update.receipt_warning(self.root, self.payload))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assert_receipt_warning(result)
        self.assertFalse((self.state / 'isolation-qualified.json').exists())

    def test_verified_matching_receipt_proceeds_without_warning(self):
        self.assertTrue(update.receipt_ok(self.root, self.payload))
        self.assertFalse(update.receipt_warning(self.root, self.payload))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(result['warnings'], [])

    def test_refusal_carries_an_empty_warning_list(self):
        code, result = self.update(image='ghcr.io/adminsyspro/vaultcenter:0.2.0')
        self.assertEqual(result['warnings'], [])

    def test_receipt_shipped_in_payload_is_installed(self):
        old_receipt = self.state / 'isolation-qualified.json'
        self.write_receipt(old_receipt, sha256(b'previous runtime'))
        self.payload = self.make_payload(OLD_ARCHIVE, receipt=sha256(RUNTIME))
        self.assertTrue(update.receipt_ok(self.root, self.payload))
        self.assertFalse(update.receipt_warning(self.root, self.payload))
        seen = []
        self.host.on_up = lambda image: seen.append(json.loads(old_receipt.read_text())['runtime_sha256'])
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(seen, [sha256(RUNTIME)])
        self.assertEqual(result['warnings'], [])

    def test_receipt_shipped_in_payload_is_installed_over_a_missing_one(self):
        receipt = self.state / 'isolation-qualified.json'
        receipt.unlink()
        self.payload = self.make_payload(OLD_ARCHIVE, receipt=sha256(RUNTIME))
        self.assertFalse(update.receipt_warning(self.root, self.payload))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(json.loads(receipt.read_text())['runtime_sha256'], sha256(RUNTIME))
        self.assertEqual(result['warnings'], [])

    def test_payload_without_runtime_is_refused(self):
        self.payload = self.make_payload(OLD_ARCHIVE, runtime=None)
        self.assertFalse(update.receipt_ok(self.root, self.payload))


class UpdateFlowTests(UpdateTestCase):
    def test_update_order(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        seen = {}
        libexec = self.root / 'usr/local/libexec/vaultcenter-host'

        def watch(argv):
            if argv[:2] == ['dkms', 'add']:
                seen['backup_at_module'] = self.backups()
            if argv == ['systemctl', 'restart', 'vaultcenter-storage-host']:
                seen['controller_at_restart'] = (libexec / 'storage_controller.py').read_text()
        self.host.on_call = watch
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(result['from'], OLD_VERSION)
        self.assertEqual(result['to'], NEW_VERSION)
        self.assertTrue(result['maintenance_left'])
        calls = self.host.calls
        enter = self.host.index(lambda a: a[-1] == 'enter')
        stop = self.host.index(lambda a: a[:2] == ['docker', 'compose'] and a[-2:] == ['stop', 'appliance'])
        add = self.host.index(lambda a: a[:2] == ['dkms', 'add'])
        build = self.host.index(lambda a: a[:2] == ['dkms', 'build'])
        install = self.host.index(lambda a: a[:2] == ['dkms', 'install'])
        unload = self.host.index(lambda a: a == ['modprobe', '-r', 'mhvtl'])
        load = self.host.index(lambda a: a == ['modprobe', 'mhvtl', 'opts=0'])
        reload = self.host.index(lambda a: a == ['systemctl', 'daemon-reload'])
        restart = self.host.index(lambda a: a == ['systemctl', 'restart', 'vaultcenter-storage-host'])
        up = self.host.index(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'])
        health = self.host.index(lambda a: a[:2] == ['docker', 'inspect'], up)
        version = self.host.index(lambda a: a[0] == 'curl', health)
        leave = self.host.index(lambda a: a[-1] == 'leave')
        self.assertEqual(sorted([enter, stop, add, build, install, unload, load, reload, restart, up, health,
                                 version, leave]),
                         [enter, stop, add, build, install, unload, load, reload, restart, up, health, version,
                          leave])
        self.assertEqual(len(seen['backup_at_module']), 1)
        self.assertEqual(seen['controller_at_restart'], '# new storage_controller.py\n')
        self.assertEqual(self.host.image, NEW_IMAGE)
        self.assertFalse((self.root / 'var/lib/vaultcenter-host/update-in-progress.json').exists())
        self.assertFalse((self.root / 'var/lib/vaultcenter-host/update-verified.json').exists())
        compose = self.root / 'etc/vaultcenter-host/compose.json'
        self.assertEqual(json.loads(compose.read_text())['services']['appliance']['image'], NEW_IMAGE)
        self.assertEqual(compose.stat().st_mode & 0o7777, 0o640)
        self.assertEqual((self.root / 'var/lib/vaultcenter-install/mhvtl.sha256').read_text().strip(),
                         sha256(NEW_ARCHIVE))
        # The previous module version stays until the update has succeeded, then goes.
        remove = self.host.index(lambda a: a[:2] == ['dkms', 'remove'])
        self.assertGreater(remove, version)
        self.assertIn(dkms_version(OLD_ARCHIVE), calls[remove])
        self.assertEqual(self.host.module_state()[0], [dkms_version(NEW_ARCHIVE)])
        self.assertEqual(self.host.loaded, dkms_version(NEW_ARCHIVE))
        for name in CONTROLLERS:
            self.assertEqual((libexec / name).read_text(), f'# new {name}\n')
        # Backup contents.
        backup = self.root / 'var/lib/vaultcenter-host/backups' / self.backups()[0]
        self.assertRegex(backup.name, r'^\d{8}-\d{6}-' + OLD_VERSION.replace('.', r'\.') + '$')
        self.assertEqual(result['backup'], '/' + str(backup.relative_to(self.root)))
        for name in ('catalog.db', 'etc-vaultcenter-host.tar', 'image', 'version', 'mhvtl.sha256', 'host-files.tar'):
            self.assertTrue((backup / name).is_file(), name)
        self.assertEqual((backup / 'image').read_text().strip(), OLD_IMAGE)
        self.assertEqual((backup / 'version').read_text().strip(), OLD_VERSION)
        self.assertEqual(backup.stat().st_mode & 0o777, 0o700)
        self.assertEqual((backup / 'etc-vaultcenter-host.tar').stat().st_mode & 0o077, 0)

    def test_unchanged_module_is_not_rebuilt_and_container_not_stopped_for_it(self):
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertFalse(any(argv[0] in ('dkms', 'modprobe', 'bash') for argv in self.host.calls))
        self.assertFalse(any(argv[-2:] == ['stop', 'appliance'] for argv in self.host.calls))
        self.assertEqual(self.host.module_state()[2], dkms_version(OLD_ARCHIVE))

    def test_busy_module_fails_without_force(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.host.module_busy = True
        before = self.snapshot()
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        for argv in self.host.calls:
            self.assertNotEqual(argv[0], 'rmmod')
            if argv[0] == 'modprobe':
                self.assertFalse({'-f', '--force'} & set(argv), argv)
        self.assertEqual(sum(argv == ['modprobe', '-r', 'mhvtl'] for argv in self.host.calls), 1)
        self.assertEqual(self.snapshot()['module'], before['module'])

    def test_previous_module_must_be_installed_to_change_it(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.host.dkms_installed = {}
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.modifying_calls(), [])

    def test_module_changed(self):
        self.assertFalse(update.module_changed(self.root, self.payload))
        self.assertTrue(update.module_changed(self.root, self.make_payload(NEW_ARCHIVE)))
        (self.root / 'var/lib/vaultcenter-install/mhvtl.sha256').unlink()
        self.assertTrue(update.module_changed(self.root, self.payload))


class ModuleRobustnessTests(UpdateTestCase):
    """The kernels that are installed but not booted, and what DKMS really did."""
    OLD, NEW = dkms_version(OLD_ARCHIVE), dkms_version(NEW_ARCHIVE)

    def setUp(self):
        super().setUp()
        self.payload = self.make_payload(NEW_ARCHIVE)

    def installs(self):
        return [(argv[argv.index('-v') + 1], argv[argv.index('-k') + 1])
                for argv in self.host.calls if argv[:2] == ['dkms', 'install']]

    def test_new_module_is_installed_for_every_kernel_that_had_the_previous_one(self):
        self.host.dkms_installed = {self.OLD: {KERNEL, OTHER_KERNEL}}
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.dkms_installed, {self.NEW: {KERNEL, OTHER_KERNEL}})
        self.assertEqual(self.installs(), [(self.NEW, KERNEL), (self.NEW, OTHER_KERNEL)])
        builds = [argv[argv.index('-k') + 1] for argv in self.host.calls if argv[:2] == ['dkms', 'build']]
        self.assertEqual(builds, [KERNEL, OTHER_KERNEL])
        remove = self.host.index(lambda a: a[:2] == ['dkms', 'remove'])
        last_install = max(i for i, a in enumerate(self.host.calls) if a[:2] == ['dkms', 'install'])
        self.assertLess(last_install, remove)
        self.assertEqual(result['warnings'], [])

    def test_a_kernel_without_the_previous_module_is_left_alone(self):
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.installs(), [(self.NEW, KERNEL)])

    def test_failure_for_a_kernel_that_is_not_running_is_a_warning(self):
        self.host.dkms_installed = {self.OLD: {KERNEL, OTHER_KERNEL}}
        self.host.fail(lambda a: a[:2] == ['dkms', 'build'] and OTHER_KERNEL in a)
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        warning = ' '.join(result['warnings'])
        self.assertIn(OTHER_KERNEL, warning)
        self.assertIn(f'dkms install --force -m mhvtl -v {self.NEW} -k {OTHER_KERNEL}', warning)
        self.assertEqual(self.host.loaded, self.NEW)

    def test_rollback_puts_the_previous_module_back_for_every_kernel(self):
        self.host.dkms_installed = {self.OLD: {KERNEL, OTHER_KERNEL}}
        before = self.host.module_state()
        self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'])
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual(self.host.module_state(), before)

    def test_install_that_dkms_did_not_perform_fails_the_step(self):
        self.host.dkms_declines = True
        before = self.snapshot()
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertTrue(any('kernel module' in reason and 'DKMS did not install' in reason
                            for reason in result['reasons']), result)
        # The module in place was never unloaded for a version that is not there.
        self.assertNotIn(['modprobe', '-r', 'mhvtl'], self.host.calls)
        self.host.dkms_declines = False
        self.assertEqual(self.snapshot()['catalog'], before['catalog'])

    def test_installs_replace_a_module_of_the_same_module_version(self):
        self.update()
        for argv in self.host.calls:
            if argv[:2] == ['dkms', 'install']:
                self.assertIn('--force', argv)

    def test_module_deleted_with_the_previous_version_is_reinstalled(self):
        self.host.remove_deletes_module = True
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        remove = self.host.index(lambda a: a[:2] == ['dkms', 'remove'])
        again = self.host.index(lambda a: a[:2] == ['dkms', 'install'], remove)
        self.assertEqual(self.host.calls[again][:3], ['dkms', 'install', '--force'])
        self.assertIn(self.NEW, self.host.calls[again])
        self.assertFalse(self.host.module_file_missing)
        self.assertTrue(any('reinstalled' in warning for warning in result['warnings']), result)
        self.assertFalse(any('reboot' in reason for reason in result['reasons']), result)

    def test_module_that_stays_missing_is_reported_with_the_command_to_run(self):
        self.host.remove_deletes_module = True
        self.host.reinstall_restores = False
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        reasons = ' '.join(result['reasons'])
        self.assertIn('next reboot', reasons)
        self.assertIn(f'dkms install --force -m mhvtl -v {self.NEW} -k {KERNEL}', reasons)
        self.assertIn(f'modinfo -k {KERNEL} mhvtl', reasons)
        # A verified update is reported, never rolled back.
        self.assertEqual(self.host.image, NEW_IMAGE)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')

    def test_module_in_place_is_checked_and_not_reinstalled(self):
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        remove = self.host.index(lambda a: a[:2] == ['dkms', 'remove'])
        self.assertEqual([a for a in self.host.calls[remove:] if a[:2] == ['dkms', 'install']], [])
        self.assertTrue([a for a in self.host.calls[remove:] if a[0] == 'modinfo'])
        self.assertEqual((result['reasons'], result['warnings']), ([], []))

    def test_finish_resumed_by_a_later_run_checks_the_module_too(self):
        self.host.raise_when(lambda a: a[-1] == 'leave', SystemExit(1))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.host.module_file_missing = True
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertFalse(self.host.module_file_missing)
        self.assertTrue(any('reinstalled' in warning for warning in result['warnings']), result)


class BootGuardUnitTests(UpdateTestCase):
    """vaultcenter-mhvtl.service follows the other host files."""
    UNIT = 'etc/systemd/system/' + MODULE_UNIT
    LINK = 'etc/systemd/system/multi-user.target.wants/' + MODULE_UNIT

    def enable_creates_the_link(self):
        def watch(argv):
            if argv == ['systemctl', 'enable', MODULE_UNIT]:
                link = self.root / self.LINK
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to('/etc/systemd/system/' + MODULE_UNIT)
        self.host.on_call = watch

    def test_update_installs_and_enables_the_unit(self):
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual((self.root / self.UNIT).read_text(), f'# new {MODULE_UNIT}\n')
        reload = self.host.index(lambda a: a == ['systemctl', 'daemon-reload'])
        enable = self.host.index(lambda a: a == ['systemctl', 'enable', MODULE_UNIT])
        restart = self.host.index(lambda a: a == ['systemctl', 'restart', 'vaultcenter-storage-host'])
        self.assertLess(reload, enable)
        self.assertLess(enable, restart)

    def test_payload_without_the_unit_is_refused(self):
        (self.payload / 'host' / MODULE_UNIT).unlink()
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertIn(MODULE_UNIT, result['reasons'][0])
        self.assertEqual(self.modifying_calls(), [])

    def test_rollback_removes_a_unit_the_appliance_did_not_have(self):
        self.enable_creates_the_link()
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertFalse((self.root / self.UNIT).exists())
        self.assertFalse((self.root / self.LINK).is_symlink())

    def test_rollback_restores_the_unit_the_appliance_had(self):
        (self.root / self.UNIT).write_text('# old unit\n')
        self.enable_creates_the_link()
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual((self.root / self.UNIT).read_text(), '# old unit\n')
        self.assertTrue((self.root / self.LINK).is_symlink())


class SeamTests(UpdateTestCase):
    """What install.sh reads in the JSON, and how the updater reads the API."""

    def no_active_vtl(self):
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute("UPDATE vtl_provisioning SET phase = 'draft'")
            db.commit()

    def test_health_is_never_read_through_a_proxy(self):
        with mock.patch.dict(os.environ, {'HTTPS_PROXY': 'http://proxy.example:3128'}):
            code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        probes = [argv for argv in self.host.calls if argv[0] == 'curl']
        self.assertTrue(probes)
        for argv in probes:
            self.assertEqual(argv[argv.index('--noproxy') + 1], '*', argv)

    def test_run_that_entered_no_vtl_leaves_none_in_maintenance(self):
        cases = {'no active VTL': self.no_active_vtl,
                 'already in maintenance': lambda: self.host.operator_enter(VTL)}
        for name, prepare in cases.items():
            with self.subTest(name):
                self.setUp()
                prepare()
                code, result = self.update()
                self.assertEqual((code, result['result']), (0, 'updated'), result)
                self.assertEqual(self.host.maintenance_calls('enter'), [])
                self.assertEqual(result['still_in_maintenance'], [])
                self.assertIs(result['maintenance_left'], True)

    def test_every_result_carries_the_list_and_the_matching_flag(self):
        runs = {'unchanged': lambda: self.update(image=OLD_IMAGE),
                'refused': lambda: self.update(version='0.0.1'),
                'updated': self.update}
        for name, run in runs.items():
            with self.subTest(name):
                self.setUp()
                code, result = run()
                self.assertEqual(result['result'], name, result)
                self.assertEqual(result['still_in_maintenance'], [])
                self.assertIs(result['maintenance_left'], True)

    def test_vtl_that_could_not_leave_is_listed_with_its_reason(self):
        self.host.fail(lambda a: a[-1] == 'leave', times=None)
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual([(item['vtl'], item['name']) for item in result['still_in_maintenance']],
                         [(VTL, f'VTL {VTL}')])
        self.assertIn('could not leave maintenance', result['still_in_maintenance'][0]['reason'])
        self.assertIs(result['maintenance_left'], False)
        # "VTL" keeps its capitals at the start of a sentence.
        self.assertTrue(any(reason.startswith(f'VTL {VTL} ') for reason in result['reasons']), result)

    def test_vtl_taken_over_is_listed_and_named_properly(self):
        def take_over(argv):
            if argv[0] == 'curl' and self.host.image == NEW_IMAGE:
                self.host.operator_enter(VTL)
        self.host.on_call = take_over
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual([item['vtl'] for item in result['still_in_maintenance']], [VTL])
        self.assertIn('someone else', result['still_in_maintenance'][0]['reason'])
        self.assertTrue(any(warning.startswith(f'VTL {VTL}: ') for warning in result['warnings']), result)
        self.assertFalse(any('Vtl' in text for text in result['warnings'] + result['reasons']), result)

    def test_rolled_back_run_lists_the_vtls_it_entered(self):
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual([item['vtl'] for item in result['still_in_maintenance']], [VTL])
        self.assertIn('operator’s decision', result['still_in_maintenance'][0]['reason'])
        self.assertIs(result['maintenance_left'], False)
        self.assertTrue(any(f'Still in maintenance: VTL {VTL}' in reason for reason in result['reasons']),
                        result)

    def test_rolled_back_run_that_entered_no_vtl_does_not_say_they_remain(self):
        self.host.operator_enter(VTL)
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual(result['still_in_maintenance'], [])
        self.assertIs(result['maintenance_left'], True)
        self.assertFalse(any('till in maintenance' in reason for reason in result['reasons']), result)
        self.assertTrue(any('No VTL' in reason for reason in result['reasons']), result)

    def test_failed_rollback_lists_the_vtls_it_entered(self):
        self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'], times=None)
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        self.assertEqual([item['vtl'] for item in result['still_in_maintenance']], [VTL])
        self.assertIs(result['maintenance_left'], False)

    def test_documentation_is_the_copy_shipped_beside_the_script(self):
        self.assertEqual(update.documentation(), 'docs/install.md')
        host = self.root / 'usr/local/share/vaultcenter/0.2.0/host'
        host.mkdir(parents=True)
        shutil.copy(REPO / 'deploy/host/update.py', host / 'update.py')
        spec = importlib.util.spec_from_file_location('shipped_update', host / 'update.py')
        shipped = importlib.util.module_from_spec(spec)
        with mock.patch.object(sys, 'dont_write_bytecode', True):
            spec.loader.exec_module(shipped)
        self.assertEqual(shipped.documentation(), 'docs/install.md')
        (host / 'install.md').write_text('# guide\n')
        self.assertEqual(shipped.documentation(), '/usr/local/share/vaultcenter/0.2.0/host/install.md')
        refusal = shipped.MarkerUnreadable(shipped.VERIFIED)
        self.assertIn('/usr/local/share/vaultcenter/0.2.0/host/install.md', refusal.reasons[0])
        self.assertNotIn('docs/install.md', refusal.reasons[0])


class HostFileListTests(unittest.TestCase):
    def test_updater_replaces_exactly_what_the_host_installer_installs(self):
        # Two lists of the same files in two scripts that cannot import each other.
        tree = ast.parse((REPO / 'deploy/host/install.py').read_text())
        loops = {node.target.id: [item.value for item in node.iter.elts]
                 for node in ast.walk(tree)
                 if isinstance(node, ast.For) and isinstance(node.target, ast.Name)
                 and isinstance(node.iter, ast.Tuple)}
        self.assertEqual(sorted(loops), ['key', 'marker', 'name', 'unit'])
        self.assertEqual(tuple(loops['name']), update.HOST_CONTROLLERS)
        self.assertEqual(tuple(loops['unit']), update.HOST_UNITS)
        self.assertEqual(list(update.HOST_CONTROLLERS), CONTROLLERS)
        self.assertEqual(list(update.HOST_UNITS), UNITS)


class RollbackTests(UpdateTestCase):
    def simulate_new_version(self, image):
        """The new version migrates the catalogue and leaves WAL side files."""
        if image != NEW_IMAGE:
            return
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute('CREATE TABLE IF NOT EXISTS migration_0028 (id INTEGER)')
            db.execute("UPDATE jobs SET status='failed'")
            db.commit()
        (self.state / 'catalog.db-wal').write_bytes(b'stale wal of the new version')
        (self.state / 'catalog.db-shm').write_bytes(b'stale shm')

    def failures(self):
        return {
            5: lambda: self.host.fail(lambda a: a[:2] == ['dkms', 'build']),
            6: lambda: self.host.fail(lambda a: a == ['systemctl', 'restart', 'vaultcenter-storage-host']),
            7: lambda: self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance']),
            8: lambda: self.host.unhealthy.add(NEW_IMAGE),
            '8-version': lambda: setattr(self.host, 'versions', {**self.host.versions, NEW_IMAGE: '0.1.9'}),
            '8-units': lambda: self.host.fail(
                lambda a: a[:2] == ['docker', 'exec'] and 'vaultcenter-library-*' in a and self.host.image == NEW_IMAGE,
                times=None),
        }

    def test_failure_at_each_step_rolls_back(self):
        for step, inject in self.failures().items():
            with self.subTest(step=step):
                self.setUp()
                self.payload = self.make_payload(NEW_ARCHIVE)
                self.host.on_up = self.simulate_new_version
                before = self.snapshot()
                inject()
                code, result = self.update()
                self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
                after = self.snapshot()
                self.assertEqual(after['files'], before['files'])
                self.assertEqual(after['catalog'], before['catalog'])
                self.assertEqual(after['module'], before['module'])
                self.assertEqual(after['side_files'], [])
                self.assertEqual(self.host.image, OLD_IMAGE)
                self.assertTrue(self.host.running)
                self.assertFalse((self.root / 'var/lib/vaultcenter-host/update-in-progress.json').exists())
                self.assertTrue(result['backup'].startswith('/var/lib/vaultcenter-host/backups/'))
                self.assertTrue((self.root / result['backup'].lstrip('/')).is_dir())
                # A version that never reached the step does not leave its source added.
                self.assertNotIn(dkms_version(NEW_ARCHIVE), self.host.dkms_added)

    def test_a_rolled_back_update_is_not_confirmable(self):
        self.payload = self.make_payload(OLD_ARCHIVE)
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((result['result'], result['confirmable']), ('rolled_back', False), result)

    def test_rollback_restores_the_previous_receipt(self):
        receipt = self.state / 'isolation-qualified.json'
        self.write_receipt(receipt, sha256(b'previous runtime'))
        before = receipt.read_bytes()
        self.payload = self.make_payload(OLD_ARCHIVE, receipt=sha256(RUNTIME))
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual(receipt.read_bytes(), before)

    def test_rollback_leaves_vtls_in_maintenance(self):
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'))
        self.assertIs(result['maintenance_left'], False)
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')
        self.assertEqual(self.host.maintenance_calls('leave'), [])
        self.assertTrue(any('maintenance' in reason for reason in result['reasons']), result)

    def test_rollback_failure_stops_there(self):
        self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'], times=None)
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        self.assertTrue(result['backup'].startswith('/var/lib/vaultcenter-host/backups/'))
        self.assertTrue(result['step'])
        self.assertIs(result['maintenance_left'], False)
        ups = [a for a in self.host.calls if a[-4:] == ['up', '-d', '--force-recreate', 'appliance']]
        self.assertEqual(len(ups), 2)
        marker = json.loads((self.root / 'var/lib/vaultcenter-host/update-in-progress.json').read_text())
        self.assertEqual(marker['backup'], result['backup'])
        self.assertEqual(marker['rollback_failed'], result['step'])
        self.assertEqual(self.host.maintenance_calls('leave'), [])

    def test_restore_on_its_own_returns_to_the_backup(self):
        before = self.snapshot()
        backup = update.backup(self.root, OLD_VERSION)
        compose = self.root / 'etc/vaultcenter-host/compose.json'
        compose.write_text(json.dumps({'name': 'vaultcenter', 'services': {'appliance': {'image': NEW_IMAGE}}}))
        (self.root / 'etc/vaultcenter-host/added-by-new-version').write_text('x')
        (self.root / 'usr/local/libexec/vaultcenter-host/new_module.py').write_text('x')
        self.simulate_new_version(NEW_IMAGE)
        with redirect_stderr(io.StringIO()):
            update.restore(self.root, backup)
        after = self.snapshot()
        self.assertEqual(after['files'], before['files'])
        self.assertEqual(after['catalog'], before['catalog'])
        self.assertEqual(after['side_files'], [])


MARKER = 'var/lib/vaultcenter-host/update-in-progress.json'
VERIFIED = 'var/lib/vaultcenter-host/update-verified.json'
MARKER_NAMES = ('update-in-progress.json', 'update-verified.json')
DONT_ROLL_BACK = r'do not[^.]*--resume-rollback'


def units_text(state):
    return ''.join(f'{unit} loaded {state} x VTL\n' for unit in (
        f'vaultcenter-library-{VTL}.service', f'vaultcenter-drive-{VTL}-1.service', f'vaultcenter-target-{VTL}.service'))


class SafetyTests(UpdateTestCase):
    STEP_CALLS = {
        'kernel module': lambda a: a[:2] == ['dkms', 'build'],
        'host files': lambda a: a == ['systemctl', 'daemon-reload'],
        'container': lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'],
        'verification': lambda a: a[:2] == ['docker', 'inspect'],
    }

    def test_interruption_in_each_step_rolls_back(self):
        for step, predicate in self.STEP_CALLS.items():
            with self.subTest(step=step):
                self.setUp()
                self.payload = self.make_payload(NEW_ARCHIVE)
                before = self.snapshot()
                self.host.raise_when(predicate, update.Interrupted('signal SIGTERM received'))
                code, result = self.update()
                self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
                self.assertTrue(any(step in reason for reason in result['reasons']), result)
                after = self.snapshot()
                self.assertEqual(after['files'], before['files'])
                self.assertEqual(after['catalog'], before['catalog'])
                self.assertEqual(after['module'], before['module'])
                self.assertFalse((self.root / MARKER).exists())

    def test_unexpected_error_in_a_step_rolls_back(self):
        before = self.snapshot()
        self.host.raise_when(lambda a: a == ['systemctl', 'restart', 'vaultcenter-storage-host'],
                             RuntimeError('bug'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual(self.snapshot()['files'], before['files'])

    def test_interruption_during_the_final_leave_still_reports_the_update(self):
        self.host.raise_when(lambda a: a[-1] == 'leave', update.Interrupted('signal SIGTERM received'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertIs(result['maintenance_left'], False)
        self.assertTrue(any('maintenance' in reason for reason in result['reasons']), result)
        # The VTL is still to take out: the verified marker stays for the next run.
        self.assertFalse((self.root / MARKER).exists())
        marker = json.loads((self.root / VERIFIED).read_text())
        self.assertEqual(marker['entered'], [{'vtl': VTL, 'lease': self.host.guards[VTL]['lease']}])

    def test_interruption_during_the_rollback_fails_it(self):
        self.host.unhealthy.add(NEW_IMAGE)
        self.host.raise_when(lambda a: a[-2:] == ['stop', 'appliance'], update.Interrupted('signal SIGTERM received'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        self.assertEqual(result['step'], 'stopping the container')
        self.assertTrue((self.root / MARKER).exists())

    def test_killed_run_is_refused_until_its_rollback_is_resumed(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        before = self.snapshot()
        # An exit no handler catches, in step 8: compose.json already names the new
        # image, and the new version never became healthy.
        self.host.unhealthy.add(NEW_IMAGE)
        self.host.raise_when(self.STEP_CALLS['verification'], SystemExit(1))
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        self.assertEqual(result['step'], 'verification')
        marker = json.loads((self.root / MARKER).read_text())
        self.assertEqual(marker['step'], 'verification')
        self.assertFalse((self.root / VERIFIED).exists())
        self.assertEqual(marker['backup'], result['backup'])
        self.assertEqual((marker['from'], marker['to']), (OLD_VERSION, NEW_VERSION))
        # The same digest is not "unchanged" while the marker exists, nor is any other run.
        for image in (NEW_IMAGE, OLD_IMAGE):
            calls = len(self.host.calls)
            code, refused = self.update(image=image)
            self.assertEqual((code, refused['result']), (3, 'refused'), refused)
            self.assertIn(result['backup'], refused['reasons'][0])
            self.assertIn('verification', refused['reasons'][0])
            self.assertIn('docs/install.md', refused['reasons'][0])
            self.assertIn('--resume-rollback', refused['reasons'][0])
            # Nothing but the read of the appliance's health.
            self.assertEqual([argv[:2] for argv in self.host.calls[calls:]], [['docker', 'inspect']])
        code, resumed = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, resumed['result']), (5, 'rolled_back'), resumed)
        self.assertEqual(resumed['backup'], result['backup'])
        after = self.snapshot()
        self.assertEqual(after['files'], before['files'])
        self.assertEqual(after['catalog'], before['catalog'])
        self.assertEqual(after['module'], before['module'])
        self.assertEqual(self.host.image, OLD_IMAGE)
        self.assertFalse((self.root / MARKER).exists())
        code, again = self.update(image=OLD_IMAGE)
        self.assertEqual((code, again['result']), (0, 'unchanged'))

    def test_resume_without_an_unfinished_update_is_refused(self):
        code, result = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertEqual(self.host.calls, [])

    def test_marker_records_the_step_in_progress(self):
        seen = []

        def watch(argv):
            if argv == ['systemctl', 'restart', 'vaultcenter-storage-host']:
                seen.append(json.loads((self.root / MARKER).read_text()))
        self.host.on_call = watch
        code, result = self.update()
        self.assertEqual(code, 0, result)
        self.assertEqual(seen[0]['step'], 'host files')
        self.assertEqual(seen[0]['backup'], result['backup'])
        self.assertTrue(seen[0]['started'])

    def test_marker_holds_what_the_finish_needs_before_the_first_modification(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        seen = []

        def watch(argv):
            if FakeHost.modifying(argv) and not seen:
                seen.append(json.loads((self.root / MARKER).read_text()))
        self.host.on_call = watch
        code, result = self.update()
        self.assertEqual(code, 0, result)
        self.assertEqual(seen[0]['image'], NEW_IMAGE)
        self.assertEqual(seen[0]['to'], NEW_VERSION)
        self.assertEqual(seen[0]['old_module'], dkms_version(OLD_ARCHIVE))
        self.assertEqual([entry['vtl'] for entry in seen[0]['entered']], [VTL])
        self.assertTrue(seen[0]['entered'][0]['lease'])

    def test_second_run_while_the_lock_is_held_is_refused(self):
        import fcntl
        lock = self.root / 'var/lib/vaultcenter-install/update.lock'
        with open(lock, 'a') as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, result = self.update(image=OLD_IMAGE)
        self.assertEqual((code, result['result']), (3, 'refused'))
        self.assertIn('already running', result['reasons'][0])
        self.assertEqual(self.host.calls, [])

    def test_media_override_is_passed_to_compose(self):
        override = self.root / 'var/lib/vaultcenter-host/mount.json'
        override.write_text('{}\n')
        self.payload = self.make_payload(NEW_ARCHIVE)
        code, result = self.update()
        self.assertEqual(code, 0, result)
        composes = [a for a in self.host.calls if a[:2] == ['docker', 'compose']]
        self.assertEqual(len(composes), 2)
        for argv in composes:
            self.assertEqual(argv[4:6], ['-f', str(override)])

    def test_rollback_order(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        failed = self.host.index(lambda a: a[:2] == ['docker', 'inspect'])
        find = lambda predicate: self.host.index(predicate, failed)
        order = [
            find(lambda a: a[-2:] == ['stop', 'appliance']),
            find(lambda a: a[0] == '#write' and a[1] == 'etc/vaultcenter-host/compose.json'),
            find(lambda a: a[0] == '#write' and a[1].endswith('/catalog.db')),
            find(lambda a: a[0] == '#write' and a[1].startswith('usr/local/libexec/vaultcenter-host/')),
            find(lambda a: a == ['systemctl', 'restart', 'vaultcenter-storage-host']),
            find(lambda a: a[:2] == ['dkms', 'remove']),
            find(lambda a: a[:2] == ['dkms', 'install']),
            find(lambda a: a == ['modprobe', 'mhvtl', 'opts=0']),
            find(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance']),
        ]
        self.assertEqual(order, sorted(order))


class VerifiedUpdateTestCase(UpdateTestCase):
    """What the tests of a verified update share."""

    def marker(self):
        """The content of the verified marker."""
        return json.loads((self.root / VERIFIED).read_text())

    def last_check(self, argv):
        """The last external command of step 8."""
        return units_listing(argv) and self.host.image == NEW_IMAGE

    def kill_at_leave(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.host.raise_when(lambda a: a[-1] == 'leave', SystemExit(1))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertIs(result['maintenance_left'], False)
        return result

    def kill_in_verification(self, predicate=None):
        """A run killed inside step 8, before its verification was recorded."""
        self.host.raise_when(predicate or self.last_check, SystemExit(1))
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        self.assertTrue((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())
        return self.root / result['backup'].lstrip('/')

    def write_after_update(self):
        """A tape write recorded once the update was verified."""
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute("INSERT OR IGNORE INTO jobs VALUES ('after-update', 'export', 'A00042', 'succeeded')")
            db.commit()

    def has_row_written_after_update(self):
        return any("'after-update'" in line for line in self.catalog_dump())

    def assert_never_rolled_back(self, result):
        """The invariant of every run that got past step 8.

        No update-in-progress.json is left unless the JSON says not to roll
        back, and neither --resume-rollback nor a normal run, in any order,
        puts the old image back or loses a row written since. Returns the
        results of [--resume-rollback, normal run, --resume-rollback].
        """
        self.assertEqual(result['result'], 'updated', result)
        if (self.root / MARKER).exists():
            self.assertRegex(' '.join(result['reasons']), DONT_ROLL_BACK)
        self.write_after_update()
        later = []
        for argv in (['--resume-rollback'], None, ['--resume-rollback']):
            code, answer = self.update(argv=argv) if argv else self.update()
            later.append(answer)
            if argv:
                self.assertEqual((code, answer['result']), (3, 'refused'), answer)
            else:
                self.assertIn(answer['result'], ('unchanged', 'refused'), answer)
                self.assertFalse(any(re.search(r'run again with --resume-rollback', reason)
                                     for reason in answer['reasons']), answer)
            self.assertEqual(self.compose_image(), NEW_IMAGE)
            self.assertEqual(self.host.image, NEW_IMAGE)
            self.assertTrue(self.host.running)
            self.assertTrue(self.has_row_written_after_update())
        return later

    @staticmethod
    def failing_os(name, filename, error):
        """os.<name> raises `error` for the file called `filename` only."""
        real = getattr(os, name)

        def call(path, *args, **kwargs):
            if isinstance(path, (str, os.PathLike)) and Path(path).name == filename:
                raise error
            return real(path, *args, **kwargs)
        return mock.patch.object(update.os, name, call)

    def failing_rename(self):
        return self.failing_os('rename', 'update-in-progress.json', OSError(30, 'Read-only file system'))


class FinalisationTests(VerifiedUpdateTestCase):
    """A verified update is finished, never rolled back."""

    def test_kill_after_verification_is_finished_by_the_next_run(self):
        self.kill_at_leave()
        self.assertFalse((self.root / MARKER).exists())
        marker = self.marker()
        self.assertEqual(marker['entered'], [{'vtl': VTL, 'lease': self.host.guards[VTL]['lease']}])
        self.assertTrue(marker['entered'][0]['lease'])
        self.assertEqual(marker['old_module'], dkms_version(OLD_ARCHIVE))
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')
        self.write_after_update()
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertTrue(any('verified' in warning and 'finalised' in warning for warning in result['warnings']),
                        result)
        self.assertFalse((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertTrue(self.has_row_written_after_update())
        self.assertEqual(self.host.image, NEW_IMAGE)

    def test_finishing_skips_a_vtl_already_out_of_maintenance(self):
        self.kill_at_leave()
        self.host.guards[VTL]['phase'] = 'online'
        leaves = len(self.host.maintenance_calls('leave'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(len(self.host.maintenance_calls('leave')), leaves)
        self.assertFalse((self.root / VERIFIED).exists())

    def test_resume_rollback_refuses_a_verified_update(self):
        self.kill_at_leave()
        self.write_after_update()
        before = self.marker()
        for flags in (['--resume-rollback'], ['--resume-rollback', '--force-rollback']):
            with self.subTest(flags=flags):
                calls = len(self.host.calls)
                code, result = self.update(argv=flags)
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertIn('verified', result['reasons'][0])
                self.assertEqual(self.host.calls[calls:], [])
                self.assertEqual(self.marker(), before)
                self.assertTrue(self.has_row_written_after_update())

    def test_both_marker_files_at_once_mean_verified(self):
        self.kill_at_leave()
        self.write_after_update()
        shutil.copy(self.root / VERIFIED, self.root / MARKER)
        for flags in (['--resume-rollback'], ['--resume-rollback', '--force-rollback']):
            with self.subTest(flags=flags):
                calls = len(self.host.calls)
                code, result = self.update(argv=flags)
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertIn('verified', result['reasons'][0])
                self.assertEqual(self.host.calls[calls:], [])
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertFalse((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())
        self.assertEqual(self.compose_image(), NEW_IMAGE)
        self.assertTrue(self.has_row_written_after_update())

    def test_failed_finishing_is_refused_and_keeps_the_verified_marker(self):
        self.kill_at_leave()
        self.host.fail(lambda a: a[-1] == 'leave', times=None)
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertEqual([entry['vtl'] for entry in self.marker()['entered']], [VTL])
        self.assertFalse(any('--resume-rollback' in reason for reason in result['reasons']), result)

    def test_failed_leave_keeps_the_verified_marker_for_the_next_run(self):
        self.host.fail(lambda a: a[-1] == 'leave')
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertIs(result['maintenance_left'], False)
        self.assertTrue(any('next run' in reason for reason in result['reasons']), result)
        self.assertFalse((self.root / MARKER).exists())
        self.assertEqual([entry['vtl'] for entry in self.marker()['entered']], [VTL])
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertFalse((self.root / VERIFIED).exists())

    def test_successful_update_clears_the_marker(self):
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertFalse((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())

    def test_resume_rollback_needs_a_complete_backup(self):
        for damage in ('missing catalogue', 'corrupt catalogue', 'missing tar'):
            with self.subTest(damage):
                self.setUp()
                backup = self.kill_in_verification()
                self.host.unhealthy.add(NEW_IMAGE)
                if damage == 'missing catalogue':
                    (backup / 'catalog.db').unlink()
                elif damage == 'corrupt catalogue':
                    (backup / 'catalog.db').write_bytes(b'SQLite format 3\x00' + b'\x00' * 200)
                else:
                    (backup / 'host-files.tar').unlink()
                calls = len(self.host.calls)
                code, result = self.update(argv=['--resume-rollback'])
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertIn('backup', result['reasons'][0])
                self.assertFalse(any(a[-2:] == ['stop', 'appliance'] for a in self.host.calls[calls:]))
                self.assertTrue((self.root / MARKER).exists())

    def test_unreadable_marker_is_refused_and_left_untouched(self):
        for name in (MARKER, VERIFIED):
            for argv in (None, ['--resume-rollback']):
                if name == VERIFIED and argv:
                    continue    # its presence alone refuses a rollback (tested below)
                with self.subTest(name=name, argv=argv):
                    self.setUp()
                    marker = self.root / name
                    marker.write_bytes(b'\x00\xffnot json')
                    code, result = self.update(argv=argv) if argv else self.update()
                    self.assertEqual((code, result['result']), (3, 'refused'), result)
                    self.assertIn('/' + name, result['reasons'][0])
                    self.assertIn('unreadable', result['reasons'][0])
                    self.assertNotIn('run again with --resume-rollback', result['reasons'][0])
                    self.assertEqual(marker.read_bytes(), b'\x00\xffnot json')
                    self.assertEqual(self.host.calls, [])

    def test_unreadable_verified_marker_still_refuses_a_rollback(self):
        (self.root / VERIFIED).write_bytes(b'\x00\xffnot json')
        (self.root / MARKER).write_text(json.dumps({'backup': '/var/lib/vaultcenter-host/backups/x'}))
        for flags in (['--resume-rollback'], ['--resume-rollback', '--force-rollback']):
            with self.subTest(flags=flags):
                code, result = self.update(argv=flags)
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertIn('verified', result['reasons'][0])
                self.assertEqual(self.host.calls, [])

    def test_marker_that_cannot_be_examined_is_unreadable_not_absent(self):
        error = OSError(5, 'Input/output error')
        for name in (MARKER, VERIFIED):
            for argv in (None, ['--resume-rollback']):
                with self.subTest(name=name, argv=argv):
                    self.setUp()
                    filename = Path(name).name
                    with self.failing_os('stat', filename, error), self.failing_os('lstat', filename, error):
                        code, result = self.update(argv=argv) if argv else self.update()
                    self.assertEqual((code, result['result']), (3, 'refused'), result)
                    self.assertIn('/' + name, result['reasons'][0])
                    self.assertIn('unreadable', result['reasons'][0])
                    self.assertEqual(self.host.calls, [])
                    self.assertEqual(self.compose_image(), OLD_IMAGE)

    # ------------------------------------------------------------- leases
    def test_vtl_taken_over_between_the_runs_is_not_left(self):
        self.kill_at_leave()
        self.host.guards[VTL].update(phase='online', lease=None)
        self.host.operator_enter(VTL)
        leaves = len(self.host.maintenance_calls('leave'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(len(self.host.maintenance_calls('leave')), leaves)
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')
        self.assertTrue(any(VTL in warning and 'taken over' in warning for warning in result['warnings']), result)
        self.assertFalse((self.root / VERIFIED).exists())

    def test_vtl_with_the_recorded_lease_is_left(self):
        self.kill_at_leave()
        leaves = len(self.host.maintenance_calls('leave'))
        code, result = self.update()
        self.assertEqual(len(self.host.maintenance_calls('leave')), leaves + 1)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')

    def test_recorded_null_lease_is_skipped_by_a_later_run(self):
        self.kill_at_leave()
        marker = self.marker()
        marker['entered'] = [{'vtl': VTL, 'lease': None}]
        (self.root / VERIFIED).write_text(json.dumps(marker))
        leaves = len(self.host.maintenance_calls('leave'))
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(len(self.host.maintenance_calls('leave')), leaves)
        self.assertTrue(any(VTL in warning for warning in result['warnings']), result)
        self.assertFalse((self.root / VERIFIED).exists())

    def test_null_lease_is_left_within_the_same_run(self):
        self.host.guards[VTL]['no_lease'] = True
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertIs(result['maintenance_left'], True)

    def test_vtl_taken_over_during_the_run_is_not_left(self):
        def take_over(argv):
            if argv[0] == 'curl' and self.host.image == NEW_IMAGE:
                self.host.operator_enter(VTL)
        self.host.on_call = take_over
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.maintenance_calls('leave'), [])
        self.assertTrue(any(VTL in warning for warning in result['warnings']), result)
        # It is still in maintenance, whoever holds it now.
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')
        self.assertIs(result['maintenance_left'], False)

    def test_maintenance_left_is_false_when_one_of_two_vtls_was_taken_over(self):
        with closing(sqlite3.connect(self.state / 'catalog.db')) as db:
            db.execute("INSERT INTO vtls VALUES (?, 'ready')", (OTHER_VTL,))
            db.execute("INSERT INTO vtl_provisioning VALUES (?, 'active')", (OTHER_VTL,))
            db.commit()
        self.host.guards[OTHER_VTL] = {'phase': 'online', 'client': 'pbs'}

        def take_over(argv):
            if argv[0] == 'curl' and self.host.image == NEW_IMAGE:
                self.host.operator_enter(OTHER_VTL)
        self.host.on_call = take_over
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertEqual(self.host.guards[OTHER_VTL]['phase'], 'maintenance')
        self.assertIs(result['maintenance_left'], False)

    def test_veeam_vtl_follows_the_same_lease_rules(self):
        self.host.guards[VTL]['client'] = 'veeam'
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.host.raise_when(lambda a: a[-1] == 'leave', SystemExit(1))
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertTrue(self.marker()['entered'][0]['lease'])
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.kill_at_leave_veeam_takeover()

    def kill_at_leave_veeam_takeover(self):
        self.setUp()
        self.host.guards[VTL]['client'] = 'veeam'
        self.host.raise_when(lambda a: a[-1] == 'leave', SystemExit(1))
        self.update('--yes')
        self.host.guards[VTL].update(phase='online', lease=None)
        self.host.operator_enter(VTL)
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'unchanged'), result)
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')

    def test_refused_preflight_cleanup_checks_the_lease(self):
        def take_over(argv):
            if argv[:2] == ['docker', 'exec'] and 'vaultcenter-worker-*' in argv and self.host.guards[VTL]['phase'] == 'maintenance':
                self.host.operator_enter(VTL)
                self.host.workers = f'vaultcenter-worker-{VTL}-0123abcd.service loaded active running Worker\n'
        self.host.on_call = take_over
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertEqual(self.host.maintenance_calls('leave'), [])
        self.assertEqual(self.host.guards[VTL]['phase'], 'maintenance')
        self.assertIs(result['maintenance_left'], False)

    def test_unexpected_exception_before_any_modification_is_refused(self):
        cases = {
            'health': (lambda a: a[0] == 'curl', RuntimeError('lecture /health')),
            'enter': (lambda a: a[-1] == 'enter', RuntimeError('enter')),
            'exit at health': (lambda a: a[0] == 'curl', SystemExit(1)),
            'exit at enter': (lambda a: a[-1] == 'enter', SystemExit(1)),
        }
        for name, (predicate, error) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.host.raise_when(predicate, error)
                code, result = self.update()
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertTrue(result['reasons'], result)
                self.assertFalse((self.root / MARKER).exists())
                self.assertEqual(self.backups(), [])
                self.assertFalse(any(a[:2] == ['docker', 'compose'] or a[0] in ('dkms', 'modprobe', 'systemctl')
                                     for a in self.host.calls))


class VerificationRecordTests(VerifiedUpdateTestCase):
    """Step 8 ends by renaming update-in-progress.json to update-verified.json:
    that name, and nothing else, means the update is verified."""

    def after_last_check(self):
        """A list that becomes non-empty once step 8 has made its last check."""
        made = []
        self.host.on_call = lambda argv: made.append(1) if self.last_check(argv) else None
        return made

    # ----------------------------------------------- the reviewer's probes
    def test_stderr_that_breaks_during_step_8_changes_nothing(self):
        errors = {'broken pipe': BrokenPipeError(32, 'Broken pipe'),
                  'full disk': OSError(28, 'No space left on device'),
                  'closed stream': ValueError('I/O operation on closed file')}
        for name, error in errors.items():
            with self.subTest(name):
                self.setUp()
                stderr = FailingStream(lambda writes: self.host.image == NEW_IMAGE, error)
                code, result = self.update(stderr=stderr)
                self.assertEqual((code, result['result']), (0, 'updated'), result)
                self.assertIs(result['maintenance_left'], True)
                self.assertFalse((self.root / MARKER).exists())
                self.assertFalse((self.root / VERIFIED).exists())
                later = self.assert_never_rolled_back(result)
                self.assertEqual(later[1]['result'], 'unchanged', later[1])

    def test_marker_writes_that_fail_after_step_8_change_nothing(self):
        for leave_fails in (False, True):
            with self.subTest(leave_fails=leave_fails):
                self.setUp()
                passed = self.after_last_check()
                self.fail_write = lambda path, data: bool(passed) and Path(path).name in MARKER_NAMES
                if leave_fails:
                    self.host.fail(lambda a: a[-1] == 'leave')
                code, result = self.update()
                self.assertEqual((code, result['result']), (0, 'updated'), result)
                self.assertFalse((self.root / MARKER).exists())
                self.assertIs((self.root / VERIFIED).exists(), leave_fails)
                self.assertIs(result['maintenance_left'], not leave_fails)
                self.fail_write = None
                later = self.assert_never_rolled_back(result)
                self.assertEqual(later[1]['result'], 'unchanged', later[1])
                self.assertEqual(self.host.guards[VTL]['phase'], 'online')
                self.assertFalse((self.root / VERIFIED).exists())

    def test_verification_is_recorded_before_the_next_progress_line(self):
        passed = self.after_last_check()
        seen = []

        def watch(writes):
            if passed:
                seen.append(((self.root / VERIFIED).exists(), (self.root / MARKER).exists()))
            return False
        code, result = self.update(stderr=FailingStream(watch))
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertTrue(seen)
        self.assertEqual(seen[0], (True, False))

    def test_kill_at_the_first_progress_line_after_step_8_leaves_a_verified_update(self):
        passed = self.after_last_check()
        code, result = self.update(stderr=FailingStream(lambda writes: bool(passed), SystemExit(1)))
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertIs(result['maintenance_left'], False)
        self.assertFalse((self.root / MARKER).exists())
        self.assertTrue((self.root / VERIFIED).exists())
        later = self.assert_never_rolled_back(result)
        self.assertEqual(later[1]['result'], 'unchanged', later[1])
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')

    def test_failing_stderr_never_changes_the_outcome(self):
        scenarios = {
            'plain': lambda: None,
            'module change': lambda: setattr(self, 'payload', self.make_payload(NEW_ARCHIVE)),
            'failed leave': lambda: self.host.fail(lambda a: a[-1] == 'leave'),
            'rollback': lambda: self.host.unhealthy.add(NEW_IMAGE),
            'failed rollback': lambda: self.host.fail(
                lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'], times=None),
        }

        def outcome(code, result):
            return (code, result['result'], result['maintenance_left'], result['step'],
                    (self.root / MARKER).exists(), (self.root / VERIFIED).exists(), self.host.image,
                    self.host.guards[VTL]['phase'], self.compose_image())
        for name, prepare in scenarios.items():
            self.setUp()
            prepare()
            counting = FailingStream(lambda writes: False)
            expected = outcome(*self.update(stderr=counting))
            self.assertGreater(counting.writes, 6, name)
            for first_failure in range(1, counting.writes + 1):
                with self.subTest(scenario=name, first_failing_write=first_failure):
                    self.setUp()
                    prepare()
                    stderr = FailingStream(lambda writes: writes >= first_failure)
                    code, result = self.update(stderr=stderr)
                    self.assertEqual(outcome(code, result), expected, result)
                    if result['result'] == 'updated':
                        self.assert_never_rolled_back(result)

    def test_unwritable_stdout_keeps_the_exit_code(self):
        for image, expected in ((OLD_IMAGE, 0), ('ghcr.io/adminsyspro/vaultcenter:0.2.0', 3)):
            with self.subTest(expected=expected):
                argv = ['--image', image, '--version', NEW_VERSION, '--payload', str(self.payload)]
                with redirect_stdout(FailingStream(lambda writes: True)), redirect_stderr(io.StringIO()):
                    self.assertEqual(update.main(argv), expected)

    def test_stdout_on_a_full_disk_keeps_the_exit_code_of_the_real_program(self):
        for image, expected in ((OLD_IMAGE, 0), ('ghcr.io/adminsyspro/vaultcenter:0.2.0', 3)):
            with self.subTest(expected=expected), open('/dev/full', 'w') as full:
                done = subprocess.run(
                    [sys.executable, '-B', str(REPO / 'deploy/host/update.py'), '--image', image,
                     '--version', NEW_VERSION, '--payload', str(self.payload)],
                    stdout=full, stderr=subprocess.PIPE, text=True, timeout=60,
                    env={**os.environ, 'VAULTCENTER_INSTALL_ROOT': str(self.root)})
                self.assertEqual(done.returncode, expected, done.stderr)
                self.assertNotIn('Traceback', done.stderr)
                self.assertNotIn('Exception ignored', done.stderr)

    # --------------------------------- only the rename marks verification
    def test_kill_at_the_last_command_of_step_8_is_not_verified(self):
        self.kill_in_verification()
        self.assertEqual(json.loads((self.root / MARKER).read_text())['step'], 'verification')
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertEqual(self.compose_image(), NEW_IMAGE)
        self.assertTrue((self.root / MARKER).exists())

    def test_resume_rollback_restores_when_the_appliance_is_unhealthy(self):
        cases = {
            'unhealthy': lambda: self.host.unhealthy.add(NEW_IMAGE),
            'stopped': lambda: setattr(self.host, 'running', False),
            'another version': lambda: self.host.versions.__setitem__(NEW_IMAGE, '0.1.9'),
        }
        for name, damage in cases.items():
            with self.subTest(name):
                self.setUp()
                before = self.snapshot()
                self.kill_in_verification()
                damage()
                code, refused = self.update()
                self.assertEqual((code, refused['result']), (3, 'refused'), refused)
                self.assertIn('run again with --resume-rollback', refused['reasons'][0])
                code, result = self.update(argv=['--resume-rollback'])
                self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
                self.assertEqual(self.compose_image(), OLD_IMAGE)
                self.assertEqual(self.host.image, OLD_IMAGE)
                after = self.snapshot()
                self.assertEqual(after['files'], before['files'])
                self.assertEqual(after['catalog'], before['catalog'])
                self.assertFalse((self.root / MARKER).exists())

    def test_failed_resumed_rollback_keeps_what_the_marker_recorded(self):
        self.payload = self.make_payload(NEW_ARCHIVE)
        self.kill_in_verification()
        self.host.unhealthy.add(NEW_IMAGE)
        before = json.loads((self.root / MARKER).read_text())
        self.assertTrue(before['entered'][0]['lease'])
        self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'], times=None)
        code, result = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        after = json.loads((self.root / MARKER).read_text())
        self.assertEqual(after.pop('rollback_failed'), result['step'])
        self.assertEqual(after, before)

    def test_resume_rollback_restores_when_another_image_is_installed(self):
        self.kill_in_verification()
        marker = json.loads((self.root / MARKER).read_text())
        marker['image'] = 'ghcr.io/adminsyspro/vaultcenter@sha256:' + 'c' * 64
        (self.root / MARKER).write_text(json.dumps(marker))
        calls = len(self.host.calls)
        code, result = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        # The health of an appliance that is not on the recorded image is not even read.
        self.assertEqual(self.host.calls[calls][-2:], ['stop', 'appliance'])

    def test_resume_rollback_is_refused_when_the_appliance_is_healthy_on_the_target(self):
        self.kill_in_verification()
        self.write_after_update()
        calls = len(self.host.calls)
        code, result = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        reason = result['reasons'][0]
        for text in (NEW_VERSION, 'good health', 'since', 'manual procedure', 'appliance stopped'):
            self.assertIn(text, reason)
        self.assertFalse(any(FakeHost.modifying(argv) for argv in self.host.calls[calls:]))
        self.assertEqual(self.compose_image(), NEW_IMAGE)
        self.assertTrue(self.has_row_written_after_update())
        self.assertTrue((self.root / MARKER).exists())
        # The ordinary refusal does not offer the rollback either.
        calls = len(self.host.calls)
        code, refused = self.update()
        self.assertEqual((code, refused['result']), (3, 'refused'), refused)
        self.assertRegex(refused['reasons'][0], DONT_ROLL_BACK)
        self.assertNotIn('run again with --resume-rollback', refused['reasons'][0])
        self.assertIn('/' + MARKER, refused['reasons'][0])
        self.assertFalse(any(FakeHost.modifying(argv) for argv in self.host.calls[calls:]))

    def test_force_rollback_restores_a_healthy_appliance(self):
        before = self.snapshot()
        self.kill_in_verification()
        code, result = self.update(argv=['--resume-rollback', '--force-rollback'])
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertEqual(self.compose_image(), OLD_IMAGE)
        after = self.snapshot()
        self.assertEqual(after['files'], before['files'])
        self.assertEqual(after['catalog'], before['catalog'])
        self.assertFalse((self.root / MARKER).exists())

    def test_force_rollback_never_applies_to_a_verified_update(self):
        self.kill_at_leave()
        self.write_after_update()
        calls = len(self.host.calls)
        code, result = self.update(argv=['--resume-rollback', '--force-rollback'])
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertIn('verified', result['reasons'][0])
        self.assertEqual(self.host.calls[calls:], [])
        self.assertEqual(self.compose_image(), NEW_IMAGE)
        self.assertTrue(self.has_row_written_after_update())

    def test_force_rollback_needs_resume_rollback(self):
        argv = ['--image', NEW_IMAGE, '--version', NEW_VERSION, '--payload', str(self.payload), '--force-rollback']
        with self.assertRaises(SystemExit) as raised, redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            update.main(argv)
        self.assertEqual(raised.exception.code, 2)
        self.assertEqual(self.host.calls, [])

    # ------------------------------------------------- a rename that fails
    def test_failed_rename_removes_the_in_progress_marker(self):
        with self.failing_rename():
            code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertFalse((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())
        self.assertTrue(any('marker' in warning for warning in result['warnings']), result)
        self.assertIs(result['maintenance_left'], True)
        later = self.assert_never_rolled_back(result)
        self.assertEqual(later[1]['result'], 'unchanged', later[1])
        self.assertFalse(any('--resume-rollback' in text for text in later[1]['reasons'] + later[1]['warnings']))

    def test_failed_rename_and_failed_leave_leave_no_marker(self):
        self.host.fail(lambda a: a[-1] == 'leave')
        with self.failing_rename():
            code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertIs(result['maintenance_left'], False)
        self.assertFalse((self.root / MARKER).exists())
        self.assertTrue(any('console' in reason for reason in result['reasons']), result)
        self.assert_never_rolled_back(result)

    def test_failed_rename_and_failed_removal_say_not_to_roll_back(self):
        error = OSError(30, 'Read-only file system')
        with self.failing_rename(), self.failing_os('unlink', 'update-in-progress.json', error):
            code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        text = ' '.join(result['reasons'])
        self.assertIn('/var/lib/vaultcenter-host/update-in-progress.json', text)
        self.assertRegex(text, DONT_ROLL_BACK)
        self.assertTrue((self.root / MARKER).exists())
        # The finish itself went through.
        self.assertEqual(self.host.guards[VTL]['phase'], 'online')
        self.assertIs(result['maintenance_left'], True)
        later = self.assert_never_rolled_back(result)
        # The rollback is refused on the appliance's own state, and never offered.
        self.assertIn('good health', later[0]['reasons'][0])
        self.assertEqual(later[1]['result'], 'refused', later[1])
        self.assertRegex(later[1]['reasons'][0], DONT_ROLL_BACK)

    def test_exit_while_securing_the_marker_reports_what_is_on_disk(self):
        with self.failing_rename(), self.failing_os('unlink', 'update-in-progress.json', SystemExit(1)):
            code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        text = ' '.join(result['reasons'])
        self.assertNotIn('verified marker', text)
        self.assertRegex(text, DONT_ROLL_BACK)
        self.assertIs(result['maintenance_left'], False)
        self.assert_never_rolled_back(result)

    # ------------------------------------------------------------ signals
    def send_at_last_check(self, number=signal.SIGTERM):
        sent = []

        def send(argv):
            if self.last_check(argv) and not sent:
                sent.append(1)
                os.kill(os.getpid(), number)
        self.host.on_call = send
        return sent

    def test_signal_between_the_last_check_and_the_rename_is_delivered_after_it(self):
        for number in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            with self.subTest(signal=number.name):
                self.setUp()
                sent = self.send_at_last_check(number)
                code, result = self.update()
                self.assertEqual(sent, [1])
                self.assertEqual((code, result['result']), (0, 'updated'), result)
                self.assertTrue(any('Interruption' in reason for reason in result['reasons']), result)
                self.assertIs(result['maintenance_left'], False)
                self.assertFalse((self.root / MARKER).exists())
                self.assertEqual([entry['vtl'] for entry in self.marker()['entered']], [VTL])
                self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, []) & {number}, set())
                later = self.assert_never_rolled_back(result)
                self.assertEqual(later[1]['result'], 'unchanged', later[1])
                self.assertEqual(self.host.guards[VTL]['phase'], 'online')

    def test_signal_during_a_check_that_does_not_pass_rolls_back(self):
        self.host.listing = lambda: units_text('active')
        sent = self.send_at_last_check()
        code, result = self.update()
        self.assertEqual(sent, [1])
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertTrue(any('SIGTERM' in reason for reason in result['reasons']), result)
        self.assertFalse((self.root / MARKER).exists())
        self.assertFalse((self.root / VERIFIED).exists())
        self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, []) & {signal.SIGTERM}, set())


class ResumeHealthTests(VerifiedUpdateTestCase):
    """--resume-rollback concludes on the appliance's health only when it is known."""

    def modifying_since(self, calls):
        return [argv for argv in self.host.calls[calls:] if FakeHost.modifying(argv)]

    def test_container_that_is_starting_is_waited_for(self):
        # After a reboot the container is "starting" for a while: one probe is
        # not enough to conclude that the new version does not run.
        self.kill_in_verification()
        self.write_after_update()
        self.host.health_sequence = ['starting', 'starting']
        calls = len(self.host.calls)
        with mock.patch.dict(os.environ, {'VAULTCENTER_UPDATE_HEALTH_TIMEOUT': '30'}):
            code, result = self.update(argv=['--resume-rollback'])
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        self.assertIn('good health', result['reasons'][0])
        self.assertIn('--resume-rollback --force-rollback', result['reasons'][0])
        self.assertEqual(len([a for a in self.host.calls[calls:] if a[:2] == ['docker', 'inspect']]), 3)
        self.assertEqual(self.modifying_since(calls), [])
        self.assertEqual(self.compose_image(), NEW_IMAGE)
        self.assertTrue(self.has_row_written_after_update())

    def test_health_that_stays_undetermined_refuses_without_force(self):
        cases = {'starting': lambda: setattr(self.host, 'health_sequence', ['starting'] * 50),
                 'no status': lambda: setattr(self.host, 'health_sequence', [''] * 50),
                 'probes keep failing': lambda: self.host.fail(lambda a: a[0] == 'curl', times=None)}
        for name, damage in cases.items():
            with self.subTest(name):
                self.setUp()
                self.kill_in_verification()
                self.write_after_update()
                damage()
                calls = len(self.host.calls)
                code, result = self.update(argv=['--resume-rollback'])
                self.assertEqual((code, result['result']), (3, 'refused'), result)
                self.assertIn('could not be determined', result['reasons'][0])
                self.assertIn('--resume-rollback --force-rollback', result['reasons'][0])
                self.assertIn('restores the backup', result['reasons'][0])
                self.assertEqual(self.modifying_since(calls), [])
                self.assertEqual(self.compose_image(), NEW_IMAGE)
                self.assertTrue((self.root / MARKER).exists())
                self.assertTrue(self.has_row_written_after_update())

    def test_force_rollback_restores_without_asking_the_health(self):
        before = self.snapshot()
        self.kill_in_verification()
        calls = len(self.host.calls)
        code, result = self.update(argv=['--resume-rollback', '--force-rollback'])
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        first = self.host.calls[calls]
        self.assertEqual(first[-2:], ['stop', 'appliance'])
        self.assertEqual(self.snapshot()['files'], before['files'])

    def test_normal_run_never_says_to_delete_the_marker_of_a_healthy_appliance(self):
        self.kill_in_verification()
        code, result = self.update()
        self.assertEqual((code, result['result']), (3, 'refused'), result)
        reason = result['reasons'][0]
        self.assertNotIn('delete', reason)
        self.assertIn('--resume-rollback --force-rollback', reason)
        for text in ('stops the appliance', 'restores the backup', 'is lost', '/var/lib/vaultcenter-host/backups/'):
            self.assertIn(text, reason)

    def test_normal_run_after_a_failed_rollback_says_so_and_names_the_flags(self):
        self.host.fail(lambda a: a[-4:] == ['up', '-d', '--force-recreate', 'appliance'], times=None)
        code, result = self.update()
        self.assertEqual((code, result['result']), (6, 'rollback_failed'), result)
        code, refused = self.update()
        self.assertEqual((code, refused['result']), (3, 'refused'), refused)
        reason = refused['reasons'][0]
        self.assertIn(f'previous rollback failed at step "{result["step"]}"', reason)
        self.assertIn('--resume-rollback', reason)
        self.assertIn('--force-rollback', reason)
        self.assertNotIn('delete', reason)

    def test_resumed_rollback_reads_which_vtls_are_still_in_maintenance(self):
        cases = {'taken out since': ('online', []), 'still in maintenance': ('maintenance', [VTL])}
        for name, (phase, expected) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.kill_in_verification()
                self.host.unhealthy.add(NEW_IMAGE)
                self.host.guards[VTL]['phase'] = phase
                code, result = self.update(argv=['--resume-rollback'])
                self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
                self.assertEqual([item['vtl'] for item in result['still_in_maintenance']], expected)
                self.assertIs(result['maintenance_left'], not expected)
                says_they_remain = any('Still in maintenance' in reason for reason in result['reasons'])
                self.assertEqual(says_they_remain, bool(expected), result)
                self.assertEqual(self.host.maintenance_calls('leave'), [])
                self.assertEqual(self.host.maintenance_calls('enter')[1:], [])


class SignalMaskTests(UpdateTestCase):
    def test_handler_that_raises_as_the_mask_is_set_leaves_no_signal_held(self):
        blocked = set(update.BLOCKED_SIGNALS)
        real = signal.pthread_sigmask
        self.addCleanup(real, signal.SIG_UNBLOCK, blocked)
        fired = []

        def pending_handler_runs(how, mask):
            result = real(how, mask)
            if how == signal.SIG_BLOCK and set(mask) == blocked and not fired:
                # A signal that arrived just before: its Python handler runs
                # here, once the mask is set and before the call returns.
                fired.append(True)
                raise update.Interrupted('signal SIGTERM received')
            return result

        with mock.patch.object(update.signal, 'pthread_sigmask', pending_handler_runs):
            code, result = self.update()
        self.assertTrue(fired)
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)
        self.assertTrue(any('verification' in reason and 'SIGTERM' in reason for reason in result['reasons']), result)
        # Neither this process nor the commands of the rollback kept them blocked.
        self.assertEqual(blocked & real(signal.SIG_BLOCK, []), set())
        self.assertEqual(self.host.violations, [])
        self.assertEqual(self.host.image, OLD_IMAGE)


class UnitComparisonTests(UpdateTestCase):
    def test_inactive_unit_that_disappears_passes(self):
        self.host.listing = lambda: ''
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)

    def test_unit_active_before_and_not_after_fails(self):
        self.host.status_fails.add(VTL)
        self.host.listing = lambda: units_text('inactive')
        code, result = self.update('--yes')
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)

    def test_unit_active_after_and_not_before_fails(self):
        self.host.listing = lambda: units_text('active')
        code, result = self.update()
        self.assertEqual((code, result['result']), (5, 'rolled_back'), result)


class PruneTests(UpdateTestCase):
    def make_backup(self, name):
        path = self.root / 'var/lib/vaultcenter-host/backups' / name
        path.mkdir(parents=True)
        (path / 'version').write_text('0.0.1\n')
        return path

    def test_only_three_backups_are_kept(self):
        for name in ('20261101-100000-0.0.1', '20261102-100000-0.0.1', '20261103-100000-0.0.1',
                     '20261104-100000-0.0.1'):
            self.make_backup(name)
        code, result = self.update()
        self.assertEqual((code, result['result']), (0, 'updated'), result)
        self.assertEqual(self.backups(), sorted([Path(result['backup']).name, '20261103-100000-0.0.1',
                                                 '20261104-100000-0.0.1']))

    def test_prune_never_deletes_the_fresh_backup(self):
        fresh = self.make_backup('20261001-100000-0.1.0')
        for name in ('20261101-100000-0.0.1', '20261102-100000-0.0.1', '20261103-100000-0.0.1'):
            self.make_backup(name)
        other = self.root / 'var/lib/vaultcenter-host/backups/notes'
        other.mkdir()
        update.prune_backups(self.root, keep=3, protect=fresh)
        self.assertEqual(self.backups(), ['20261001-100000-0.1.0', '20261102-100000-0.0.1',
                                          '20261103-100000-0.0.1', 'notes'])

    def test_failed_update_does_not_prune(self):
        for name in ('20261101-100000-0.0.1', '20261102-100000-0.0.1', '20261103-100000-0.0.1'):
            self.make_backup(name)
        self.host.unhealthy.add(NEW_IMAGE)
        code, result = self.update()
        self.assertEqual(result['result'], 'rolled_back')
        self.assertEqual(len(self.backups()), 4)


if __name__ == '__main__':
    unittest.main()
