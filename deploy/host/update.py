#!/usr/bin/env python3
"""Update an installed VaultCenter appliance, and roll back by itself on failure.

Launched by install.sh from the payload of the NEW version, as root, on the
host:

    python3 update.py --image <name@sha256:…> --version X.Y.Z[-pre] --payload <dir> [--yes]
    python3 update.py --resume-rollback [--force-rollback]

<dir> is the extracted payload (…/vaultcenter/<version>/, holding host/ and
mhvtl-source.tar.gz). The image is already local: this script never talks to
a registry and handles no secret.

Order:
version check (X.Y.Z or X.Y.Z-<pre-release>, ordered as semver), safe-restart
conditions, maintenance, backup, then the modifying steps 5 (kernel module, for
every kernel that had the previous one), 6 (host files), 7 (container) and
8 (verification). A failure of 5 to 8 restores the backup (rollback) and
leaves the VTLs in maintenance; a failure of the restoration stops there.

Progress goes to stderr in English. The last line of stdout is one JSON object:
{"result", "from", "to", "reasons", "warnings", "backup", "still_in_maintenance",
"maintenance_left", "step", "confirmable"}. still_in_maintenance lists the VTLs an
update put in maintenance that are still there at the end, as {"vtl", "name",
"reason"}; maintenance_left is true exactly when that list is empty. confirmable:
refused only because --yes would let some VTLs through.
Exit codes: 0 updated/unchanged, 3 refused, 5 rolled_back, 6 rollback_failed.

From step 5 until the outcome is known, /var/lib/vaultcenter-host/
update-in-progress.json records the backup, the step reached and what the
finish will need. While it exists every run is refused, except
`--resume-rollback`, which restores that backup. The last act of step 8 is to
rename it to update-verified.json: that name, and nothing written in the
file, means the update is verified. A verified update is never rolled back,
and the next run completes its finish (leave maintenance, clean up).
`--resume-rollback` also refuses to restore under an appliance that runs the
target version in good health, or whose health cannot be determined, unless
`--force-rollback` is given. The script never prompts: install.sh asks and
passes --yes.

Every path read or written is prefixed by VAULTCENTER_INSTALL_ROOT (empty in
real use; a test root otherwise, which also skips the root check). Paths
written into files or reported in the JSON stay logical. Every external
command goes through run().
"""
import argparse
from contextlib import closing
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time

EXIT_CODES = {'updated': 0, 'unchanged': 0, 'refused': 3, 'rolled_back': 5, 'rollback_failed': 6}

IMAGE_PATTERN = re.compile(r'[a-z0-9./-]+@sha256:[0-9a-f]{64}')
# X.Y.Z, or X.Y.Z-<pre-release> with semver pre-release identifiers.
VERSION_PATTERN = re.compile(r'(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?')
VTL_ID_PATTERN = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
BACKUP_NAME = re.compile(r'\d{8}-\d{6}-\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?')

# Logical paths (prefixed by the install root when read or written).
ETC_DIR = '/etc/vaultcenter-host'
COMPOSE_FILE = ETC_DIR + '/compose.json'
STORAGE_FILE = ETC_DIR + '/storage.json'
HOST_STATE = '/var/lib/vaultcenter-host'
MOUNT_OVERRIDE = HOST_STATE + '/mount.json'
BACKUPS_DIR = HOST_STATE + '/backups'
INSTALL_STATE = '/var/lib/vaultcenter-install'
MODULE_FINGERPRINT = INSTALL_STATE + '/mhvtl.sha256'
UPDATE_LOCK = INSTALL_STATE + '/update.lock'
IN_PROGRESS = HOST_STATE + '/update-in-progress.json'
VERIFIED = HOST_STATE + '/update-verified.json'
LIBEXEC_DIR = '/usr/local/libexec/vaultcenter-host'
UNIT_DIR = '/etc/systemd/system'
UNIT_GLOB = 'vaultcenter-storage-*.service'
LOADED_MODULE = '/sys/module/mhvtl'

# What install.py installs on the host, taken from the payload's host/.
HOST_CONTROLLERS = ('storage_controller.py', 'storage_paths.py', 'vtl_state.py', 'vtl_storage.py',
                    'storage_mounts.py')
HOST_UNITS = ('vaultcenter-storage-host.service', 'vaultcenter-storage-mounts.service')
HOST_SERVICE = 'vaultcenter-storage-host'
# The boot-time guard install.sh installs with the kernel module: it rebuilds
# and loads mhvtl when a kernel upgrade left the booted kernel without it.
MODULE_UNIT = 'vaultcenter-mhvtl.service'
MODULE_UNIT_LINK = UNIT_DIR + '/multi-user.target.wants/' + MODULE_UNIT

SERVICE = 'appliance'
HEALTH_URL = 'https://127.0.0.1:8443/health'
MAINTENANCE_HELPER = '/usr/local/libexec/cold-vtl-maintenance'
ACTOR = 'vaultcenter-update'
RECEIPT = 'isolation-qualified.json'
CATALOG = 'catalog.db'
FREE_SPACE_MARGIN = 200 * 1024 * 1024
UNIT_PATTERNS = ('vaultcenter-library-*', 'vaultcenter-drive-*', 'vaultcenter-target-*')
LEGACY_UNIT_PATTERNS = ('vtllibrary@*', 'vtltape@*', 'tgt.service')

# Timeouts in seconds. Stopping the appliance honours its 300 s grace period.
COMMAND_TIMEOUT = 60
STOP_TIMEOUT = 360
RECREATE_TIMEOUT = 420
TRANSITION_TIMEOUT = 600
DKMS_TIMEOUT = 1800

# Backup file names.
BACKUP_CATALOG = 'catalog.db'
BACKUP_ETC = 'etc-vaultcenter-host.tar'
BACKUP_HOST_FILES = 'host-files.tar'
BACKUP_IMAGE = 'image'
BACKUP_VERSION = 'version'
BACKUP_FINGERPRINT = 'mhvtl.sha256'
BACKUP_RECEIPT = RECEIPT
BACKUP_MODULE_CHANGE = 'module-change.json'


# ------------------------------------------------------------------ errors
class CommandError(Exception):
    """An external command failed, timed out or could not be started."""

    def __init__(self, argv, result=None, detail=None):
        self.argv = list(argv)
        self.result = result
        if detail is None and result is not None:
            lines = (result.stderr or result.stdout or '').strip().splitlines()
            detail = f'code {result.returncode}' + (f': {lines[-1][-300:]}' if lines else '')
        super().__init__(f'"{" ".join(self.argv[:3])}" failed ({detail})')


class Refused(Exception):
    """The update cannot start; nothing was modified (or it was put back)."""

    def __init__(self, *reasons, confirmable=False):
        super().__init__('; '.join(reasons))
        self.reasons = list(reasons)
        # True only when --yes alone would let the run through (see plan_maintenance).
        self.confirmable = confirmable


class StepFailed(Exception):
    """A modifying step (5 to 8) failed: the backup must be restored."""


class RollbackFailed(Exception):
    """The restoration itself failed at `step`; nothing more is attempted."""

    def __init__(self, step, error):
        super().__init__(f'{step}: {error}')
        self.step = step


class Interrupted(BaseException):
    """SIGTERM or SIGHUP received; handled like a failure of the current phase."""


# What a phase catches to refuse or roll back: errors, signals and Ctrl-C.
FAILURES = (Exception, Interrupted, KeyboardInterrupt)


