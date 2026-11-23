"""Host-only directory validation. Never follow links or adopt existing data.
Only a missing final component may be created, below an existing parent."""
from contextlib import contextmanager
import json
import os
from pathlib import PurePosixPath
import subprocess
import uuid

PROTECTED = ('/etc', '/usr', '/bin', '/sbin', '/lib', '/lib64', '/proc', '/sys', '/dev', '/run', '/boot', '/root', '/var/lib/docker', '/var/lib/containerd', '/var/lib/vaultcenter-host')

def validate(path):
    if (not isinstance(path, str) or not path.startswith('/') or path.startswith('//') or len(path.encode()) > 4096
            or any(ord(c) < 32 for c in path) or '..' in path.split('/') or path == '/'):
        raise ValueError('Enter an absolute path to an existing folder.')

@contextmanager
def directory(path):
    validate(path)
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in PurePosixPath(path).parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd); fd = child
        yield fd
    except OSError as error:
        raise ValueError('Folder inaccessible, a symbolic link or not writable.') from error
    finally:
        os.close(fd)

def create_leaf(path):
    """Create only the last component, below an existing link-free parent."""
    validate(path)
    parent, leaf = str(PurePosixPath(path).parent), PurePosixPath(path).name
    if parent == '/': return False
    with directory(parent) as fd:
        try:
            os.mkdir(leaf, 0o700, dir_fd=fd)
            return True
        except FileExistsError:
            return False
        except OSError as error:
            raise ValueError('Cannot create the folder in its parent folder.') from error

def remove_leaf(path):
    with directory(str(PurePosixPath(path).parent)) as fd:
        try: os.rmdir(PurePosixPath(path).name, dir_fd=fd)
        except OSError: pass

def mount_identity(path):
    try:
        output = subprocess.run(['findmnt', '--json', '--target', path, '--output', 'TARGET,FSTYPE,UUID,FSROOT'], check=True, capture_output=True, text=True, timeout=5)
        row = json.loads(output.stdout)['filesystems'][0]
        # Only validated local filesystems with a persistent identity. Never infer
        # identity from /dev/sdN, st_dev or transient mount IDs.
        if row['fstype'] not in ('ext4', 'xfs') or not row.get('uuid'):
            raise ValueError('A local ext4 or XFS volume with a UUID is required; network shares are not supported yet.')
        return {'uuid': row['uuid'], 'filesystem': row['fstype'], 'mount_root': row['fsroot'], 'mountpoint': row['target']}
    except (OSError, subprocess.SubprocessError, KeyError, IndexError, TypeError, json.JSONDecodeError) as error:
        raise ValueError('Cannot verify the volume identity.') from error

def identity_for(fd, path):
    info = os.fstat(fd)
    return {**mount_identity(path), 'inode': info.st_ino}

def inspect_path(path, forbidden):
    canonical = str(PurePosixPath(path)) if isinstance(path, str) else ''
    for item in (*PROTECTED, *forbidden):
        root = str(PurePosixPath(item))
        if canonical == root or canonical.startswith(root + '/') or root.startswith(canonical + '/'):
            raise ValueError('This folder is reserved for the system or the VaultCenter configuration.')
    created = create_leaf(canonical)
    try:
        return checked(canonical)
    except BaseException:
        if created: remove_leaf(canonical)
        raise

def checked(canonical):
    with directory(canonical) as fd:
        if os.listdir(fd):
            raise ValueError('Choose an empty folder. Existing data will not be moved.')
        identity = identity_for(fd, canonical)
        probe = '.vaultcenter-check-' + uuid.uuid4().hex
        created = False
        try:
            probe_fd = os.open(probe, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
            created = True
            try:
                os.write(probe_fd, b'VaultCenter storage check\n'); os.fsync(probe_fd)
            finally:
                os.close(probe_fd)
        finally:
            if created: os.unlink(probe, dir_fd=fd)
        os.fsync(fd)
        stats = os.fstatvfs(fd)
        verify_identity(canonical, identity)
        return {'path': canonical, 'filesystem': identity['filesystem'], 'total_bytes': stats.f_blocks * stats.f_frsize, 'available_bytes': stats.f_bavail * stats.f_frsize, 'identity': identity}

def verify_identity(path, identity):
    with directory(path) as fd:
        if identity_for(fd, path) != identity:
            raise ValueError('The folder or volume has changed. Restore the configured storage.')
        stats = os.fstatvfs(fd)
        if stats.f_flag & os.ST_RDONLY:
            raise ValueError('The storage is mounted read-only.')
        return {'total_bytes': stats.f_blocks * stats.f_frsize, 'available_bytes': stats.f_bavail * stats.f_frsize}
