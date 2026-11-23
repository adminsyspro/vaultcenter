"""Attest the running engines before allowing per-cartridge online reads."""
from pathlib import Path
import mmap
import os

ENGINE = Path('/usr/lib/libvtlscsi.so')
PROC = Path('/proc')
CAPABILITY = b'VAULTCENTER_MEDIA_LOCK_V1'


def capable(path):
    try:
        with path.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as image:
            return image.find(CAPABILITY) >= 0
    except (OSError, ValueError):
        return False


def running_capable(pid):
    # The cartridge backend is a shared library. Inspect the mapped inode,
    # not just vtltape or the new library sitting on disk after an upgrade.
    try:
        info = ENGINE.stat()
        rows = [line.split() for line in (PROC / pid / 'maps').read_text().splitlines()
                if 'libvtlscsi.so' in line]
        if not rows or not any('x' in row[1] for row in rows):
            return False
        for row in rows:
            major, minor = (int(n, 16) for n in row[3].split(':'))
            if (len(row) != 6 or int(row[4]) != info.st_ino
                    or os.makedev(major, minor) != info.st_dev):
                return False
        return capable(ENGINE)
    except (OSError, ValueError, IndexError):
        return False


def check(instance, request):
    instance.installed(request)
    controller = instance.controller
    if not instance.scoped or controller.read()['phase'] != 'online':
        raise RuntimeError('Online export unavailable in this library state.')
    if (controller.block.exists() or set(controller.services().values()) != {'active'}
            or any((instance.root / name).exists() for name in (
                'cartridge-deletion-pending.json', 'reintegration-pending.json'))):
        raise RuntimeError('Finish the work on the library before the export.')
    drives = controller.config['runtime']['drives']
    if not drives or not capable(ENGINE):
        raise RuntimeError('Update the mhVTL engine to export cartridges without maintenance.')
    for drive in drives:
        unit = f'vaultcenter-drive-{instance.vtl_id}-{drive["id"]}.service'
        pid = controller.run(['systemctl', 'show', '--value', '--property=MainPID', unit]).strip()
        if not pid.isdecimal() or int(pid) <= 0 or not running_capable(pid):
            raise RuntimeError('The running mhVTL engine does not protect cartridges: restart the updated engine before the online export.')
    return {'vtl_id': instance.vtl_id, 'media_lock_version': 1}