# --------------------------------------------------------------- utilities
def run(argv, *, input=None, check=True, timeout=COMMAND_TIMEOUT):
    """Run one external command, without a shell, and return its CompletedProcess.

    Output is captured as text. With check (the default) a non-zero exit, a
    timeout or a missing program raises CommandError. Never pass a secret in
    argv: this script has none to pass.
    """
    argv = [str(item) for item in argv]
    try:
        result = subprocess.run(argv, input=input, capture_output=True, text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        raise CommandError(argv, detail=f'timed out after {timeout} s') from None
    except OSError as error:
        raise CommandError(argv, detail=str(error)) from None
    if check and result.returncode != 0:
        raise CommandError(argv, result)
    return result


def log(message):
    """Print one progress line for the operator (stderr, English).

    Never raises: a reader of stderr that went away (broken pipe, full disk,
    closed stream) must not change what the update does.
    """
    try:
        print(f'[update] {message}', file=sys.stderr, flush=True)
    except (OSError, ValueError):
        pass


def install_root():
    """The install root as a Path: VAULTCENTER_INSTALL_ROOT, or / in real use."""
    return Path(os.environ.get('VAULTCENTER_INSTALL_ROOT') or '/')


def on_host(root, logical):
    """The real location of a logical host path under the install root."""
    return Path(root) / str(logical).lstrip('/')


def logical_path(root, path):
    """The logical form of a path under the install root (for reports)."""
    return '/' + str(Path(path).relative_to(root)) if Path(root) != Path('/') else str(path)


def documentation():
    """Where the operator finds the installation guide: the copy shipped beside
    this script in the payload (…/vaultcenter/<version>/host/install.md), as a
    logical path; the repository path when this script is not run from a
    payload."""
    shipped = Path(__file__).resolve().with_name('install.md')
    if not shipped.is_file():
        return 'docs/install.md'
    try:
        return logical_path(install_root(), shipped)
    except ValueError:
        return str(shipped)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def parse_version(text):
    """'X.Y.Z' or 'X.Y.Z-<pre-release>' as a key that orders versions as semver
    does; ValueError otherwise.

    A pre-release is lower than its release; its dot-separated identifiers
    compare in turn, numeric ones as numbers and below the others, and a
    shorter list is lower than a longer one that starts with it.
    """
    match = VERSION_PATTERN.fullmatch(str(text or ''))
    if not match:
        raise ValueError(f'invalid version: {text!r}')
    numbers = tuple(int(part) for part in match.groups()[:3])
    if match.group(4) is None:
        return numbers + (1, ())
    identifiers = tuple((0, int(part), '') if part.isdigit() else (1, 0, part) for part in match.group(4).split('.'))
    return numbers + (0, identifiers)


def env_seconds(name, default):
    """A non-negative number of seconds from the environment (test seam)."""
    try:
        value = float(os.environ.get(name, default))
    except ValueError:
        return default
    return value if value >= 0 else default


def fsync_directory(path):
    descriptor = os.open(path, os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write(path, data, *, mode=0o600, uid=None, gid=None, mtime=None):
    """Replace `path` with `data` atomically (temporary file, fsync, rename).

    The new file gets `mode`, and `uid`/`gid` when running as root. Never
    leaves a partial file at `path`: either the old or the new content.
    """
    path = Path(path)
    descriptor, temporary = tempfile.mkstemp(prefix='.' + path.name + '.', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        if os.geteuid() == 0 and uid is not None:
            os.chown(temporary, uid, -1 if gid is None else gid)
        if mtime is not None:
            os.utime(temporary, (mtime, mtime))
        os.replace(temporary, path)
        fsync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def atomic_copy(source, destination, *, like=None):
    """Copy `source` onto `destination` atomically.

    Mode and owner come from `like` (an os.stat_result) when given, else from
    the source.
    """
    reference = like or os.stat(source)
    atomic_write(destination, Path(source).read_bytes(), mode=reference.st_mode & 0o7777,
                 uid=reference.st_uid, gid=reference.st_gid)


# ----------------------------------------------------------- installation
class Appliance:
    """What /etc/vaultcenter-host/storage.json says about the installation."""

    def __init__(self, root):
        self.root = Path(root)
        config = json.loads(on_host(root, STORAGE_FILE).read_text())
        self.container = config['container']
        self.state_dir = on_host(root, config['state_dir'])
        self.isolated = bool(config.get('isolated'))

    @property
    def catalog(self):
        return self.state_dir / CATALOG

    @property
    def receipt(self):
        return self.state_dir / RECEIPT


def compose_command(root):
    """`docker compose` on the installed compose.json (and the media override
    the non-isolated storage service writes, when present)."""
    command = ['docker', 'compose', '-f', str(on_host(root, COMPOSE_FILE))]
    override = on_host(root, MOUNT_OVERRIDE)
    if override.is_file():
        command += ['-f', str(override)]
    return command


def installed_image(root):
    return json.loads(on_host(root, COMPOSE_FILE).read_text())['services'][SERVICE]['image']


def api_version():
    """The version reported by the running API's /health; CommandError or
    ValueError when it does not answer properly."""
    # --noproxy: the API is on the loopback, never behind HTTPS_PROXY.
    answer = run(['curl', '-fsSk', '--noproxy', '*', '--max-time', '10', HEALTH_URL], timeout=20).stdout
    version = json.loads(answer).get('version')
    parse_version(version)
    return version


def container_health(container):
    result = run(['docker', 'inspect', '--format', '{{.State.Health.Status}}', container], check=False)
    return result.stdout.strip() if result.returncode == 0 else ''


def wait_until(check, description):
    """Call `check` until it returns a true value, within the health limit.

    The limit (default 300 s) and the interval (default 5 s) come from
    VAULTCENTER_UPDATE_HEALTH_TIMEOUT and VAULTCENTER_UPDATE_HEALTH_INTERVAL.
    `check` is called at least once. Raises StepFailed with `description`.
    """
    interval = env_seconds('VAULTCENTER_UPDATE_HEALTH_INTERVAL', 5)
    deadline = time.monotonic() + env_seconds('VAULTCENTER_UPDATE_HEALTH_TIMEOUT', 300)
    while True:
        value = check()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise StepFailed(description)
        time.sleep(interval)


# ---------------------------------------------------------------- catalogue
def open_catalog(path):
    """Open the SQLite catalogue strictly read-only, never creating side files.

    `mode=ro` on a WAL database whose -wal/-shm files are absent would create
    them (and a read-only connection cannot remove them). So:
    - both side files present (the API has the database open): mode=ro, which
      reads the WAL content too;
    - WAL mode and no side file: everything is in the main file, which is
      opened immutable (no lock, no side file);
    - -wal present without -shm: refused, it cannot be read safely;
    - rollback-journal mode: mode=ro.
    """
    path = Path(path)
    wal, shm = path.with_name(path.name + '-wal'), path.with_name(path.name + '-shm')
    with open(path, 'rb') as stream:
        header = stream.read(100)
    wal_mode = len(header) >= 20 and header[18] == 2 and header[19] == 2
    uri = f'file:{path}?mode=ro'
    if wal_mode and not wal.exists() and not shm.exists():
        uri += '&immutable=1'
    elif wal_mode and not shm.exists():
        raise sqlite3.OperationalError(f'{path.name}-wal present without {path.name}-shm: cannot be opened read-only')
    return sqlite3.connect(uri, uri=True, timeout=10)


def active_vtls(appliance):
    """The VTLs whose maintenance matters: activated ones in isolated mode, or
    the single library (None) otherwise."""
    if not appliance.isolated:
        return [None]
    with closing(open_catalog(appliance.catalog)) as db:
        rows = db.execute("SELECT v.id FROM vtls v JOIN vtl_provisioning p ON p.vtl_id = v.id "
                          "WHERE v.lifecycle = 'ready' AND p.phase = 'active' ORDER BY v.id").fetchall()
    vtls = [row[0] for row in rows]
    for vtl in vtls:
        if not VTL_ID_PATTERN.fullmatch(vtl):
            raise ValueError(f'invalid VTL identifier in the catalogue: {vtl!r}')
    return vtls


def blockers(root):
    """What forbids restarting the appliance now, as English sentences.

    Checks: jobs queued or running in catalog.db (read-only), active
    vaultcenter-worker-* units in the container, VTL storage neither ready nor
    unconfigured. A check
    that cannot be made is itself a blocker. Modifies nothing.
    """
    reasons = []
    appliance = Appliance(root)
    try:
        with closing(open_catalog(appliance.catalog)) as db:
            rows = db.execute("SELECT status, COUNT(*) FROM jobs WHERE status IN ('queued', 'running') "
                              "GROUP BY status ORDER BY status").fetchall()
        if rows:
            counts = ', '.join(f'{count} {"queued" if status == "queued" else "running"}' for status, count in rows)
            reasons.append(f'Unfinished jobs in the catalogue ({counts}): wait for them to end.')
    except (sqlite3.Error, OSError) as error:
        reasons.append(f'The catalogue cannot be read in read-only mode ({error}).')
    try:
        listing = run(['docker', 'exec', appliance.container, 'systemctl', 'list-units', '--no-legend', '--plain',
                       'vaultcenter-worker-*']).stdout
        active = [line.split()[0] for line in listing.splitlines()
                  if len(line.split()) >= 3 and line.split()[2] not in ('inactive', 'failed')]
        if active:
            reasons.append(f'VTL workers are active ({", ".join(active)}): wait for them to end.')
    except CommandError as error:
        reasons.append(f'The state of the VTL workers could not be checked ({error}).')
    for path in sorted(on_host(root, HOST_STATE + '/vtls').glob('*/storage.json')):
        try:
            phase = json.loads(path.read_text()).get('phase')
        except (OSError, ValueError):
            phase = 'unreadable'
        # A draft VTL (storage never configured) has nothing in progress.
        if phase not in ('ready', 'unconfigured'):
            reasons.append(f'The storage of VTL {path.parent.name} is not ready (phase "{phase}").')
    return reasons


def catalog_integrity(path):
    """The result of PRAGMA integrity_check on a backup copy ('ok' when sound)."""
    with closing(open_catalog(path)) as db:
        rows = db.execute('PRAGMA integrity_check').fetchall()
    return '\n'.join(str(row[0]) for row in rows)


def copy_catalog(source, destination):
    """Consistent copy of the live catalogue with the SQLite backup API.

    The source is opened read-only; the copy is turned into a single
    self-contained file (journal_mode=DELETE) with mode 0600.
    """
    with closing(open_catalog(source)) as origin, closing(sqlite3.connect(destination)) as copy:
        origin.backup(copy)
        copy.execute('PRAGMA journal_mode=DELETE').fetchall()
    os.chmod(destination, 0o600)


# ------------------------------------------------------------ receipts
def runtime_digest(payload):
    """sha256 of the new version's vtl_runtime.py, or None if not shipped."""
    path = Path(payload) / 'host' / 'vtl_runtime.py'
    return sha256_file(path) if path.is_file() else None


def read_receipt(path):
    """The receipt at `path` as a dict, or None when absent or unreadable."""
    try:
        value = json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def receipt_matches(path, digest):
    """The receipt at `path` qualifies the runtime `digest` (as vtl_runtime.py
    and cold-vtld require: same runtime_sha256 and isolation_verified true)."""
    value = read_receipt(path)
    return (value is not None and digest is not None and value.get('runtime_sha256') == digest
            and value.get('isolation_verified') is True)


def receipt_verified(path):
    """The receipt at `path` records a working qualification (isolation_verified true)."""
    value = read_receipt(path)
    return value is not None and value.get('isolation_verified') is True


def shipped_receipt(root, payload):
    """The payload's receipt when it qualifies the new runtime (installed in
    step 7 in every case), else None."""
    shipped = Path(payload) / 'host' / RECEIPT
    return shipped if receipt_matches(shipped, runtime_digest(payload)) else None


def receipt_ok(root, payload):
    """The update may proceed as far as the isolation receipt is concerned.

    False only when the appliance holds a verified receipt (a working
    qualification) for another runtime than the payload's host/vtl_runtime.py
    and the payload ships no receipt for the new runtime: the update would
    break workers that work today. A payload without vtl_runtime.py counts as
    another runtime.
    """
    installed = Appliance(root).receipt
    if not receipt_verified(installed):
        return True
    digest = runtime_digest(payload)
    return receipt_matches(installed, digest) or shipped_receipt(root, payload) is not None


def receipt_warning(root, payload):
    """After the update, the appliance will still have no valid isolation
    receipt: none verified is installed and the payload ships none for the new
    runtime (workers were already refused and stay refused)."""
    return not receipt_verified(Appliance(root).receipt) and shipped_receipt(root, payload) is None


# --------------------------------------------------------------- module
def dkms_version(fingerprint):
    """The DKMS version of an archive: vc- and 12 hex digits of its sha256."""
    return 'vc-' + fingerprint[:12]


def dkms_tree_args(root):
    """DKMS arguments that point at the test root; empty on a real machine."""
    if Path(root) == Path('/'):
        return []
    return ['--sourcetree', str(on_host(root, '/usr/src')), '--dkmstree', str(on_host(root, '/var/lib/dkms'))]


def dkms_state(root, version, kernel):
    """'installed' (for that kernel), 'added', or None for an unknown version."""
    output = run(['dkms', 'status', '-m', 'mhvtl', '-v', version, '-k', kernel, *dkms_tree_args(root)],
                 check=False).stdout
    if ': installed' in output:
        return 'installed'
    return 'added' if output.strip() else None


def dkms_kernels(root, version):
    """The kernels for which DKMS reports this mhvtl version installed."""
    output = run(['dkms', 'status', '-m', 'mhvtl', '-v', version, *dkms_tree_args(root)], check=False).stdout
    pattern = re.compile(r'\s*mhvtl[/,]\s*' + re.escape(version) + r',\s*([^,:\s]+),[^:]*:\s*installed')
    kernels = []
    for line in output.splitlines():
        match = pattern.match(line)
        if match and match.group(1) not in kernels:
            kernels.append(match.group(1))
    return kernels


def dkms_install(root, version, kernel):
    """Build and install one mhvtl version for one kernel. --force: two
    versions of the module carry the same MODULE_VERSION, and DKMS would
    otherwise decline to replace the installed file."""
    tree = dkms_tree_args(root)
    run(['dkms', 'build', '-m', 'mhvtl', '-v', version, '-k', kernel, *tree], timeout=DKMS_TIMEOUT)
    run(['dkms', 'install', '--force', '-m', 'mhvtl', '-v', version, '-k', kernel, *tree], timeout=DKMS_TIMEOUT)


def module_resolves(root, kernel):
    """modprobe would find an mhvtl module for that kernel at the next boot."""
    base = [] if Path(root) == Path('/') else ['-b', str(root)]
    return run(['modinfo', *base, '-k', kernel, 'mhvtl'], check=False).returncode == 0


def installed_fingerprint(root):
    path = on_host(root, MODULE_FINGERPRINT)
    try:
        value = path.read_text().strip()
    except OSError:
        return None
    return value if re.fullmatch(r'[0-9a-f]{64}', value) else None


def module_changed(root, payload):
    """The payload's mhvtl sources differ from the installed ones (or the
    installed fingerprint is unknown)."""
    return sha256_file(Path(payload) / 'mhvtl-source.tar.gz') != installed_fingerprint(root)


def kernel_release():
    return run(['uname', '-r']).stdout.strip()


def module_loaded(root):
    return on_host(root, LOADED_MODULE).exists()


# ------------------------------------------------------------ maintenance
class MaintenanceError(Exception):
    pass


def maintenance(container, vtl, action, *, confirm_idle=False):
    """Call cold-vtl-maintenance in the container for one VTL (None: the
    single library) and return its JSON answer; MaintenanceError otherwise.

    Transitions read their request on stdin, as the API sends it.
    """
    transition = action in ('enter', 'leave')
    argv = ['docker', 'exec']
    if transition:
        argv.append('-i')
    if vtl is not None:
        argv += ['-e', f'COLD_VTL_VTL_ID={vtl}']
    argv += [container, MAINTENANCE_HELPER, action]
    request = json.dumps({'actor': ACTOR, 'confirm_idle': confirm_idle}) if transition else None
    try:
        result = run(argv, input=request, check=False,
                     timeout=TRANSITION_TIMEOUT if transition else COMMAND_TIMEOUT)
    except CommandError as error:
        raise MaintenanceError(str(error)) from None
    try:
        body = json.loads(result.stdout)
    except ValueError:
        body = None
    if result.returncode != 0 or not isinstance(body, dict):
        detail = body.get('error') if isinstance(body, dict) else None
        raise MaintenanceError(detail or f'code {result.returncode}')
    return body


def vtl_name(vtl):
    return f'VTL {vtl}' if vtl else 'the library'


def vtl_title(vtl):
    """vtl_name with a capital first letter (and "VTL" left as it is)."""
    name = vtl_name(vtl)
    return name[0].upper() + name[1:]


def still_in(update, vtl, reason):
    """Record that a VTL an update put in maintenance is still there, and why
    (the JSON's still_in_maintenance). One record per VTL: the last one wins."""
    update.still = [record for record in update.still if record['vtl'] != vtl]
    update.still.append({'vtl': vtl, 'name': vtl_name(vtl), 'reason': reason})


def plan_maintenance(appliance, yes):
    """Decide, per active VTL, what the update does with its maintenance.

    Returns (to_enter, unguarded): to_enter is a list of (vtl, confirm_idle).
    A VTL whose guard cannot be asked (non-zero exit or configured false) is
    unguarded: it needs --yes and is neither entered nor left. A Veeam VTL's
    guard is local and cannot see a running Veeam job: it needs --yes too, and
    is entered with confirm_idle. A VTL already in maintenance is left alone.
    Raises Refused; modifies nothing.
    """
    to_enter, unguarded, reasons = [], [], []
    confirmable = True
    for vtl in active_vtls(appliance):
        try:
            status = maintenance(appliance.container, vtl, 'status')
        except MaintenanceError:
            status = None
        if status is None or status.get('configured') is False:
            unguarded.append(vtl)
            if not yes:
                reasons.append(f'No maintenance guard answers for {vtl_name(vtl)}: the script cannot know that no '
                               'backup is running. Check it, then run again with --yes.')
            continue
        phase = status.get('phase')
        if phase == 'maintenance':
            continue
        if phase != 'online':
            reasons.append(f'The maintenance of {vtl_name(vtl)} is in the "{phase}" state: complete it or '
                           'resume it before the update.')
            confirmable = False
            continue
        veeam = status.get('client') == 'veeam'
        if veeam and not yes:
            reasons.append(f'The maintenance guard of {vtl_name(vtl)} (Veeam) is local: VaultCenter cannot '
                           'see a running Veeam job. Check in Veeam that no job is using this library, then run '
                           'again with --yes: --yes asserts that no Veeam job is running.')
            continue
        to_enter.append((vtl, veeam))
    if reasons:
        raise Refused(*reasons, confirmable=confirmable)
    return to_enter, unguarded


def active_units(appliance):
    """The VTL units of the container whose active state is `active`.

    A unit listed inactive and a unit no longer listed are the same thing
    here: a recreated container may not list the units it never started.
    """
    patterns = UNIT_PATTERNS if appliance.isolated else LEGACY_UNIT_PATTERNS
    listing = run(['docker', 'exec', appliance.container, 'systemctl', 'list-units', '--all', '--no-legend',
                   '--plain', '--type=service', *patterns]).stdout
    return {parts[0] for parts in (line.split() for line in listing.splitlines())
            if len(parts) >= 3 and parts[2] == 'active'}


# ------------------------------------------------------------------ tars
def tar_members(root, logical_paths):
    """Every regular file and directory below the logical paths, as paths
    relative to the root (no __pycache__)."""
    members = []
    for logical in logical_paths:
        base = on_host(root, logical)
        if not base.exists():
            continue
        paths = [base] + (sorted(base.rglob('*')) if base.is_dir() and not base.is_symlink() else [])
        for path in paths:
            if '__pycache__' in path.parts:
                continue
            if path.is_symlink() or not (path.is_file() or path.is_dir()):
                raise OSError(f'unexpected file type: {logical_path(root, path)}')
            members.append(str(path.relative_to(root)))
    return members


def write_tar(destination, root, logical_paths):
    """Archive the logical paths (relative names) into `destination` (0600)
    and read the archive back to check it is complete."""
    members = tar_members(root, logical_paths)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'wb') as stream:
        with tarfile.open(fileobj=stream, mode='w', format=tarfile.PAX_FORMAT) as tar:
            for name in members:
                tar.add(Path(root) / name, arcname=name, recursive=False)
        stream.flush()
        os.fsync(stream.fileno())
    with tarfile.open(destination) as tar:
        names = [member.name for member in tar.getmembers()]
    if names != members:
        raise tarfile.TarError(f'incomplete archive: {Path(destination).name}')


def host_file_paths(root):
    """The logical host files that step 6 replaces."""
    directory = on_host(root, UNIT_DIR)
    units = sorted(directory.glob(UNIT_GLOB)) + sorted(directory.glob(MODULE_UNIT))
    return [LIBEXEC_DIR, *(f'{UNIT_DIR}/{unit.name}' for unit in units)]


def restore_tar(root, archive, scopes):
    """Put back every file of `archive`, then remove what is not in it.

    Each file is replaced atomically with its recorded mode, owner and mtime.
    `scopes` are (logical directory, glob) pairs: files there that the archive
    does not hold (added by the new version) are removed. Members must be
    plain files or directories with relative names.
    """
    root = Path(root)
    kept = set()
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            name = Path(member.name)
            if name.is_absolute() or '..' in name.parts or not (member.isfile() or member.isdir()):
                raise tarfile.TarError(f'entry refused in {Path(archive).name}: {member.name}')
            target = root / name
            kept.add(target)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                os.chmod(target, member.mode & 0o7777)
                if os.geteuid() == 0:
                    os.chown(target, member.uid, member.gid)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            data = tar.extractfile(member).read()
            atomic_write(target, data, mode=member.mode & 0o7777, uid=member.uid, gid=member.gid,
                         mtime=member.mtime)
    for directory, pattern in scopes:
        base = on_host(root, directory)
        if not base.is_dir():
            continue
        for path in sorted(base.glob(pattern), reverse=True):
            if path in kept or '__pycache__' in path.parts:
                continue
            if path.is_dir() and not path.is_symlink():
                if not any(path.iterdir()):
                    path.rmdir()
            else:
                path.unlink()


# ---------------------------------------------------------------- backup
def free_space_needed(appliance):
    """2 × the catalogue (with its WAL) + 200 MiB, in bytes."""
    size = appliance.catalog.stat().st_size
    wal = appliance.catalog.with_name(CATALOG + '-wal')
    if wal.exists():
        size += wal.stat().st_size
    return 2 * size + FREE_SPACE_MARGIN


def check_free_space(root, appliance):
    """Refused when the filesystem of the backups cannot hold the backup."""
    directory = on_host(root, BACKUPS_DIR)
    while not directory.exists():
        directory = directory.parent
    needed = free_space_needed(appliance)
    free = shutil.disk_usage(directory).free
    if free < needed:
        raise Refused(f'Not enough free space for the backup in {BACKUPS_DIR}: '
                      f'{needed // (1024 * 1024)} MiB needed, {free // (1024 * 1024)} MiB free.')


def backup(root, label):
    """Make a complete, verified backup and return its directory.

    Directory: /var/lib/vaultcenter-host/backups/<YYYYMMDD-HHMMSS>-<label>/
    (0700), holding catalog.db (SQLite backup API, integrity checked),
    etc-vaultcenter-host.tar, image (current digest), version (`label`),
    mhvtl.sha256, host-files.tar and the isolation receipt when one is
    installed. Any failure removes the partial directory and raises; the
    installation itself is never modified.
    """
    parse_version(label)
    appliance = Appliance(root)
    fingerprint = installed_fingerprint(root)
    if fingerprint is None:
        raise OSError(f'{MODULE_FINGERPRINT} missing or unreadable')
    backups = on_host(root, BACKUPS_DIR)
    backups.mkdir(mode=0o700, parents=True, exist_ok=True)
    target = backups / f'{time.strftime("%Y%m%d-%H%M%S")}-{label}'
    target.mkdir(mode=0o700)
    try:
        copy_catalog(appliance.catalog, target / BACKUP_CATALOG)
        integrity = catalog_integrity(target / BACKUP_CATALOG)
        if integrity != 'ok':
            raise sqlite3.DatabaseError(f'the catalogue copy failed the integrity check ({integrity.splitlines()[0][:200]})')
        write_tar(target / BACKUP_ETC, root, [ETC_DIR])
        write_tar(target / BACKUP_HOST_FILES, root, host_file_paths(root))
        atomic_write(target / BACKUP_IMAGE, (installed_image(root) + '\n').encode())
        atomic_write(target / BACKUP_VERSION, (label + '\n').encode())
        atomic_write(target / BACKUP_FINGERPRINT, (fingerprint + '\n').encode())
        if appliance.receipt.exists():
            atomic_copy(appliance.receipt, target / BACKUP_RECEIPT)
        fsync_directory(target)
    except BaseException:
        shutil.rmtree(target, ignore_errors=True)
        raise
    return target


def prune_backups(root, keep=3, protect=None):
    """Keep the `keep` most recent backups (by name), always including
    `protect`. Only directories named like a backup are ever removed."""
    directory = on_host(root, BACKUPS_DIR)
    if not directory.is_dir():
        return
    protect = Path(protect).resolve() if protect else None
    candidates = sorted((path for path in directory.iterdir()
                         if path.is_dir() and not path.is_symlink() and BACKUP_NAME.fullmatch(path.name)),
                        key=lambda path: path.name, reverse=True)
    others = [path for path in candidates if path.resolve() != protect]
    room = keep - (1 if protect and any(path.resolve() == protect for path in candidates) else 0)
    for path in others[max(room, 0):]:
        shutil.rmtree(path)


# ---------------------------------------------------------- container ops
def stop_container(root):
    run(compose_command(root) + ['stop', SERVICE], timeout=STOP_TIMEOUT)


def recreate_container(root):
    run(compose_command(root) + ['up', '-d', '--force-recreate', SERVICE], timeout=RECREATE_TIMEOUT)


def wait_healthy(container):
    wait_until(lambda: container_health(container) == 'healthy',
               f'container {container} did not become healthy in time')


def write_image(root, image):
    """Write `image` into compose.json atomically, keeping its mode and owner."""
    path = on_host(root, COMPOSE_FILE)
    config = json.loads(path.read_text())
    config['services'][SERVICE]['image'] = image
    info = path.stat()
    atomic_write(path, (json.dumps(config, indent=2) + '\n').encode(), mode=info.st_mode & 0o7777,
                 uid=info.st_uid, gid=info.st_gid)


# ------------------------------------------------------------------ steps
class Update:
    """The state of one run, shared by the steps."""

    def __init__(self, root, args):
        self.root = Path(root)
        self.payload = Path(args.payload or '.')
        self.image = args.image or ''
        self.version = args.version
        self.yes = args.yes
        self.resume = args.resume_rollback
        self.force = args.force_rollback
        self.appliance = None
        self.old_version = None
        self.kernel = None
        self.module = None          # (old dkms version, new dkms version, new fingerprint) when changed
        self.old_module = None      # DKMS version the finish removes (None: module unchanged)
        self.receipt = None         # payload receipt to install, if any
        self.entered = []           # {"vtl", "lease"} of each VTL this run put in maintenance
        self.left = False
        self.leave_ran = False      # the leave of the entered VTLs ran to its end
        self.still = []             # {"vtl", "name", "reason"}: entered VTLs still in maintenance at the end
        self.units = set()          # VTL units active after entering maintenance
        self.phase = 'preflight'    # preflight, modifying, rollback, finishing
        self.step = None            # modifying step or rollback stage in progress
        self.marker_step = None     # last modifying step begun (recorded in the marker)
        self.started = None
        self.rollback_stage = None
        self.marker_state = None    # after step 8: verified (renamed), deleted or stuck
        self.saved_mask = None      # signal mask to restore after a check of step 8
        self.backup = None
        self.reasons = []
        self.warnings = []
        self.confirmable = False    # a refusal that --yes alone would lift (reported to install.sh)


def step_module(update):
    """Step 5: build and load the payload's mhvtl through DKMS, if it changed.

    Stops the container first. The new version is installed for the running
    kernel, and DKMS must report it installed (otherwise the step fails), then
    for every other kernel that had the previous version (a failure there is
    a warning). The previous DKMS version stays known to DKMS: it is the
    rollback target. A module that stays in use fails the step: it is never
    removed by force. The backup records the change in progress so that
    restore() knows what to undo.
    """
    if update.module is None:
        return
    old, new, fingerprint = update.module
    root, kernel = update.root, update.kernel
    marker = update.backup / BACKUP_MODULE_CHANGE
    # A kernel installed but not booted yet must not be left without a module
    # once the finish removes the previous version.
    others = [other for other in dkms_kernels(root, old) if other != kernel]
    change = {'old_version': old, 'new_version': new, 'kernel': kernel, 'other_kernels': others, 'reloaded': False}
    atomic_write(marker, json.dumps(change).encode())
    log('stopping the appliance to change the mhvtl module (up to 5 minutes)')
    stop_container(root)
    host = update.payload / 'host'
    log(f'preparing mhvtl module {new} for kernel {kernel}')
    prepared = run(['bash', host / 'prepare-dkms-source.sh', update.payload / 'mhvtl-source.tar.gz', str(root),
                    host / 'dkms.conf']).stdout.strip()
    if prepared != new:
        raise StepFailed(f'the prepared DKMS sources have version {prepared or "unknown"} instead of {new}')
    tree = dkms_tree_args(root)
    if dkms_state(root, new, kernel) is None:
        run(['dkms', 'add', '-m', 'mhvtl', '-v', new, *tree], timeout=DKMS_TIMEOUT)
    dkms_install(root, new, kernel)
    if dkms_state(root, new, kernel) != 'installed':
        raise StepFailed(f'DKMS did not install mhvtl module {new} for kernel {kernel}')
    for other in others:
        log(f'mhvtl module {new} for kernel {other}, installed but not booted')
        try:
            dkms_install(root, new, other)
            if dkms_state(root, new, other) != 'installed':
                raise CommandError(['dkms', 'install', '--force'], detail='the module is not installed')
        except CommandError as error:
            update.warnings.append(f'mhvtl module {new} could not be installed for kernel {other}, '
                                   f'installed but not booted ({error}). Before rebooting on this kernel, run: '
                                   f'dkms install --force -m mhvtl -v {new} -k {other}')
    if module_loaded(root):
        result = run(['modprobe', '-r', 'mhvtl'], check=False)
        if result.returncode != 0:
            raise StepFailed('the mhvtl module is still in use and cannot be unloaded; it is never '
                             'removed by force')
    change['reloaded'] = True
    atomic_write(marker, json.dumps(change).encode())
    run(['modprobe', 'mhvtl', 'opts=0'])
    atomic_write(on_host(root, MODULE_FINGERPRINT), (fingerprint + '\n').encode(), mode=0o644)


def step_host_files(update):
    """Step 6: replace the host controllers and units (the boot-time guard
    of the module included, enabled here for an appliance installed before it
    existed), reload, restart the host storage service."""
    host = update.payload / 'host'
    libexec = on_host(update.root, LIBEXEC_DIR)
    libexec.mkdir(parents=True, exist_ok=True)
    for name in HOST_CONTROLLERS:
        atomic_copy(host / name, libexec / name)
    for name in (*HOST_UNITS, MODULE_UNIT):
        atomic_copy(host / name, on_host(update.root, UNIT_DIR) / name)
    run(['systemctl', 'daemon-reload'])
    run(['systemctl', 'enable', MODULE_UNIT])
    run(['systemctl', 'restart', HOST_SERVICE])


def step_container(update):
    """Step 7: install the payload's receipt when it qualifies the new
    runtime, write the new digest, recreate the appliance."""
    if update.receipt is not None:
        receipt = update.appliance.receipt
        atomic_write(receipt, update.receipt.read_bytes(), mode=0o600)
    write_image(update.root, update.image)
    log('recreating the appliance with the new image')
    recreate_container(update.root)


def step_verify(update):
    """Step 8: healthy container, /health reports the new version, and the
    same VTL units active as after entering maintenance (no more, no fewer).

    Its last act records the verification (record_verification): the step has
    not passed until then. Each units check, which may be the last one, runs
    with SIGINT, SIGTERM and SIGHUP held, and nothing that can raise sits
    between a check that passes and the rename. On success the signals stay
    held for perform() to release; a signal held during a check that did not
    pass is delivered here and fails the step.
    """
    container = update.appliance.container
    log('waiting for the appliance to be healthy (up to 5 minutes)')
    wait_healthy(container)
    try:
        version = api_version()
    except (CommandError, ValueError) as error:
        raise StepFailed(f'/health does not answer properly ({error})') from None
    if version != update.version:
        raise StepFailed(f'/health reports version {version} instead of {update.version}')
    source, target = on_host(update.root, IN_PROGRESS), on_host(update.root, VERIFIED)
    interval = env_seconds('VAULTCENTER_UPDATE_HEALTH_INTERVAL', 5)
    deadline = time.monotonic() + env_seconds('VAULTCENTER_UPDATE_HEALTH_TIMEOUT', 300)
    while True:
        try:
            block_signals(update)
            if same_units(update):
                record_verification(update, source, target)
                return
        except BaseException:
            unblock_signals(update)
            raise
        held = unblock_signals(update)
        if held is not None:
            raise Interrupted(str(held))
        if time.monotonic() >= deadline:
            raise StepFailed('the VTL units did not return to their state from before the update')
        time.sleep(interval)


def same_units(update):
    """The VTL units active now are those active after entering maintenance."""
    try:
        return active_units(update.appliance) == update.units
    except CommandError:
        return False


def record_verification(update, source, target):
    """The last act of step 8: rename update-in-progress.json to
    update-verified.json and fsync the directory. Called with signals held.

    The rename is what marks the update verified: it needs no free space and
    is the single point of failure. If it fails, the in-progress marker is
    removed instead, so that a verified update never keeps a marker that
    allows a rollback; if that fails too, the marker is stuck and the caller
    says not to roll back. Never raises an Exception. Sets update.phase to
    'finishing' and update.marker_state to 'verified', 'deleted' or 'stuck'.
    """
    # Every check passed: from here nothing may restore the backup, whatever
    # becomes of the rename (an assignment cannot fail; it comes first so that
    # no outcome of the lines below can be taken for a failed step).
    update.phase = 'finishing'
    try:
        os.rename(source, target)
        update.marker_state = 'verified'
    except Exception:
        try:
            try:
                os.unlink(source)
            except FileNotFoundError:
                pass
            update.marker_state = 'deleted'
        except Exception:
            update.marker_state = 'stuck'
            return
    try:
        fsync_directory(target.parent)
    except Exception:
        pass    # the rename stands; only its durability across a power loss is not confirmed


STEPS = (('kernel module', step_module), ('host files', step_host_files),
         ('container', step_container), ('verification', step_verify))


# ------------------------------------------------------------------ restore
def restore(root, backup, on_stage=None):
    """Put the installation back as the backup recorded it, in reverse order.

    Stops the container; restores /etc/vaultcenter-host (compose.json
    included); restores catalog.db (stale -wal/-shm/-journal removed FIRST, so
    that no journal of the new version is ever applied to the restored file)
    and the isolation receipt; restores the host files and restarts the host
    service; puts the previous DKMS module back if it was changed; recreates
    the container with the old digest and checks /health reports the old
    version. Raises RollbackFailed(step, error) at the first failure (an
    interruption included) and attempts nothing more. Never touches
    maintenance. `on_stage` is told the name of each stage as it starts.
    """
    root, backup = Path(root), Path(backup)

    def stage(name, action):
        try:
            if on_stage:
                on_stage(name)
            log(f'rollback: {name}')
            action()
        except FAILURES as error:
            raise RollbackFailed(name, error) from error

    stage('stopping the container', lambda: stop_container(root))
    stage('host configuration', lambda: restore_tar(root, backup / BACKUP_ETC, [(ETC_DIR, '**/*')]))
    stage('catalogue', lambda: restore_catalog(Appliance(root), backup))
    stage('host files', lambda: restore_host_files(root, backup))
    if (backup / BACKUP_MODULE_CHANGE).exists():
        stage('kernel module', lambda: restore_module(root, backup))
    stage('restarting the container', lambda: recreate_container(root))
    stage('health check', lambda: check_restored(Appliance(root), backup))


def restore_catalog(appliance, backup):
    """Replace catalog.db by the backup copy, and the receipt as it was."""
    target = appliance.catalog
    reference = target.stat() if target.exists() else (backup / BACKUP_CATALOG).stat()
    for suffix in ('-wal', '-shm', '-journal'):
        target.with_name(CATALOG + suffix).unlink(missing_ok=True)
    atomic_copy(backup / BACKUP_CATALOG, target, like=reference)
    saved = backup / BACKUP_RECEIPT
    if saved.exists():
        atomic_copy(saved, appliance.receipt)
    else:
        appliance.receipt.unlink(missing_ok=True)


def restore_host_files(root, backup):
    restore_tar(root, backup / BACKUP_HOST_FILES,
                [(LIBEXEC_DIR, '**/*'), (UNIT_DIR, UNIT_GLOB), (UNIT_DIR, MODULE_UNIT)])
    if not on_host(root, UNIT_DIR + '/' + MODULE_UNIT).exists():
        # The update had installed and enabled a unit the backup does not hold.
        on_host(root, MODULE_UNIT_LINK).unlink(missing_ok=True)
    run(['systemctl', 'daemon-reload'])
    run(['systemctl', 'restart', HOST_SERVICE])


def restore_module(root, backup):
    """Reinstall and, if it had been unloaded, reload the previous DKMS
    version; drop the new one so that DKMS never rebuilds it. The other
    kernels that had the previous version get it back too; a failure there is
    reported and does not fail the restoration."""
    change = json.loads((backup / BACKUP_MODULE_CHANGE).read_text())
    old, new, kernel = change['old_version'], change['new_version'], change['kernel']
    tree = dkms_tree_args(root)
    if change.get('reloaded') and module_loaded(root):
        run(['modprobe', '-r', 'mhvtl'])
    if dkms_state(root, new, kernel) is not None:
        run(['dkms', 'remove', '-m', 'mhvtl', '-v', new, '--all', *tree], timeout=DKMS_TIMEOUT)
    if dkms_state(root, old, kernel) != 'installed':
        run(['dkms', 'install', '--force', '-m', 'mhvtl', '-v', old, '-k', kernel, *tree], timeout=DKMS_TIMEOUT)
    for other in change.get('other_kernels') or []:
        try:
            if dkms_state(root, old, other) != 'installed':
                run(['dkms', 'install', '--force', '-m', 'mhvtl', '-v', old, '-k', other, *tree],
                    timeout=DKMS_TIMEOUT)
        except CommandError as error:
            log(f'mhvtl module {old} could not be put back for kernel {other} ({error}): before rebooting '
                f'on this kernel, run dkms install --force -m mhvtl -v {old} -k {other}')
    if not module_loaded(root):
        run(['modprobe', 'mhvtl', 'opts=0'])
    atomic_write(on_host(root, MODULE_FINGERPRINT), (backup / BACKUP_FINGERPRINT).read_bytes(), mode=0o644)


def check_restored(appliance, backup):
    old_version = (backup / BACKUP_VERSION).read_text().strip()
    wait_healthy(appliance.container)
    version = api_version()
    if version != old_version:
        raise ValueError(f'/health reports version {version} instead of {old_version}')


# --------------------------------------------------------------------- run
def leave_recorded(update, container, entries, *, same_run):
    """Take out of maintenance the VTLs an update entered, and only those.

    `entries` are {"vtl", "lease"} records of this run's `enter` answers (or
    of the run that recorded them in the marker). Each VTL's status is read
    first: an online VTL is skipped; a VTL is left only when it is in
    maintenance under the recorded lease. A null lease is trusted only within
    the run that entered the VTL (`same_run`). A VTL whose maintenance was
    taken over since is skipped with a warning, which is not a failure.
    Returns (remaining, taken_over): the entries still to take out (failures,
    interruption), and those left as they are because someone else holds
    their maintenance now. Both kinds are recorded as still in maintenance.
    """
    remaining, taken_over = [], []
    for position, entry in enumerate(entries):
        vtl, lease = entry['vtl'], entry.get('lease')
        try:
            status = maintenance(container, vtl, 'status')
            phase = status.get('phase')
            if phase == 'online':
                continue
            if phase != 'maintenance' or (lease is None and not same_run) or (
                    lease is not None and status.get('lease') != lease):
                taken_over.append(entry)
                still_in(update, vtl, 'its maintenance has since been taken over by someone else; the update '
                                      'leaves it as it is')
                update.warnings.append(f'{vtl_title(vtl)}: its maintenance has since been taken over by '
                                       'someone else; the update leaves it as it is.')
                continue
            log(f'taking {vtl_name(vtl)} out of maintenance')
            maintenance(container, vtl, 'leave')
        except (Interrupted, KeyboardInterrupt) as error:
            remaining += entries[position:]
            for later in entries[position:]:
                still_in(update, later['vtl'], f'interrupted while leaving maintenance ({error or "Ctrl-C"})')
            update.reasons.append(f'Interrupted while leaving maintenance ({error or "Ctrl-C"}): '
                                  f'{", ".join(vtl_name(e["vtl"]) for e in entries[position:])} remain in '
                                  'maintenance.')
            break
        except Exception as error:
            remaining.append(entry)
            still_in(update, vtl, f'it could not leave maintenance ({error})')
            update.reasons.append(f'{vtl_title(vtl)} could not leave maintenance ({error}).')
    return remaining, taken_over


def leave_entered(update):
    """Leave maintenance for the VTLs this run entered (same-run rules of
    leave_recorded). Sets update.left, true only when none of them is still
    in maintenance for any reason; returns the entries still to take out."""
    remaining, taken_over = leave_recorded(update, update.appliance.container, update.entered, same_run=True)
    update.left = bool(update.entered) and not remaining and not taken_over
    update.leave_ran = True
    return remaining


# ----------------------------------------------------------------- markers
# update-in-progress.json: an update that has not passed step 8 (a rollback
# is possible). update-verified.json: the same file renamed by the last act
# of step 8 (never rolled back; its finish may remain to do). The name alone
# tells which: nothing written inside either file says "verified".
class MarkerUnreadable(Refused):
    """A marker cannot be examined or read: never rewritten, never rolled
    back from, never finished."""

    def __init__(self, logical):
        state = ('A previous verified update did not complete its finalisation: do not restore its '
                 'backup' if logical == VERIFIED else 'A previous update did not finish')
        super().__init__(f'The marker {logical} is unreadable: its content could not be read. {state}; the '
                         f'file has not been changed. Examine it by hand (procedure in {documentation()}) before '
                         'any new update.')


def marker_exists(root, logical):
    """Whether the marker file is there. A file that cannot be examined
    (any error other than its absence) raises MarkerUnreadable: it is never
    taken for "no marker"."""
    try:
        os.lstat(on_host(root, logical))
    except FileNotFoundError:
        return False
    except OSError:
        raise MarkerUnreadable(logical) from None
    return True


def read_marker(root, logical):
    """The marker as a dict, or None when there is none. Raises
    MarkerUnreadable when the file exists but cannot be read as a marker."""
    if not marker_exists(root, logical):
        return None
    try:
        value = json.loads(on_host(root, logical).read_text())
    except (OSError, ValueError):
        raise MarkerUnreadable(logical) from None
    if not isinstance(value, dict):
        raise MarkerUnreadable(logical)
    return value


def store_marker(root, logical, content):
    """Write a marker atomically, with fsync. `content` is complete: the
    file on disk is never merged into."""
    path = on_host(root, logical)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    atomic_write(path, (json.dumps(content, ensure_ascii=False, indent=2) + '\n').encode())


def remove_marker(root, logical):
    path = on_host(root, logical)
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    fsync_directory(path.parent)


def marker_content(update, entered=None):
    """What a marker records, built from memory: the backup, the versions,
    the target image, the last modifying step begun, and what the finish
    needs (the VTLs this update entered with their leases, the previous DKMS
    version to remove). `entered` replaces the full list once some are out."""
    content = {'backup': logical_path(update.root, update.backup), 'from': update.old_version,
               'to': update.version, 'image': update.image, 'started': update.started,
               'step': update.marker_step, 'entered': update.entered if entered is None else entered,
               'old_module': update.old_module}
    if update.rollback_stage:
        content['rollback_failed'] = update.rollback_stage
    return content


def write_in_progress(update):
    """Write update-in-progress.json for this run (step 8 has not passed)."""
    if update.started is None:
        update.started = time.strftime('%Y-%m-%dT%H:%M:%S%z')
    store_marker(update.root, IN_PROGRESS, marker_content(update))


def dkms_known(root, version):
    """DKMS knows this mhvtl version (added, built or installed)."""
    result = run(['dkms', 'status', '-m', 'mhvtl', '-v', version, *dkms_tree_args(root)], check=False)
    return bool(result.stdout.strip())


DONT_ROLL_BACK = ('The update is verified, but the file {path} could be neither renamed nor deleted: '
                  'do not use --resume-rollback (it would restore the old catalogue and lose what has been '
                  'written since); delete this file by hand.')


def entry_list(marker):
    """The marker's `entered` records, validated."""
    entries = []
    for entry in marker.get('entered') or []:
        if not isinstance(entry, dict):
            raise Refused(f'Invalid marker {VERIFIED} ({entry!r}): check it by hand.')
        vtl, lease = entry.get('vtl'), entry.get('lease')
        if (vtl is not None and not VTL_ID_PATTERN.fullmatch(str(vtl))) or not (lease is None or isinstance(lease, str)):
            raise Refused(f'Invalid marker {VERIFIED} (VTL {vtl!r}): check it by hand.')
        entries.append({'vtl': vtl, 'lease': lease})
    return entries


def complete_finish(update, marker):
    """Finish an earlier verified update whose run stopped during its finish:
    leave maintenance for its VTLs under the recorded leases (later-run
    rules), drop the previous module, prune, remove the verified marker (and
    an in-progress marker found beside it, which means nothing then). Raises
    Refused, keeping the verified marker, when a VTL could not be taken out."""
    root = update.root
    entries = entry_list(marker)
    appliance = Appliance(root)
    remaining, _ = leave_recorded(update, appliance.container, entries, same_run=False)
    if remaining:
        try:
            store_marker(root, VERIFIED, {**marker, 'entered': remaining})
        except OSError:
            pass    # the file still lists every VTL; the next run skips those already out
        raise Refused(f'A verified update to {marker.get("to")} is not finalised.', *update.reasons,
                      'The finalisation will be resumed at the next run; otherwise, take these VTLs out of '
                      'maintenance in the console.')
    old = marker.get('old_module')
    if old:
        try:
            if dkms_known(root, old):
                run(['dkms', 'remove', '-m', 'mhvtl', '-v', old, '--all', *dkms_tree_args(root)],
                    timeout=DKMS_TIMEOUT)
        except CommandError as error:
            update.warnings.append(f'The old version {old} of the module could not be removed from DKMS ({error}).')
        fingerprint = installed_fingerprint(root)
        if fingerprint:
            try:
                kernel = kernel_release()
            except CommandError:
                kernel = None
            if kernel:
                secure_module(update, dkms_version(fingerprint), kernel)
    backup_path = marker.get('backup')
    prune_backups(root, keep=3, protect=on_host(root, backup_path) if backup_path else None)
    remove_marker(root, IN_PROGRESS)
    remove_marker(root, VERIFIED)
    update.warnings.append(f'A previous verified update (to {marker.get("to")}) has been finalised: '
                           'its VTLs left maintenance and the cleanup was done.')


def target_health(root, marker, *, wait):
    """What the appliance's own state says of the recorded update.

    'other': compose.json does not name the recorded target image (or the
    marker records none), or the healthy appliance reports another version:
    the update did not go through. 'healthy': it names it, the container is
    healthy and /health reports the target version: the update went through.
    'unhealthy': it names it and the container reports itself unhealthy.
    'unknown': it names it and nothing more could be established (container
    starting or stopped, probes failing). With `wait`, 'unknown' is concluded
    only after the health limit of wait_until (a container that has just
    started is "starting" for a while). Runs read-only commands only.
    """
    try:
        image, version = marker.get('image'), marker.get('to')
        if not image or not version or installed_image(root) != image:
            return 'other'
        container = Appliance(root).container
    except (OSError, ValueError, KeyError, TypeError):
        return 'other'
    interval = env_seconds('VAULTCENTER_UPDATE_HEALTH_INTERVAL', 5)
    deadline = time.monotonic() + env_seconds('VAULTCENTER_UPDATE_HEALTH_TIMEOUT', 300)
    while True:
        try:
            health = container_health(container)
            if health == 'unhealthy':
                return 'unhealthy'
            if health == 'healthy':
                return 'healthy' if api_version() == version else 'other'
        except (CommandError, ValueError):
            pass    # a probe that fails proves nothing: try again
        if not wait or time.monotonic() >= deadline:
            return 'unknown'
        time.sleep(interval)


def runs_target_version(root, marker):
    """The appliance shows, now, that the recorded update went through (one
    probe, no waiting)."""
    return target_health(root, marker, wait=False) == 'healthy'


FORCE_ROLLBACK_EFFECT = ('it stops the appliance, restores the backup (catalogue, configuration, host files, '
                         'kernel module) and puts the previous version back; everything the appliance has written '
                         'since the update is lost')


def unfinished_refusal(root, marker):
    """Why a normal run refuses while update-in-progress.json exists. The
    rollback is offered only when the appliance does not show that the
    update went through."""
    backup = marker.get('backup') or 'unknown'
    failed = marker.get('rollback_failed')
    earlier = f' A previous rollback failed at step "{failed}".' if failed else ''
    if runs_target_version(root, marker):
        # Never "delete the marker": the update was not verified (its last
        # step failed or never ended), and deleting the file would hide it.
        return Refused(f'A previous update to {marker.get("to")} was not recorded as finished '
                       f'(file {IN_PROGRESS}, step "{marker.get("step")}"), but the appliance runs this '
                       'version, in good health: do not use --resume-rollback without weighing its effect.'
                       f'{earlier} Nothing has been changed by this run. --resume-rollback alone is refused in this '
                       f'state. With --resume-rollback --force-rollback, the rollback takes place: '
                       f'{FORCE_ROLLBACK_EFFECT} (backup {backup}). To keep the current version, follow '
                       f'"Keeping an unrecorded update" in {documentation()}.')
    script = Path(__file__).resolve()
    try:
        script = logical_path(install_root(), script)
    except ValueError:
        pass
    return Refused(f'A previous update did not finish (step "{marker.get("step")}", backup '
                   f'{backup}).{earlier} Nothing has been changed by this run: run again with --resume-rollback '
                   'to restore this backup (adding --force-rollback if the health of the appliance cannot '
                   f'be determined), or follow the manual procedure in {documentation()}. Without network access, the '
                   f'rollback can be started directly: python3 {script} --resume-rollback')


def check_arguments(update):
    """Argument and privilege checks; reads nothing of the installation."""
    if not IMAGE_PATTERN.fullmatch(update.image):
        raise Refused('The image must be designated by its digest (name@sha256:<64 hexadecimal digits>).')
    try:
        parse_version(update.version)
    except ValueError:
        raise Refused(f'Invalid target version: {update.version!r} (expected X.Y.Z or X.Y.Z-<pre-release>).') from None
    if not os.environ.get('VAULTCENTER_INSTALL_ROOT') and os.geteuid() != 0:
        raise Refused('Run the update as root on the host of the appliance.')


def already_installed(update):
    """Whether the image is already the installed one (runs no command)."""
    root = update.root
    if not on_host(root, COMPOSE_FILE).is_file():
        raise Refused(f'{COMPOSE_FILE} is missing: no installed appliance to update.')
    return installed_image(root) == update.image


def preflight(update):
    """Every other refusal, before anything is modified: payload, installed
    version, receipt, blockers, rollback target of the module."""
    root = update.root
    host = update.payload / 'host'
    needed = [host / name for name in (*HOST_CONTROLLERS, *HOST_UNITS, MODULE_UNIT, 'prepare-dkms-source.sh',
                                       'dkms.conf')]
    missing = [path.name for path in [update.payload / 'mhvtl-source.tar.gz', *needed] if not path.is_file()]
    if missing:
        raise Refused(f'Files missing from the payload {update.payload}: {", ".join(missing)}.')
    update.appliance = Appliance(root)
    try:
        update.old_version = api_version()
    except (CommandError, ValueError) as error:
        raise Refused(f'The installed version could not be read from /health ({error}): the appliance must be '
                      'started and healthy.') from None
    if parse_version(update.version) < parse_version(update.old_version):
        raise Refused(f'The target version {update.version} is older than the installed version '
                      f'{update.old_version}: going back to an older version is not done by an update.')
    if not receipt_ok(root, update.payload):
        raise Refused('The installed isolation receipt does not match the vtl_runtime.py of the new version and '
                      'the new version ships none: the workers would no longer start.')
    if receipt_warning(root, update.payload):
        update.warnings.append('This appliance has no valid isolation receipt: the workers (S3 exports) '
                               'stay refused until a receipt is installed, and the update does not change '
                               'that.')
    update.receipt = shipped_receipt(root, update.payload)
    reasons = blockers(root)
    if reasons:
        raise Refused(*reasons)
    if module_changed(root, update.payload):
        update.kernel = kernel_release()
        previous = installed_fingerprint(root)
        old = dkms_version(previous) if previous else None
        if old is None or dkms_state(root, old, update.kernel) != 'installed':
            raise Refused(f'The installed mhvtl module ({old or "unknown version"}) is not registered in DKMS '
                          f'for kernel {update.kernel}: it could not be put back in case of failure.')
        fingerprint = sha256_file(update.payload / 'mhvtl-source.tar.gz')
        update.module = (old, dkms_version(fingerprint), fingerprint)
        update.old_module = old


def enter_maintenance(update):
    """Enter maintenance where needed, capture the VTL units, re-check the
    blockers, and make the backup. On any failure, leave what was entered
    and raise Refused (the partial backup is already removed)."""
    to_enter, unguarded = plan_maintenance(update.appliance, update.yes)
    for vtl in unguarded:
        log(f'warning: {vtl_name(vtl)} has no maintenance guard (confirmed by --yes)')
    check_free_space(update.root, update.appliance)
    try:
        for vtl, confirm_idle in to_enter:
            log(f'putting {vtl_name(vtl)} into maintenance')
            answer = maintenance(update.appliance.container, vtl, 'enter', confirm_idle=confirm_idle)
            lease = answer.get('lease')
            update.entered.append({'vtl': vtl, 'lease': lease if isinstance(lease, str) else None})
        reasons = blockers(update.root)
        if reasons:
            raise Refused(*reasons)
        update.units = active_units(update.appliance)
        log('backing up the catalogue, the configuration and the host files')
        update.backup = backup(update.root, update.old_version)
        try:
            write_in_progress(update)
        except FAILURES:
            on_host(update.root, IN_PROGRESS).unlink(missing_ok=True)
            shutil.rmtree(update.backup, ignore_errors=True)
            update.backup = None
            raise
        update.phase = 'modifying'
    except FAILURES as error:
        reasons = error.reasons if isinstance(error, Refused) else [f'The preparation failed: {error}']
        leave_entered(update)
        raise Refused(*reasons, *update.reasons) from error


def modify(update):
    """Steps 5 to 8; any failure (an interruption included) restores the
    backup. Returns None on success, else the rollback outcome. Once step 8
    has recorded the verification, nothing is ever rolled back."""
    for name, step in STEPS:
        try:
            update.step = update.marker_step = name
            write_in_progress(update)
            log(f'step: {name}')
            step(update)
        except FAILURES as error:
            if update.phase == 'finishing':
                raise
            return roll_back(update, name, error)
    return None


BLOCKED_SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)


def block_signals(update):
    """Hold SIGINT, SIGTERM and SIGHUP from before a check of step 8 until
    the verification is recorded (or the check did not pass); the only
    external command run meanwhile is that check."""
    # The mask to restore is recorded before anything is blocked: a handler that
    # raises as soon as the mask is set must not leave the signals held (for
    # the rollback, and for every child process it starts) with nothing to
    # restore.
    update.saved_mask = signal.pthread_sigmask(signal.SIG_BLOCK, [])
    signal.pthread_sigmask(signal.SIG_BLOCK, BLOCKED_SIGNALS)


def unblock_signals(update):
    """Restore the signal mask. A signal held meanwhile is delivered now; the
    interruption it raises is returned (None when none came)."""
    if update.saved_mask is None:
        return None
    mask, update.saved_mask = update.saved_mask, None
    try:
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        time.sleep(0)
    except (Interrupted, KeyboardInterrupt) as error:
        return error or 'Ctrl-C'
    return None


def ignore_signals():
    """From now on, an interruption would do more harm than a slow end."""
    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, signal.SIG_IGN)


