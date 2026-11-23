import importlib.util
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'cold-vtl'))
path=Path(__file__).resolve().parents[1]/'docker/isolated-boot.py'
spec=importlib.util.spec_from_file_location('isolated_boot',path);boot=importlib.util.module_from_spec(spec);spec.loader.exec_module(boot)

class IsolatedBootTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        state=self.root/'var/lib/cold-vtl';state.mkdir(parents=True);(state/'storage-isolated').write_text('{}')
        settings=self.root/'etc/cold-vtl/vtl-runtime.json';settings.parent.mkdir(parents=True);settings.write_text('{"resources":[],"bindings":{}}')
        self.system=self.root/'etc/systemd/system';self.system.mkdir(parents=True)
    def test_image_installed_legacy_unit_is_preserved_and_disabled(self):
        file=self.system/'vaultcenter-telemetry.service';original='[Service]\nExecStart=/original\n';file.write_text(original)
        with sqlite3.connect(self.root/'var/lib/cold-vtl/catalog.db') as db:db.execute('CREATE TABLE vtl_provisioning(vtl_id TEXT,phase TEXT)')
        boot.configure(self.root)
        self.assertEqual(file.read_text(),original)
        self.assertIn('ConditionPathExists=!/var/lib/cold-vtl/storage-isolated',(self.system/'vaultcenter-telemetry.service.d/zz-isolated.conf').read_text())
    def test_fresh_appliance_can_boot_before_api_creates_catalogue(self):
        boot.configure(self.root)
        self.assertFalse((self.root/'var/lib/cold-vtl/catalog.db').exists())

    def test_isolated_tgt_masks_legacy_dependency_dropin(self):
        old=self.system/'tgt.service.d/cold-vtl.conf';old.parent.mkdir(parents=True)
        text='[Unit]\nRequires=vtllibrary@10.service\n';old.write_text(text)
        boot.configure(self.root)
        self.assertTrue(old.is_symlink())
        self.assertEqual(str(old.readlink()),'/dev/null')
        self.assertEqual((self.root/'var/lib/cold-vtl/legacy-systemd/tgt-cold-vtl.conf').read_text(),text)

    def test_existing_draft_gets_explicit_unlimited_configuration(self):
        state=self.root/'var/lib/cold-vtl'
        vtl='00000000-0000-4000-8000-000000000021'
        with sqlite3.connect(state/'catalog.db') as db:
            db.execute('CREATE TABLE vtl_provisioning(vtl_id TEXT,phase TEXT)')
            db.execute('CREATE TABLE vtls(id TEXT)')
            db.execute('INSERT INTO vtls VALUES(?)',(vtl,))
        boot.configure(self.root)
        file=state/'vtls'/vtl/'bandwidth.json'
        self.assertTrue(file.exists(), 'Draft VTLs must be explicitly migrated too')
        self.assertIsNone(json.loads(file.read_text())['s3_bps'])
        file.unlink()
        boot.configure(self.root)
        self.assertFalse(file.exists(), 'A missing migrated configuration must never reset limits')
