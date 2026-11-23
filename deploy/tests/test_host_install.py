import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
INSTALL = ROOT / 'deploy/host/install.py'
IMAGE = 'ghcr.io/adminsyspro/vaultcenter@sha256:' + 'a' * 64

FAKE_DOCKER = '''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
root = os.environ['VAULTCENTER_INSTALL_ROOT']
if args[:1] == ['compose']:
    print(json.dumps({
        'name': 'vaultcenter',
        'services': {'appliance': {'image': os.environ.get('VAULTCENTER_IMAGE', ''), 'volumes': []}},
        'volumes': {'state': {'name': 'vc_state'}, 'media': {'name': 'vc_media'}}}))
elif args[:1] == ['inspect']:
    sys.exit(1)
elif args[:2] == ['volume', 'create']:
    os.makedirs(root + '/docker/' + args[2], exist_ok=True)
    print(args[2])
elif args[:2] == ['volume', 'inspect']:
    print(json.dumps([{'Mountpoint': root + '/docker/' + args[2]}]))
else:
    sys.exit('unexpected docker call: %r' % args)
'''
FAKE_OK = '#!/bin/sh\nexit 0\n'


def snapshot(directory):
    return sorted(str(p.relative_to(directory)) for p in Path(directory).rglob('*'))


class UnitOrderTests(unittest.TestCase):
    def test_host_service_starts_before_docker(self):
        unit = (ROOT / 'deploy/host/vaultcenter-storage-host.service').read_text()
        self.assertIn('Before=docker.service', unit)
        self.assertNotIn('After=docker.service', unit)
        self.assertNotIn('Requires=docker.service', unit)


class InstallImageTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name) / 'root'
        self.root.mkdir()
        bin_dir = Path(temp.name) / 'bin'
        bin_dir.mkdir()
        for name, body in (('docker', FAKE_DOCKER), ('systemctl', FAKE_OK), ('sysctl', FAKE_OK)):
            (bin_dir / name).write_text(body)
            (bin_dir / name).chmod(0o755)
        self.bin_dir = bin_dir
        self.compose = Path(temp.name) / 'compose.yaml'
        self.compose.write_text('services: {}\n')

    def install(self, *extra):
        env = dict(os.environ, VAULTCENTER_INSTALL_ROOT=str(self.root),
                   PATH=f'{self.bin_dir}:{os.environ["PATH"]}')
        env.pop('VAULTCENTER_IMAGE', None)
        return subprocess.run(
            [sys.executable, str(INSTALL), '--compose', str(self.compose), '--new-appliance', *extra],
            env=env, capture_output=True, text=True)

    def test_resolved_image_is_written_to_compose_json(self):
        result = self.install('--image', IMAGE)
        self.assertEqual(result.returncode, 0, result.stderr)
        compose = json.loads((self.root / 'etc/vaultcenter-host/compose.json').read_text())
        self.assertEqual(compose['services']['appliance']['image'], IMAGE)

    def test_tag_reference_is_refused(self):
        result = self.install('--image', 'ghcr.io/adminsyspro/vaultcenter:0.2.0')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('digest', result.stderr)
        self.assertEqual(snapshot(self.root), [])

    def test_existing_host_configuration_is_refused(self):
        (self.root / 'etc/vaultcenter-host').mkdir(parents=True)
        before = snapshot(self.root)
        result = self.install('--image', IMAGE)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(snapshot(self.root), before)


if __name__ == '__main__':
    unittest.main()