ROLLED_BACK_REASON = 'the rollback leaves it in maintenance: taking it out is the operator’s decision'


def record_after_rollback(update, resumed):
    """Which VTLs the update had entered are still in maintenance once the
    previous version runs again. Within the run that entered them, all of
    them (nothing took them out). After a resumed rollback their state is
    read: the operator, or the finish of a verified update, may have taken
    them out since. Read-only; never raises."""
    for entry in update.entered:
        vtl = entry.get('vtl') if isinstance(entry, dict) else None
        if not isinstance(entry, dict) or (vtl is not None and not VTL_ID_PATTERN.fullmatch(str(vtl))):
            continue
        if not resumed:
            still_in(update, vtl, ROLLED_BACK_REASON)
            continue
        try:
            phase = maintenance(Appliance(update.root).container, vtl, 'status').get('phase')
        except FAILURES as error:
            still_in(update, vtl, f'its maintenance state could not be read after the rollback ({error}): '
                                  'check it in the console')
            continue
        if phase != 'online':
            still_in(update, vtl, ROLLED_BACK_REASON)


def run_restore(update, resumed=False):
    """restore() from update.backup, then the outcome: ('rolled_back', None)
    with the marker removed, or ('rollback_failed', stage) with it kept.
    `resumed`: the rollback of an earlier run (--resume-rollback)."""
    update.phase = 'rollback'

    def on_stage(name):
        update.step = name
    try:
        restore(update.root, update.backup, on_stage=on_stage)
    except FAILURES as failure:
        stage = failure.step if isinstance(failure, RollbackFailed) else update.step or 'unknown'
        update.reasons.append(f'The rollback failed ({failure}). Nothing else was attempted: follow the '
                              f'manual procedure in {documentation()} with this backup.')
        update.rollback_stage = stage
        for entry in update.entered:
            if isinstance(entry, dict):
                still_in(update, entry.get('vtl'), 'the rollback failed: its state must be checked')
        try:
            write_in_progress(update)
        except FAILURES:
            pass
        return 'rollback_failed', stage
    record_after_rollback(update, resumed)
    if update.still:
        names = ', '.join(record['name'] for record in update.still)
        update.reasons.append(f'The previous version is restored. Still in maintenance: {names}; taking them '
                              'out is the operator’s decision.')
    else:
        update.reasons.append('The previous version is restored. No VTL put in maintenance by the update has '
                              'stayed there.')
    try:
        remove_marker(update.root, IN_PROGRESS)
    except FAILURES as error:
        update.reasons.append(f'The marker {IN_PROGRESS} could not be removed ({error}): delete it by hand.')
    return 'rolled_back', None


