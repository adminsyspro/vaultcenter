import json
from pathlib import Path
import shutil
import sqlite3
import struct
import tempfile
import unittest
from unittest.mock import patch
import uuid
from test_maintenance import module, FakeController, FakeGuard

reint = module('recovery_reintegration', 'cold-vtl/reintegration-controller.py')
recovery = reint.recovery_install
LEGACY = '00000000-0000-4000-8000-000000000001'


class RecoveryInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.media = self.root/'media'; self.media.mkdir()
        self.restores = self.root/'restores'; self.restores.mkdir()
        self.config = self.root/'config'; self.config.mkdir()
        (self.config/'device.conf').write_text('Library: 10\n Home directory: '+str(self.media)+'\nDrive: 11\n Library ID: 10 Slot: 01\n Vendor identification: IBM\n Product identification: ULT3580-TD8\n')
        self.contents = self.config/'library_contents.10'
        self.contents.write_text('Drive 1:\nPicker 1:\nMAP 1:\nSlot 1:\nSlot 2:\n')
        self.before = self.contents.read_text()
        self.guard = FakeGuard(self.root)
        self.controller = FakeController(self.root,self.guard)
        self.addCleanup(lambda:self.guard.lock.close() if self.guard.lock else None)
        self.instance = reint.Reintegration({'media_root':str(self.media),'restore_root':str(self.restores),'mhvtl_config':str(self.config)},self.root,self.controller)
        self.source = self.restores/'download';self.source.mkdir()
        attrs = {1:(1024**3).to_bytes(8,'big'),0x405:b'\x5e',0x806:b'TAPE01'}
        (self.source/'mam').write_bytes(struct.pack('=II',6,4)+b''.join(struct.pack('=HH',k,len(v))+v for k,v in attrs.items()))
        for name in ['mhvtl_data','data.0','indx.0','meta.0']:(self.source/name).write_bytes(b'recovered-'+name.encode())
        self.receipt = {'status':'restored','barcode':'TAPE01','generation':7,'manifest_sha256':'a'*64,'files':reint.inventory(self.source)}
        self.id = str(uuid.uuid4())
        self.selected = {'destination_id':'historical','barcode':'TAPE01','generation':7,'manifest_sha256':'a'*64,'target_vtl_id':LEGACY,'replace':False}
        self.request = {'job_id':self.id,'recovery_job_id':self.id,'actor':'admin'}
        with self.instance.db() as db:
            db.executescript('CREATE TABLE vtls(id TEXT,media_root TEXT,lifecycle TEXT); CREATE TABLE jobs(id TEXT,vtl_id TEXT,kind TEXT,subject TEXT,status TEXT,generation INTEGER,destination TEXT,destination_id TEXT,configuration_revision INTEGER); CREATE TABLE recovery_requests(job_id TEXT,target_job_id TEXT,source_vtl_id TEXT,target_vtl_id TEXT,request TEXT,receipt TEXT,phase TEXT); CREATE TABLE cartridges(vtl_id TEXT,barcode TEXT,capacity_bytes INTEGER,media_type TEXT,generation INTEGER,bytes_written INTEGER,state TEXT,updated_at TEXT); CREATE TABLE remote_generations(barcode TEXT,generation INTEGER,metadata TEXT,destination_id TEXT); CREATE TABLE cartridge_deletions(vtl_id TEXT,barcode TEXT,request_id TEXT,PRIMARY KEY(vtl_id,barcode));')
            db.execute('INSERT INTO vtls VALUES (?,?,?)',(LEGACY,str(self.media),'ready'))
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?)',(self.id,LEGACY,'recovery','TAPE01','running',7,str(self.source),'historical',3))
            db.execute('INSERT INTO recovery_requests VALUES (?,?,?,?,?,?,?)',(self.id,self.id,LEGACY,LEGACY,json.dumps(self.selected),json.dumps(self.receipt),'installing'))

    def begin(self):
        return recovery.begin(self.instance,self.request,reint.inventory,reint.directory,reint.maintenance.atomic_json)

    def rollback(self,journal):
        job = str(uuid.uuid4())
        with self.instance.db() as db:
            db.execute("UPDATE jobs SET status='failed'")
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?)',(job,LEGACY,'rollback','TAPE01','running',None,None,None,None))
        return self.instance.rollback({'job_id':job,'operation_id':journal['id'],'actor':'admin'})

    def test_empty_catalogue_installs_and_registers_slot_then_can_remove(self):
        preflight = recovery.check(self.instance,dict(self.selected,required_bytes=1024))
        self.assertTrue(preflight['can_restore']);self.assertEqual(preflight['local_identity'],'absent')
        journal = self.begin()
        with self.instance.db() as db:self.assertEqual(db.execute('SELECT count(*) FROM cartridges').fetchone()[0],0)
        result = self.instance.install(journal)
        self.assertEqual(result['phase'],'completed')
        self.assertIsNone(result['backup'])
        self.assertEqual(self.controller.expected_barcode,'TAPE01')
        self.assertIn('Slot 1: TAPE01',self.contents.read_text())
        self.assertEqual(reint.inventory(self.media/'TAPE01'),self.receipt['files'])
        with self.instance.db() as db:self.assertEqual(tuple(db.execute('SELECT generation,state,media_type FROM cartridges').fetchone()),(7,'verified','LTO8'))
        self.rollback(result)
        self.assertFalse((self.media/'TAPE01').exists());self.assertEqual(self.contents.read_text(),self.before)
        self.assertIsNone(self.guard.lease)
        with self.instance.db() as db:self.assertEqual(db.execute('SELECT count(*) FROM cartridge_deletions').fetchone()[0],1)

    def test_deleted_catalogue_row_is_not_misreported_present(self):
        with self.instance.db() as db:
            db.execute('INSERT INTO cartridges VALUES (?,?,?,?,?,?,?,NULL)',(LEGACY,'TAPE01',1024**3,'LTO8',1,100,'verified'))
            db.execute('INSERT INTO cartridge_deletions VALUES (?,?,?)',(LEGACY,'TAPE01','deleted'))
            db.execute('INSERT INTO remote_generations VALUES (?,?,?,?)',('TAPE01',7,json.dumps({'manifest_sha256':'a'*64,'local_match':False}),'historical'))
        result = self.instance.install(self.begin())
        with self.instance.db() as db:self.assertEqual(db.execute('SELECT count(*) FROM cartridge_deletions').fetchone()[0],0)
        self.rollback(result)
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT generation FROM cartridges').fetchone()[0],1)
            self.assertEqual(db.execute('SELECT count(*) FROM cartridge_deletions').fetchone()[0],1)
            self.assertFalse(json.loads(db.execute('SELECT metadata FROM remote_generations').fetchone()[0])['local_match'])

    def test_homonym_requires_separate_replacement_and_preserves_backup(self):
        shutil.copytree(self.source,self.media/'TAPE01');(self.media/'TAPE01'/'data.0').write_bytes(b'different old media')
        original = reint.inventory(self.media/'TAPE01')
        self.contents.write_text(self.before.replace('Slot 1:','Slot 1: TAPE01'))
        with self.instance.db() as db:db.execute('INSERT INTO cartridges VALUES (?,?,?,?,?,?,?,NULL)',(LEGACY,'TAPE01',1024**3,'LTO8',1,100,'writable'))
        check = recovery.check(self.instance,dict(self.selected,required_bytes=1024))
        self.assertFalse(check['can_restore']);self.assertTrue(check['conflict'])
        with self.assertRaisesRegex(RuntimeError,'same barcode'):self.begin()
        self.selected['replace'] = True
        with self.instance.db() as db:db.execute('UPDATE recovery_requests SET request=?',(json.dumps(self.selected),))
        result = self.instance.install(self.begin())
        self.assertEqual(reint.inventory(Path(result['backup'])),original)
        self.rollback(result)
        self.assertEqual(reint.inventory(self.media/'TAPE01'),original)

    def test_hash_or_source_destination_mismatch_refused_before_maintenance(self):
        for field,value in [('manifest_sha256','b'*64),('generation',8)]:
            receipt = dict(self.receipt,**{field:value})
            with self.instance.db() as db:db.execute('UPDATE recovery_requests SET receipt=?',(json.dumps(receipt),))
            with self.assertRaisesRegex(RuntimeError,'Integrity receipt'):self.begin()
        with self.instance.db() as db:
            db.execute('UPDATE recovery_requests SET receipt=?',(json.dumps(self.receipt),))
            db.execute("UPDATE jobs SET destination_id='foreign'")
        with self.assertRaisesRegex(RuntimeError,'not pinned'):self.begin()
        self.assertFalse(self.controller.commands)

    def test_interruption_before_and_after_slot_registration_rolls_back_without_local_row(self):
        journal = self.begin()
        original_move = reint.move
        def crash(source,target):
            original_move(source,target)
            if target == self.media/'TAPE01':raise SystemExit('crash before slot registration')
        with patch.object(reint,'move',side_effect=crash):
            with self.assertRaises(SystemExit):self.instance.install(journal)
        self.assertTrue(self.controller.block.exists())
        with self.instance.db() as db:self.assertEqual(db.execute('SELECT count(*) FROM cartridges').fetchone()[0],0)
        self.rollback(journal)
        self.assertFalse((self.media/'TAPE01').exists());self.assertEqual(self.contents.read_text(),self.before)
        self.assertIsNone(self.guard.lease)

    def test_slot_changes_before_maintenance_are_preserved_during_install_and_rollback(self):
        journal = self.begin()
        during_download = self.before.replace('Slot 2:', 'Slot 2: OTHER02')
        self.contents.write_text(during_download)
        installed = self.instance.install(journal)
        self.assertIn('Slot 2: OTHER02', self.contents.read_text())
        self.assertIn('Slot 1: TAPE01', self.contents.read_text())
        self.rollback(installed)
        self.assertEqual(self.contents.read_text(), during_download)
        self.assertIsNone(self.guard.lease)

    def test_completed_recovery_rollback_preserves_later_unrelated_slot_moves(self):
        installed = self.instance.install(self.begin())
        self.contents.write_text(self.contents.read_text().replace('Slot 2:', 'Slot 2: OTHER02'))
        self.rollback(installed)
        self.assertEqual(self.contents.read_text(), self.before.replace('Slot 2:', 'Slot 2: OTHER02'))
        self.assertIsNone(self.guard.lease)

    def test_rollback_before_slot_snapshot_does_not_rewrite_live_inventory(self):
        journal = self.begin()
        during_download = self.before.replace('Slot 2:', 'Slot 2: OTHER02')
        self.contents.write_text(during_download)
        self.rollback(journal)
        self.assertEqual(self.contents.read_text(), during_download)
        self.assertIsNone(self.guard.lease)

    def test_pbs_resume_failure_retains_guard_and_new_media_can_be_rolled_back(self):
        journal = self.begin();self.controller.fail = ('start','vtltape@11.service')
        with self.assertRaises(RuntimeError):self.instance.install(journal)
        self.assertIsNotNone(self.guard.lease)
        self.controller.fail = None
        self.rollback(journal)
        self.assertFalse((self.media/'TAPE01').exists());self.assertEqual(self.contents.read_text(),self.before)

    def test_cross_vtl_uses_source_pin_and_ignores_source_local_homonym(self):
        target = str(uuid.uuid4()); target_job = str(uuid.uuid4())
        target_root = self.root/'vtls'/target;target_root.mkdir(parents=True)
        source_root = self.restores/LEGACY;source_root.mkdir()
        self.source.rename(source_root/'download');self.source = source_root/'download'
        target_restores = self.restores/target;target_restores.mkdir()
        self.selected['target_vtl_id'] = target
        with self.instance.db() as db:
            db.execute('ALTER TABLE remote_generations ADD COLUMN vtl_id TEXT')
            db.execute('INSERT INTO vtls VALUES (?,?,?)',(target,str(self.media),'ready'))
            db.execute('INSERT INTO cartridges VALUES (?,?,?,?,?,?,?,NULL)',(LEGACY,'TAPE01',1024**3,'LTO8',1,100,'writable'))
            db.execute('UPDATE jobs SET destination=?',(str(self.source),))
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?)',(target_job,target,'recovery_install','TAPE01','running',7,None,None,None))
            db.execute('UPDATE recovery_requests SET target_job_id=?,target_vtl_id=?,request=?',(target_job,target,json.dumps(self.selected)))
        self.instance = reint.Reintegration({'vtl_id':target,'media_root':str(self.media),'restore_root':str(target_restores),'mhvtl_config':str(self.config),'database':str(self.root/'catalog.db')},target_root,self.controller)
        self.request['job_id'] = target_job
        result = self.instance.install(self.begin())
        self.assertEqual(result['source_vtl_id'],LEGACY)
        self.assertEqual(result['destination_id'],'historical')
        self.assertEqual(result['vtl_id'],target)
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT generation FROM cartridges WHERE vtl_id=?',(LEGACY,)).fetchone()[0],1)
            self.assertEqual(db.execute('SELECT generation FROM cartridges WHERE vtl_id=?',(target,)).fetchone()[0],7)

    def test_download_disk_shortage_reports_the_limiting_volume(self):
        required = 28 * 1024**3
        with patch.object(recovery.shutil, 'disk_usage', side_effect=[
            shutil._ntuple_diskusage(40*1024**4, 27*1024**4, 13*1024**4),
            shutil._ntuple_diskusage(30*1024**3, 10*1024**3, 20*1024**3),
        ]):
            check = recovery.check(self.instance, dict(self.selected, required_bytes=required, download_root=str(self.restores)))
        self.assertFalse(check['space_ok'])
        self.assertIn('download', check['reason'])
        self.assertEqual(check['download_available_bytes'], 20*1024**3)
        self.assertEqual(check['download_required_bytes'], required+16*1024**2)
        self.assertEqual(check['installation_required_bytes'], 2*required+16*1024**2)

    def test_space_slot_and_ambiguous_catalogue_fail_closed(self):
        self.contents.write_text(self.before.replace('Slot 1:','Slot 1: OTHER01').replace('Slot 2:','Slot 2: OTHER02'))
        self.assertFalse(recovery.check(self.instance,dict(self.selected,required_bytes=0))['can_restore'])
        self.contents.write_text(self.before)
        with patch.object(recovery.shutil,'disk_usage',return_value=shutil._ntuple_diskusage(100,100,0)):
            self.assertFalse(recovery.check(self.instance,dict(self.selected,required_bytes=1))['space_ok'])
        with self.instance.db() as db:db.execute('INSERT INTO cartridges VALUES (?,?,?,?,?,?,?,NULL)',(LEGACY,'TAPE01',1024**3,'LTO8',1,100,'writable'))
        self.assertTrue(recovery.check(self.instance,dict(self.selected,required_bytes=0))['conflict'])

if __name__ == '__main__':unittest.main()
