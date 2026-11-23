"""A lost bridge must not silently turn a managed appliance into legacy mode."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT=Path(__file__).resolve().parents[1]/'cold-vtl/storage-check.py'
class StorageGateTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SCRIPT.exists(), 'Storage gate missing')
        spec=importlib.util.spec_from_file_location('storage_check',SCRIPT)
        self.module=importlib.util.module_from_spec(spec);spec.loader.exec_module(self.module)
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
    def test_legacy_installation_remains_usable(self):
        self.module.check(self.root, '/absent')
    def test_managed_missing_socket_and_unready_storage_are_refused(self):
        (self.root/'storage-managed').touch()
        with self.assertRaises(ValueError): self.module.check(self.root,'/absent')
        for phase in ('unconfigured','unavailable','applying','recovery_required'):
            with patch.object(self.module,'request',return_value={'phase':phase}), self.assertRaises(ValueError): self.module.check(self.root,'/unused')
    def test_ready_storage_permits_operations_but_service_hold_is_respected(self):
        (self.root/'storage-managed').touch(); (self.root/'storage-services-held').touch()
        with patch.object(self.module,'request',return_value={'phase':'ready'}):
            self.module.check(self.root,'/unused')
            with self.assertRaises(ValueError): self.module.check(self.root,'/unused',service=True)
            (self.root/'storage-services-held').unlink()
            self.module.check(self.root,'/unused',service=True)
            (self.root/'storage-block').touch()
            with self.assertRaises(ValueError): self.module.check(self.root,'/unused')

    def test_isolated_storage_requires_context_and_only_its_gate(self):
        a='00000000-0000-4000-8000-000000000001';b='00000000-0000-4000-8000-000000000003'
        (self.root/'storage-managed').touch();(self.root/'storage-isolated').touch()
        for vtl in (a,b):(self.root/'vtls'/vtl).mkdir(parents=True)
        (self.root/'vtls'/a/'storage-block').touch()
        with patch.object(self.module,'request',return_value={'vtl_id':b,'phase':'ready'}):
            with self.assertRaises(ValueError):self.module.check(self.root,'/unused')
            self.module.check(self.root,'/unused',vtl_id=b)
            with self.assertRaises(ValueError):self.module.check(self.root,'/unused',vtl_id=a)
        with patch.object(self.module,'request',return_value={'vtl_id':a,'phase':'ready'}):
            with self.assertRaises(ValueError):self.module.check(self.root,'/unused',vtl_id=b)