def roll_back(update, failed_step, error):
    """Restore the backup after a failure of `failed_step`."""
    ignore_signals()
    detail = error if str(error) else type(error).__name__
    update.reasons.append(f'Step "{failed_step}" failed: {detail}')
    log(f'step "{failed_step}" failed: {detail}; rolling back')
    return run_restore(update)


def verified_refusal(root):
    """Why --resume-rollback refuses once update-verified.json exists."""
    try:
        target = (read_marker(root, VERIFIED) or {}).get('to')
    except MarkerUnreadable:
        target = None
    return Refused(f'The update{f" to {target}" if target else ""} has been verified: restoring the old '
                   'catalogue would lose what has been written since. Rollback refused; a normal run '
                   'will complete its finalisation.')


def resume_rollback(update):
    """--resume-rollback: restore the backup an unfinished update recorded.

    Acts on update-in-progress.json only. Refused when update-verified.json
    exists (whatever else does, and whatever the flags), and, unless
    --force-rollback is given, when the appliance itself shows that the
    update went through, or does not let it be told (target_health, which
    waits for a container that is starting): a healthy appliance on the new
    version is never rolled back by --resume-rollback alone, and neither is
    one whose health could not be determined.
    """
    if marker_exists(update.root, VERIFIED):
        raise verified_refusal(update.root)
    marker = read_marker(update.root, IN_PROGRESS)
    if marker is None:
        raise Refused('No interrupted update is recorded: nothing to restore.')
    if not update.force:
        log('reading the health of the appliance before any rollback (up to 5 minutes if it is starting)')
        state = target_health(update.root, marker, wait=True)
        if state == 'healthy':
            raise Refused(f'The appliance runs version {marker.get("to")}, in good health, with the target '
                          'image: a rollback would discard what it has written since the update. Rollback '
                          'refused, nothing has been touched. If it is really wanted, run again with '
                          f'--resume-rollback --force-rollback: {FORCE_ROLLBACK_EFFECT}. Or follow the manual '
                          f'procedure in {documentation()}, with the appliance stopped.')
        if state == 'unknown':
            raise Refused('The health of the appliance could not be determined (the target image is installed, but '
                          'the container reports itself neither healthy nor unhealthy, or /health does not answer): it '
                          f'may be running version {marker.get("to")} and may have written data since the '
                          'update. Rollback refused, nothing has been touched. Check the appliance (docker ps), '
                          'then run again; if it is stopped or does not start, run again with --resume-rollback '
                          f'--force-rollback: {FORCE_ROLLBACK_EFFECT}.')
    backup_path = marker.get('backup')
    backups = on_host(update.root, BACKUPS_DIR).resolve()
    path = on_host(update.root, backup_path).resolve() if backup_path else None
    if path is None or path.parent != backups or not path.is_dir():
        raise Refused(f'The recorded backup ({backup_path or "unknown"}) cannot be found: follow the '
                      f'manual procedure in {documentation()}.')
    missing = [name for name in (BACKUP_CATALOG, BACKUP_ETC, BACKUP_HOST_FILES, BACKUP_IMAGE, BACKUP_VERSION)
               if not (path / name).is_file()]
    if missing:
        raise Refused(f'The backup {backup_path} is incomplete ({", ".join(missing)}): nothing has been touched; '
                      f'follow the manual procedure in {documentation()}.')
    try:
        integrity = catalog_integrity(path / BACKUP_CATALOG)
    except (sqlite3.Error, OSError) as error:
        integrity = str(error)
    if integrity != 'ok':
        raise Refused(f'The catalogue of the backup {backup_path} failed the integrity check ({integrity[:200]}): '
                      f'nothing has been touched; follow the manual procedure in {documentation()}.')
    update.backup = path
    update.old_version = marker.get('from')
    update.version = marker.get('to') or update.version
    update.started = marker.get('started')
    update.marker_step = marker.get('step')
    # Kept so that a failed restoration rewrites the marker without losing them.
    update.image = marker.get('image') or ''
    update.entered = marker.get('entered') if isinstance(marker.get('entered'), list) else []
    update.old_module = marker.get('old_module')
    log(f'resuming the rollback from {backup_path}')
    ignore_signals()
    update.reasons.append(f'Rollback of an update interrupted at step "{marker.get("step")}".')
    return run_restore(update, resumed=True)


