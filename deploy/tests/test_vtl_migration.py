"""Mode switch preserves the catalogue and refuses adoption of used storage."""
import importlib
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'host'))
A='00000000-0000-4000-8000-000000000001'

class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.module=importlib.import_module('migrate_vtl_storage')
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        self.state=self.root/'state';self.state.mkdir();self.media=self.root/'media';self.media.mkdir()
        self.host=self.root/'host';self.host.mkdir()
        self.db=sqlite3.connect(self.state/'catalog.db');self.addCleanup(self.db.close)
        self.db.executescript('CREATE TABLE vtls(id TEXT, lifecycle TEXT,media_root TEXT,restore_root TEXT);CREATE TABLE cartridges(vtl_id TEXT,barcode TEXT);CREATE TABLE jobs(status TEXT);CREATE TABLE users(name TEXT);INSERT INTO users VALUES("preserved");')
        self.db.execute('INSERT INTO vtls VALUES(?,?,?,?)',(A,'ready','/opt/mhvtl','/var/lib/cold-vtl/restores'));self.db.commit()
        self.compose=self.root/'compose.json';self.compose.write_text(json.dumps({'services':{'appliance':{'image':'old','volumes':[]}},'volumes':{}}))
        self.config=self.root/'storage.json';self.config.write_text(json.dumps({'state_dir':str(self.state),'media_dir':str(self.media),'host_state':str(self.host),'compose_file':str(self.compose),'container':'test'}))
        self.migration=self.module.Migration(self.config,stopped=lambda:True)

    def test_empty_appliance_switch_preserves_catalogue_and_is_recoverable(self):
        manifest=self.migration.prepare(self.root/'backup','sha256:'+'a'*64)
        self.migration.apply(manifest)
        self.migration.apply(manifest)
        self.assertEqual(self.db.execute('SELECT name FROM users').fetchone()[0],'preserved')
        self.assertEqual(self.db.execute('SELECT lifecycle,media_root FROM vtls').fetchone(),('draft',None))
        self.assertTrue((self.state/'storage-isolated').exists())
        self.assertTrue(json.loads(self.config.read_text())['isolated'])

    def test_used_media_and_monotone_marker_are_preserved_by_refusal(self):
        (self.media/'TAPE01').mkdir();(self.media/'TAPE01/data').write_bytes(b'preserve')
        marker=self.state/'vtls'/A/'data-locked';marker.parent.mkdir(parents=True);marker.write_text('{}')
        with self.assertRaises(ValueError):self.migration.prepare(self.root/'backup','sha256:'+'a'*64)
        self.assertEqual((self.media/'TAPE01/data').read_bytes(),b'preserve');self.assertTrue(marker.exists())
        self.assertFalse((self.state/'storage-isolated').exists())

    def test_changed_catalogue_after_preparation_refuses(self):
        manifest=self.migration.prepare(self.root/'backup','sha256:'+'a'*64)
        self.db.execute('INSERT INTO users VALUES("new")');self.db.commit()
        with self.assertRaises(ValueError):self.migration.apply(manifest)
        self.assertFalse((self.state/'storage-isolated').exists())

    def test_interrupted_mode_switch_recovers_from_its_journal(self):
        manifest=self.migration.prepare(self.root/'backup','sha256:'+'a'*64)
        real=self.module.atomic
        def interrupt(path,value):
            real(path,value)
            if Path(path).name=='storage-isolated':raise SystemExit('power loss')
        with patch.object(self.module,'atomic',side_effect=interrupt):
            with self.assertRaises(SystemExit):self.migration.apply(manifest)
        restarted=self.module.Migration(self.config,stopped=lambda:True)
        restarted.apply(manifest)
        self.assertEqual(json.loads(manifest.read_text())['phase'],'complete')
        self.assertEqual(self.db.execute('SELECT name FROM users').fetchone()[0],'preserved')
