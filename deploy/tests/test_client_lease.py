"""Local lease mirrors the PBS guard protocol for clients without a guard."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'cold-vtl'))
import client_lease
L1='00000000-0000-4000-8000-0000000000a1'
L2='00000000-0000-4000-8000-0000000000a2'
V='00000000-0000-4000-8000-000000000031'

class LocalLeaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.g=client_lease.LocalLease(self.root,V)

    def test_cycle(self):
        self.assertEqual(self.g.handle('status'),{'ok':True,'vtl_id':V,'registered':True,'bootstrap':False,'lease':None,'locked':False})
        for action in ('bootstrap','verify'):
            value=self.g.handle(action,L1);self.assertEqual((value['lease'],value['locked']),(L1,True))
        self.assertEqual(self.g.handle('acquire',L1)['lease'],L1)
        self.assertEqual(self.g.handle('release',L1)['locked'],False)

    def test_local_lease_refuses_foreign_lease(self):
        self.g.handle('acquire',L1)
        for action in ('acquire','bootstrap','verify','release'):
            with self.subTest(action=action),self.assertRaises(ValueError):self.g.handle(action,L2)
        self.assertEqual(self.g.handle('status')['lease'],L1)

    def test_release_is_idempotent_but_refuses_foreign_lease(self):
        value=self.g.handle('release',L1)
        self.assertEqual((value['lease'],value['locked']),(None,False))
        self.g.handle('acquire',L1)
        with self.assertRaises(ValueError):self.g.handle('release',L2)
        self.assertEqual(self.g.handle('status')['lease'],L1)

    def test_lease_must_be_uuid_shaped(self):
        for action in ('acquire','bootstrap','verify','release'):
            for bad in ('L1','','x'*36,L1.upper()):
                with self.subTest(action=action,bad=bad),self.assertRaises(ValueError):self.g.handle(action,bad)

    def test_verify_without_lease_refused(self):
        with self.assertRaises(ValueError):self.g.handle('verify',L1)

    def test_local_lease_rejects_corrupt_file(self):
        (self.root/'client-lease.json').write_text(json.dumps({'vtl_id':'other','lease':L1}))
        with self.assertRaises(ValueError):self.g.handle('status')
        (self.root/'client-lease.json').write_text('{')
        with self.assertRaises(ValueError):self.g.handle('status')

    def test_unknown_action_and_missing_lease(self):
        with self.assertRaises(ValueError):self.g.handle('inventory')
        with self.assertRaises(ValueError):self.g.handle('acquire',None)

if __name__=='__main__':unittest.main()