def secure_module(update, new, kernel):
    """Once the previous DKMS version is gone, the module of `new` must still
    be installed for the running kernel and found by modprobe: removing the
    previous version may have deleted the module file. Reinstalls it once
    with --force when it is not there; when that does not help, says that the
    module will be missing at the next reboot and what to run. Never raises;
    never rolls anything back (the update is verified)."""
    root = update.root
    command = f'dkms install --force -m mhvtl -v {new} -k {kernel}'
    try:
        if dkms_state(root, new, kernel) == 'installed' and module_resolves(root, kernel):
            return
        log(f'mhvtl module {new} is no longer in place for kernel {kernel}: reinstalling')
        run(['dkms', 'install', '--force', '-m', 'mhvtl', '-v', new, '-k', kernel, *dkms_tree_args(root)],
            check=False, timeout=DKMS_TIMEOUT)
        if dkms_state(root, new, kernel) == 'installed' and module_resolves(root, kernel):
            update.warnings.append(f'mhvtl module {new} was no longer in place for kernel {kernel} after the '
                                   'removal of the old version: it has been reinstalled.')
            return
        detail = ''
    except FAILURES as error:
        detail = f' ({error})'
    update.reasons.append(f'mhvtl module {new} is not installed for kernel {kernel}{detail}: it will be missing at the '
                          f'next reboot and the appliance will not start its VTLs. Run: {command}; then '
                          f'check with: modinfo -k {kernel} mhvtl')


