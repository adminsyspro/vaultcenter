"""Path changes must never lose ownership or become editable after data existed."""
import importlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

HOST = Path(__file__).resolve().parents[1] / 'host'
sys.path.insert(0, str(HOST))
A = '00000000-0000-4000-8000-000000000001'
B = '00000000-0000-4000-8000-000000000003'

class VtlStorageTests(unittest.TestCase):
    def setUp(self):
        self.module = importlib.import_module('vtl_storage')
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root/'state'; self.state.mkdir()
        self.db = sqlite3.connect(self.state/'catalog.db'); self.addCleanup(self.db.close)
        self.db.executescript('''CREATE TABLE vtls(id TEXT PRIMARY KEY);
            CREATE TABLE cartridges(vtl_id TEXT,barcode TEXT);
            CREATE TABLE jobs(vtl_id TEXT,status TEXT);
            CREATE TABLE archive_requests(vtl_id TEXT,phase TEXT);
            CREATE TABLE bucket_rotations(vtl_id TEXT,phase TEXT);''')
        self.db.executemany('INSERT INTO vtls VALUES(?)', [(A,), (B,)]); self.db.commit()
        self.paths = {}
        for name in ('a','b','new-a','new-b'):
            self.paths[name] = self.root/name; self.paths[name].mkdir()
        self.config = {'state_dir':str(self.state),'host_state':str(self.root/'host'),
            'initial_media':{A:str(self.paths['a']),B:str(self.paths['b'])},
            'mount_root':str(self.root/'published'),'container':'test-appliance'}
        self.mount = patch('storage_paths.mount_identity', side_effect=self.identity)
        self.mount.start(); self.addCleanup(self.mount.stop)
        self.c = self.module.VtlStorage(self.config)
        self.idle = patch.object(self.c, 'require_idle', return_value=None)
        self.idle.start(); self.addCleanup(self.idle.stop)

    def identity(self, path):
        return {'uuid':'disk-one','filesystem':'ext4','mount_root':'/', 'mountpoint':str(self.root)}

    def request(self, vtl=A, path='new-a', request_id='request-one'):
        checked = self.c.check(vtl, str(self.paths[path]))
        return {'path':checked['path'],'revision':self.c.status(vtl)['revision'],
                'request_id':request_id,'check_id':checked['check_id'],'actor':'admin'}

    def test_data_lock_is_monotone_and_scoped(self):
        self.db.execute('INSERT INTO cartridges VALUES(?,?)',(A,'SAME01L8')); self.db.commit()
        self.assertFalse(self.c.status(A)['change_allowed'])
        self.assertTrue(self.c.status(A)['data_locked'])
        self.db.execute('DELETE FROM cartridges'); self.db.commit()
        restarted = self.module.VtlStorage(self.config)
        self.assertEqual(restarted.status(A)['change_lock_reason'],'data_present')
        self.assertTrue(restarted.status(B)['change_allowed'])
        (self.paths['b']/'unregistered').write_bytes(b'preserve')
        self.assertTrue(restarted.status(B)['data_locked'])
        (self.paths['b']/'unregistered').unlink()
        self.assertFalse(self.module.VtlStorage(self.config).status(B)['change_allowed'])

    def test_data_appears_after_check(self):
        request = self.request()
        (self.paths['a']/'cartridge').write_text('data')
        with self.assertRaises(ValueError): self.c.apply(A, request)
        self.assertTrue(self.c.status(A)['data_locked'])
        self.assertEqual(self.c.status(A)['revision'],0)
        self.assertEqual((self.paths['a']/'cartridge').read_text(),'data')
        self.assertEqual(list(self.paths['new-a'].iterdir()),[])

    def test_check_token_and_replay_are_vtl_bound(self):
        request = self.request()
        with self.assertRaises(ValueError): self.c.apply(B, request)
        first = self.c.apply(A, request)
        self.assertEqual(first['phase'],'applying')
        self.assertEqual(self.c.apply(A, request)['operation']['id'], first['operation']['id'])
        with self.assertRaises(ValueError): self.c.apply(A, {**request,'path':str(self.paths['new-b'])})
        self.assertTrue(self.c.status(B)['change_allowed'])

    def test_operation_locks_are_scoped_and_data_lock_cannot_be_cleared(self):
        with self.c.store.operation_lock(A):
            with self.assertRaises(ValueError): self.c.lock_data(A)
            self.c.lock_data(B)
        self.c.lock_data(A)
        state = self.c.store.load(A); state['data_locked'] = False; self.c.store.save(A,state)
        self.assertTrue(self.c.status(A)['data_locked'])

    def test_busy_b_does_not_block_a(self):
        self.db.execute('INSERT INTO jobs VALUES(?,?)',(B,'running')); self.db.commit()
        self.assertTrue(self.c.status(A)['change_allowed'])
        self.assertEqual(self.c.status(B)['change_lock_reason'],'operation_in_progress')

    def test_pending_destination_is_reserved_across_instances(self):
        self.c.apply(A, self.request())
        other = self.module.VtlStorage(self.config)
        with self.assertRaises(ValueError): other.check(B,str(self.paths['new-a']))
        child = self.paths['new-a']/'child'; child.mkdir()
        with self.assertRaises(ValueError): other.check(B,str(child))

    def test_alias_of_same_filesystem_subtree_is_rejected(self):
        self.c.apply(A, self.request())
        # Different mountpoint spelling, same on-volume subdirectory.
        def alias(path):
            if path == str(self.paths['new-b']):
                return {'uuid':'disk-one','filesystem':'ext4', 'mount_root':'/new-a', 'mountpoint':path}
            return self.identity(path)
        with patch('storage_paths.mount_identity',side_effect=alias):
            with self.assertRaises(ValueError): self.c.check(B,str(self.paths['new-b']))

    def test_unknown_vtl_and_replaced_path_fail_closed(self):
        with self.assertRaises(ValueError): self.c.status('../escape')
        with self.assertRaises(ValueError): self.c.status('00000000-0000-4000-8000-000000000099')
        req = self.request(); self.paths['new-a'].rename(self.root/'saved'); self.paths['new-a'].mkdir()
        with self.assertRaises(ValueError): self.c.apply(A, req)

    def test_missing_initial_storage_does_not_become_editable(self):
        self.paths['a'].rmdir()
        self.assertFalse(self.c.status(A)['change_allowed'])
        self.assertEqual(self.c.status(A)['change_lock_reason'],'storage_unavailable')

if __name__ == '__main__': unittest.main()
