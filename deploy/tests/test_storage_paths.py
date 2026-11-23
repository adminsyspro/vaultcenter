"""Verify host directories without adopting data or following replaced paths."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'host/storage_paths.py'

class StoragePathsTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SCRIPT.exists(), 'Host path validator is missing')
        spec = importlib.util.spec_from_file_location('storage_paths', SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.target = self.root / 'tapes'; self.target.mkdir()
        self.mount = patch.object(self.module, 'mount_identity', return_value={'uuid':'disk-A', 'filesystem':'ext4', 'mount_root':'/', 'mountpoint':str(self.root)})
        self.mount.start(); self.addCleanup(self.mount.stop)

    def test_empty_directory_reports_capacity_and_leaves_no_probe(self):
        result = self.module.inspect_path(str(self.target), [])
        self.assertEqual(result['path'], str(self.target))
        self.assertGreater(result['total_bytes'], 0)
        self.assertGreater(result['available_bytes'], 0)
        self.assertEqual(list(self.target.iterdir()), [])
        self.module.verify_identity(str(self.target), result['identity'])

    def test_creates_missing_leaf_under_existing_parent(self):
        path = self.target/'veeam'
        result = self.module.inspect_path(str(path), [])
        self.assertTrue(path.is_dir())
        self.assertEqual(path.stat().st_mode & 0o777, 0o700)
        self.assertEqual(list(path.iterdir()), [])
        self.module.verify_identity(str(path), result['identity'])

    def test_never_creates_missing_parents_or_through_symlinks(self):
        with self.assertRaises(ValueError): self.module.inspect_path(str(self.root/'missing'/'veeam'), [])
        self.assertFalse((self.root/'missing').exists())
        link = self.root/'link'; link.symlink_to(self.target, target_is_directory=True)
        with self.assertRaises(ValueError): self.module.inspect_path(str(link/'veeam'), [])
        self.assertFalse((self.target/'veeam').exists())

    def test_created_leaf_is_removed_when_validation_fails(self):
        with patch.object(self.module, 'mount_identity', side_effect=ValueError('nfs')):
            with self.assertRaises(ValueError): self.module.inspect_path(str(self.target/'veeam'), [])
        self.assertFalse((self.target/'veeam').exists())

    def test_rejects_missing_nonempty_relative_and_traversal(self):
        for path in (str(self.root/'missing'/'leaf'), 'relative', str(self.root)+'/../tapes'):
            with self.subTest(path=path), self.assertRaises(ValueError): self.module.inspect_path(path, [])
        (self.target/'existing').write_text('keep me')
        with self.assertRaises(ValueError): self.module.inspect_path(str(self.target), [])
        self.assertEqual((self.target/'existing').read_text(), 'keep me')

    def test_rejects_symlinks_and_protected_subtrees(self):
        link = self.root/'link'; link.symlink_to(self.target, target_is_directory=True)
        for path, forbidden in ((str(link), []), (str(self.target), [str(self.root)]), ('/'+str(self.target), [str(self.root)]), ('/etc', []), ('/', [])):
            with self.subTest(path=path), self.assertRaises(ValueError): self.module.inspect_path(path, forbidden)

    def test_mount_loss_or_directory_replacement_invalidates_identity(self):
        value = self.module.inspect_path(str(self.target), [])['identity']
        with patch.object(self.module, 'mount_identity', return_value={'uuid':'system-disk', 'filesystem':'ext4', 'mount_root':'/', 'mountpoint':'/'}):
            with self.assertRaises(ValueError): self.module.verify_identity(str(self.target), value)
        self.target.rename(self.root/'old'); self.target.mkdir()
        with self.assertRaises(ValueError): self.module.verify_identity(str(self.target), value)

    def test_write_failure_does_not_leave_probe(self):
        with patch.object(self.module.os, 'write', side_effect=PermissionError('denied')):
            with self.assertRaises(ValueError): self.module.inspect_path(str(self.target), [])
        self.assertEqual(list(self.target.iterdir()), [])

if __name__ == '__main__': unittest.main()