def finish(update):
    """After a verified update: drop the previous module, leave maintenance,
    prune the backups, then remove the verified marker, or keep it (with the
    VTLs still to take out) when a leave failed, so that the next run
    retries. Failures here (interruptions included) are reported, never
    rolled back."""
    if update.module is not None:
        old, new = update.module[0], update.module[1]
        try:
            run(['dkms', 'remove', '-m', 'mhvtl', '-v', old, '--all', *dkms_tree_args(update.root)],
                timeout=DKMS_TIMEOUT)
        except FAILURES as error:
            update.reasons.append(f'The old version {old} of the module could not be removed from DKMS ({error}).')
        secure_module(update, new, update.kernel)
    remaining = leave_entered(update)
    try:
        prune_backups(update.root, keep=3, protect=update.backup)
    except FAILURES as error:
        update.reasons.append(f'The old backups could not be deleted ({error}).')
    settle_verified_marker(update, remaining)


def settle_verified_marker(update, remaining):
    """After the finish: remove update-verified.json when nothing remains to
    do, else record in it the VTLs still to take out. A failure changes
    nothing about the update being verified: the file keeps its name, and the
    next run redoes a finish whose every action is idempotent."""
    if update.marker_state != 'verified':
        if remaining:
            update.reasons.append('Take the VTLs still in maintenance out of it in the console: no marker lets '
                                  'the next run do it.')
        return
    try:
        if remaining:
            update.reasons.append('The finalisation (leaving maintenance) will be resumed at the next run.')
            store_marker(update.root, VERIFIED, marker_content(update, remaining))
        else:
            remove_marker(update.root, VERIFIED)
    except FAILURES as error:
        update.warnings.append(f'The verified marker {VERIFIED} could not be updated ({error}): the next '
                               'run will resume the finalisation and remove it.')


