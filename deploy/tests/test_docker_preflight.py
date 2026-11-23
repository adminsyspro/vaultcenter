"""A missing host prerequisite must never start the appliance."""
import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / 'docker' / 'preflight.py'


class DockerPreflightTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SCRIPT.is_file(), 'Docker preflight is missing')
        spec = importlib.util.spec_from_file_location('docker_preflight', SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for path in ('sys/module/mhvtl', 'sys/module/sg'):
            (self.root / path).mkdir(parents=True)
        for path in ('etc/cold-vtl/auth.env', 'etc/tgt/targets.conf',
                     'etc/mhvtl/device.conf', 'etc/mhvtl/mhvtl.conf',
                     'etc/mhvtl/library_contents.10'):
            file = self.root / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text('# operator configuration\n')

    def test_valid_prerequisites_do_not_modify_data(self):
        marker = self.root / 'var/lib/cold-vtl/maintenance-block'
        marker.parent.mkdir(parents=True)
        marker.write_text('{"lease":"existing"}')
        before = {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        self.assertEqual(self.module.check(self.root), [])
        self.assertEqual(before, {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()})

    def test_missing_kernel_module_is_reported(self):
        (self.root / 'sys/module/mhvtl').rmdir()
        self.assertTrue(any('mhvtl' in error for error in self.module.check(self.root)))

    def test_missing_configuration_is_reported(self):
        (self.root / 'etc/cold-vtl/auth.env').unlink()
        (self.root / 'etc/tgt/targets.conf').unlink()
        errors = '\n'.join(self.module.check(self.root))
        self.assertIn('/etc/cold-vtl/auth.env', errors)
        self.assertIn('/etc/tgt/targets.conf', errors)

    def test_empty_configuration_is_reported(self):
        (self.root / 'etc/mhvtl/device.conf').write_text('')
        self.assertTrue(any('device.conf' in error for error in self.module.check(self.root)))
