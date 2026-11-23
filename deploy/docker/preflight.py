#!/usr/bin/env python3
"""Read-only host/configuration checks before starting systemd."""
from pathlib import Path
import sys


def check(root=Path('/')):
    errors = []
    for module in ('mhvtl', 'sg'):
        if not (root / 'sys/module' / module).is_dir():
            errors.append(f'Load the {module} kernel module on the Linux host first.')
    names=('/etc/cold-vtl/auth.env','/etc/cold-vtl/vtl-runtime.json') if (root/'var/lib/cold-vtl/storage-isolated').exists() else ('/etc/cold-vtl/auth.env','/etc/tgt/targets.conf','/etc/mhvtl/device.conf','/etc/mhvtl/mhvtl.conf','/etc/mhvtl/library_contents.10')
    for name in names:
        path = root / name.lstrip('/')
        if not path.is_file() or not path.stat().st_size:
            errors.append(f'Missing or empty operator configuration: {name}')
    return errors


if __name__ == '__main__':
    failures = check()
    for failure in failures:
        print(f'VaultCenter: {failure}', file=sys.stderr)
    sys.exit(bool(failures))