def interrupt(number, frame):
    raise Interrupted(f'signal {signal.Signals(number).name} received')


def parse_args(argv):
    parser = argparse.ArgumentParser(description='Update of an installed VaultCenter appliance.')
    parser.add_argument('--image', help='target image, designated by its digest')
    parser.add_argument('--version', help='target version X.Y.Z or X.Y.Z-<pre-release>')
    parser.add_argument('--payload', help='host files extracted from the new image')
    parser.add_argument('--yes', action='store_true', help='confirms the VTLs without a maintenance guard')
    parser.add_argument('--resume-rollback', action='store_true',
                        help='restores the backup of an interrupted update')
    parser.add_argument('--force-rollback', action='store_true',
                        help='with --resume-rollback: restores even if the appliance runs the target version '
                             'in good health (never after a verified update)')
    args = parser.parse_args(argv)
    if args.force_rollback and not args.resume_rollback:
        parser.error('--force-rollback is only used with --resume-rollback')
    if not args.resume_rollback and not (args.image and args.version and args.payload):
        parser.error('--image, --version and --payload are required (except with --resume-rollback)')
    return args


def report(update, result, step=None):
    backup_path = logical_path(update.root, update.backup) if update.backup else None
    return {'result': result, 'from': update.old_version, 'to': update.version, 'reasons': update.reasons,
            'warnings': update.warnings, 'backup': backup_path,
            # Always present: the VTLs an update put in maintenance that are still
            # there, each with its reason. maintenance_left is true exactly when
            # there is none (so also when the run entered no VTL at all).
            'still_in_maintenance': update.still, 'maintenance_left': not update.still,
            'step': step, 'confirmable': bool(update.confirmable and result == 'refused')}


