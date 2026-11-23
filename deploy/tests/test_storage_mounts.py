"""State-machine recovery never falls back to restarting the appliance."""
import importlib
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
from test_vtl_storage_state import VtlStorageTests, A, B

class StorageTransitionTests(VtlStorageTests):
    def test_publish_changes_only_a(self):
        request=self.request();self.c.apply(A,request)
        published={};calls=[]
        class Mounts:
            def __init__(self,config): pass
            def publish(self,vtl,path,identity):published[vtl]=(path,identity);calls.append(vtl)
            def verify(self,vtl,path,identity):
                if published.get(vtl)!=(path,identity):raise ValueError('Wrong mount')
        module=importlib.import_module('storage_mounts')
        with patch.object(module,'Mounts',Mounts):self.c.execute(A)
        self.assertEqual(calls,[A]);self.assertNotIn(B,published)
        self.assertEqual(self.c.store.load(A)['phase'],'ready')
        self.assertFalse((self.c.store.operation_root(A)/'storage-block').exists())
        self.assertTrue((self.c.store.operation_root(A)/'storage-services-held').exists())

    def test_recovery_never_recreates_appliance(self):
        self.c.apply(A,self.request());module=importlib.import_module('storage_mounts')
        with patch.object(module.Mounts,'publish',side_effect=ValueError('Lost mount')):
            self.c.execute(A)
        self.assertEqual(self.c.store.load(A)['phase'],'recovery_required')
        self.assertTrue(self.c.status(B)['change_allowed'])
        with patch.object(module.Mounts,'publish'),patch.object(module.Mounts,'verify'):
            self.c.execute(A)
        self.assertEqual(self.c.store.load(A)['phase'],'ready')
        self.assertEqual(self.c.store.load(A)['revision'],1)

    def test_data_in_candidate_after_acceptance_prevents_publish(self):
        self.c.apply(A,self.request());(self.paths['new-a']/'data').write_text('new')
        module=importlib.import_module('storage_mounts')
        with patch.object(module.Mounts,'publish') as publish:
            self.c.execute(A);self.assertFalse(publish.called)
        self.assertEqual(self.c.store.load(A)['phase'],'recovery_required')
        self.assertEqual((self.paths['new-a']/'data').read_text(),'new')

    def test_job_accepted_after_request_prevents_publication(self):
        self.c.apply(A,self.request())
        self.db.execute('INSERT INTO jobs VALUES(?,?)',(A,'running'));self.db.commit()
        module=importlib.import_module('storage_mounts')
        with patch.object(module.Mounts,'publish') as publish:
            self.c.execute(A);self.assertFalse(publish.called)
        self.assertEqual(self.c.store.load(A)['phase'],'recovery_required')

    def test_missing_volume_boot_still_installs_readonly_slot(self):
        module=importlib.import_module('storage_mounts');m=module.Mounts(self.c.config)
        calls=[]
        with patch.object(m,'prepare'),patch.object(m,'slot',side_effect=lambda v:calls.append(v)),patch.object(m,'publish',side_effect=ValueError('missing source')):
            with self.assertRaises(ValueError):m.restore(A,'/missing',{})
        self.assertEqual(calls,[A])

    def test_boot_prepares_slots_for_interrupted_transitions(self):
        module=importlib.import_module('storage_mounts');m=module.Mounts(self.c.config)
        calls=[]
        with patch.object(m,'prepare'),patch.object(m,'slot',side_effect=lambda v:calls.append(v)),patch.object(m,'restore'):
            m.restore_states([{'vtl_id':A,'phase':'applying'},{'vtl_id':B,'phase':'recovery_required'}])
        self.assertEqual(calls,[A,B])

if __name__=='__main__':unittest.main()
