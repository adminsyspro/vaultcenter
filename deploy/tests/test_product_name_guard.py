from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from test_release import git_env

ROOT = Path(__file__).parents[2]
GUARD = ROOT / 'deploy/tests/check_product_name.sh'
FORMER = 'free' + 'zer'
SCRIPT_LINES = [
    '#!/bin/sh',
    f'LEGACY_HOST_DIR=/etc/{FORMER}-host',
    f'LEGACY_CONTAINER={FORMER}-appliance-1',
    f'systemctl stop {FORMER}-storage.service',
]
GUARD_TEXT = GUARD.read_text()


def listed_detectors():
    """The detector paths named in the guard's DETECTORS array."""
    block = GUARD_TEXT.split('DETECTORS=(', 1)[1].split(')', 1)[0]
    return [line.strip() for line in block.splitlines()
            if line.strip() and not line.strip().startswith('#')]


class ProductNameGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True, env=git_env())
        (self.root / 'deploy/tests').mkdir(parents=True)
        (self.root / 'deploy/install').mkdir(parents=True)
        shutil.copy(GUARD, self.root / 'deploy/tests/check_product_name.sh')
        self.write('README.md', 'VaultCenter\n')
        self.write('deploy/install/install.sh', '\n'.join(SCRIPT_LINES) + '\n')

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_guard(self):
        subprocess.run(['git', 'add', '-A'], cwd=self.root, check=True, env=git_env())
        return subprocess.run(['bash', 'deploy/tests/check_product_name.sh'],
                              cwd=self.root, env=git_env(), capture_output=True, text=True)

    def test_allowed_forms_in_a_detector_pass(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_former_name_in_another_file_fails(self):
        self.write('docs/other.md', f'Installed as {FORMER}.\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn('docs/other.md', result.stdout)
        self.assertIn('outside historical records', result.stderr)

    def test_former_name_in_a_historical_record_passes(self):
        self.write('docs/migration-to-vaultcenter.md', f'{FORMER}\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_disallowed_form_in_a_detector_fails(self):
        self.write('deploy/install/install.sh',
                   '\n'.join(SCRIPT_LINES) + f'\necho "Welcome to {FORMER.capitalize()} 2"\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn('Welcome to', result.stderr)
        self.assertIn('beyond the old paths and units', result.stderr)

    def test_bare_name_before_a_unit_form_fails(self):
        self.write('deploy/install/install.sh', f'echo "{FORMER} {FORMER}-host"\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)


class ProductNameGuardExclusionListTests(unittest.TestCase):
    def test_every_excluded_detector_exists(self):
        for name in listed_detectors():
            self.assertTrue((ROOT / name).exists(), name)

    def test_guard_passes_on_this_tree(self):
        result = subprocess.run(['bash', str(GUARD)], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