def perform(update):
    """The whole update; returns (result, step).

    Order: argument checks, the lock, the in-progress marker, the digest,
    then preflight, maintenance and backup, steps 5 to 8, the finish.
    """
    try:
        if not update.resume:
            check_arguments(update)
        elif not os.environ.get('VAULTCENTER_INSTALL_ROOT') and os.geteuid() != 0:
            raise Refused('Run the update as root on the host of the appliance.')
        lock_path = on_host(update.root, UPDATE_LOCK)
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        lock = open(lock_path, 'a')
    except Refused as refusal:
        update.reasons = refusal.reasons
        return 'refused', None
    with lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            update.reasons.append('Another update is already running.')
            return 'refused', None
        try:
            if update.resume:
                return resume_rollback(update)
            if marker_exists(update.root, VERIFIED):
                complete_finish(update, read_marker(update.root, VERIFIED))
            else:
                marker = read_marker(update.root, IN_PROGRESS)
                if marker is not None:
                    raise unfinished_refusal(update.root, marker)
            if already_installed(update):
                update.old_version = update.version
                log('the target image is already installed: nothing to do')
                return 'unchanged', None
            preflight(update)
            enter_maintenance(update)
        except Refused as refusal:
            update.confirmable = refusal.confirmable and not update.yes and not update.reasons
            update.reasons = refusal.reasons + update.reasons
            return 'refused', None
        except FAILURES as error:
            update.reasons.append(f'The preflight check could not be made: {error}')
            return 'refused', None
        failure = modify(update)
        if failure is not None:
            return failure
        # Step 8 renamed the marker with signals held: release them, and only then speak.
        interruption = unblock_signals(update)
        log(f'update verified: version {update.version}')
        if update.marker_state == 'deleted':
            update.warnings.append(f'The update is verified, but its marker {IN_PROGRESS} could not be '
                                   f'renamed to {VERIFIED}: it has been deleted. If leaving maintenance fails, '
                                   'take the VTLs out of maintenance in the console.')
        elif update.marker_state == 'stuck':
            update.reasons.append(DONT_ROLL_BACK.format(path=IN_PROGRESS))
        if interruption is not None:
            update.left = False
            for entry in update.entered:
                still_in(update, entry['vtl'], 'interrupted just after the verification: it has yet to leave '
                                               'maintenance')
            update.reasons.append(f'Interruption received just after the verification ({interruption}): the update '
                                  'is verified; leaving maintenance and the cleanup are left to the next '
                                  'run.')
            if update.marker_state != 'verified':
                update.reasons.append('Take the VTLs still in maintenance out of it in the console.')
            return 'updated', None
        finish(update)
        return 'updated', None


def finishing_marker_note(root):
    """What the files on disk mean for a verified update (looked at, never
    assumed). Never raises."""
    try:
        if marker_exists(root, VERIFIED):
            return (f'The verified marker {VERIFIED} is kept: the next run will complete the '
                    'finalisation (leaving maintenance, cleanup).')
        if not marker_exists(root, IN_PROGRESS):
            return 'No marker is left: take the VTLs still in maintenance out of it in the console.'
    except Exception:
        pass
    return DONT_ROLL_BACK.format(path=IN_PROGRESS)


def unsettled(update, reason):
    """After an unexpected stop: every VTL this run entered whose leave did not
    run to its end, and that is not recorded yet, counts as still in
    maintenance."""
    if update.leave_ran:
        return
    known = {record['vtl'] for record in update.still}
    for entry in update.entered:
        if isinstance(entry, dict) and entry.get('vtl') not in known:
            still_in(update, entry.get('vtl'), reason)


def last_resort(update, error):
    """The outcome when an exception escaped every handler (SystemExit, a
    bug): what was really done decides, and nothing more is attempted."""
    detail = f'{type(error).__name__}: {error}' if str(error) else type(error).__name__
    if update.phase == 'preflight':
        update.reasons.append(f'Unexpected stop before any change ({detail}).')
        if update.entered and not update.left:
            unsettled(update, 'unexpected stop: it may have stayed in maintenance')
            update.reasons.append('VTLs put in maintenance by this run may have stayed there: '
                                  'take them out of maintenance in the console.')
        return 'refused', None
    if update.phase == 'finishing':
        update.left = False
        unsettled(update, 'unexpected stop during the finalisation: it has yet to leave maintenance')
        update.reasons.append(f'Unexpected stop after a verified update ({detail}).')
        note = finishing_marker_note(update.root)
        if note not in update.reasons:
            update.reasons.append(note)
        return 'updated', None
    unsettled(update, 'unexpected stop: its state must be checked')
    where = 'during the rollback' if update.phase == 'rollback' else 'without a rollback'
    update.reasons.append(f'Unexpected stop {where} at step "{update.step}" ({detail}). The marker '
                          f'{IN_PROGRESS} is kept: run again with --resume-rollback, or follow the manual '
                          f'procedure in {documentation()} with the backup.')
    return 'rollback_failed', update.step


def main(argv=None):
    args = parse_args(argv)
    os.umask(0o077)
    previous = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, interrupt)
    update = Update(install_root(), args)
    try:
        result, step = perform(update)
    except BaseException as error:
        result, step = last_resort(update, error)
    finally:
        held = unblock_signals(update)
        if held is not None:
            update.reasons.append(f'Interruption received at the end of the run ({held}).')
        for number, handler in previous.items():
            signal.signal(number, handler)
    try:
        print(json.dumps(report(update, result, step), ensure_ascii=False), flush=True)
    except (OSError, ValueError):
        # Nobody can read the result: the exit code still tells it. Point
        # stdout elsewhere so that the interpreter's final flush cannot fail.
        try:
            os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        except (OSError, ValueError, AttributeError):
            pass
    return EXIT_CODES[result]


if __name__ == '__main__':
    sys.exit(main())
