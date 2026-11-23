import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
import uuid
from test_maintenance import module, FakeController, FakeGuard, pbs

reint = module('reintegration', 'cold-vtl/reintegration-controller.py')


class ReintegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.media = self.root / 'media'; self.media.mkdir()
        self.restores = self.root / 'restores'; self.restores.mkdir()
        self.current = self.media / 'TAPE01'; self.current.mkdir()
        self.source = self.restores / 'restored'; self.source.mkdir()
        for name in ['mam', 'data.0', 'indx.0', 'meta.0', 'mhvtl_data']:
            (self.current / name).write_bytes(b'original-' + name.encode())
            (self.source / name).write_bytes(b'restored-' + name.encode())
        self.original = reint.inventory(self.current)
        self.receipt = {'status':'restored','barcode':'TAPE01','generation':2,'manifest_sha256':'a'*64,'files':reint.inventory(self.source)}
        self.guard = FakeGuard(self.root)
        self.controller = FakeController(self.root, self.guard)
        self.instance = reint.Reintegration({'media_root':str(self.media),'restore_root':str(self.restores)}, self.root, self.controller)
        self.restore_id = str(uuid.uuid4()); self.job_id = str(uuid.uuid4())
        with self.instance.db() as db:
            db.executescript('CREATE TABLE jobs(id TEXT,kind TEXT,subject TEXT,status TEXT,generation INTEGER,destination TEXT,message TEXT); CREATE TABLE cartridges(barcode TEXT,generation INTEGER,bytes_written INTEGER,state TEXT,updated_at TEXT); CREATE TABLE remote_generations(barcode TEXT,generation INTEGER,metadata TEXT);')
            db.execute('INSERT INTO cartridges VALUES (?,?,?,?,NULL)', ('TAPE01',1,100,'writable'))
            db.execute('INSERT INTO remote_generations VALUES (?,?,?)', ('TAPE01',2,json.dumps({'manifest_sha256':'a'*64,'local_match':False})))
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?)',(self.restore_id,'restore','TAPE01','completed',2,str(self.source),json.dumps(self.receipt)))
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?)',(self.job_id,'reintegrate','TAPE01','running',2,None,None))
        self.request = {'job_id':self.job_id,'restore_job_id':self.restore_id,'actor':'admin','replace':True}

    def tearDown(self):
        if self.guard.lock: self.guard.lock.close()
        self.temp.cleanup()

    def begin(self): return self.instance.begin(self.request)

    def rollback(self, journal):
        job_id = str(uuid.uuid4())
        with self.instance.db() as db:
            db.execute("UPDATE jobs SET status='failed' WHERE status='running'")
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?)',(job_id,'rollback','TAPE01','running',1,None,None))
        return self.instance.rollback({'job_id':job_id,'operation_id':journal['id'],'actor':'recovery-admin'})

    def assert_restored_original(self):
        self.assertEqual(reint.inventory(self.current), self.original)
        self.assertIsNone(self.guard.lease)
        self.assertFalse(self.instance.pending.exists())
        self.assertEqual(set(self.controller.states.values()), {'active'})
        with self.instance.db() as db:
            row = db.execute('SELECT generation,state FROM cartridges').fetchone()
            self.assertEqual(tuple(row), (1,'writable'))

    def test_full_install_and_rollback_preserve_both_cartridges(self):
        journal = self.instance.install(self.begin())
        self.assertEqual(reint.inventory(self.current), self.receipt['files'])
        self.assertEqual(reint.inventory(Path(journal['backup'])), self.original)
        self.assertEqual(reint.inventory(self.source), self.receipt['files'])
        self.assertEqual(journal['phase'],'completed')
        self.assertIsNone(self.guard.lease)
        result = self.rollback(journal)
        self.assert_restored_original()
        self.assertEqual(reint.inventory(Path(result['retained_replacement'])),self.receipt['files'])

    def test_tampered_source_never_enters_maintenance(self):
        (self.source/'data.0').write_bytes(b'changed')
        with self.assertRaisesRegex(RuntimeError,'changed'): self.begin()
        self.assertFalse(self.controller.commands)
        self.assertFalse(self.instance.pending.exists())

    def test_symlinks_incomplete_receipt_and_wrong_identity_refused(self):
        (self.source/'.cold-vtl-incomplete').write_text('incomplete')
        with self.assertRaises(RuntimeError): self.begin()
        (self.source/'.cold-vtl-incomplete').unlink()
        (self.source/'data.0').unlink(); (self.source/'data.0').symlink_to(self.current/'data.0')
        with self.assertRaises(RuntimeError): self.begin()
        self.assertEqual(reint.inventory(self.current),self.original)

    def test_crash_between_renames_is_recoverable_after_restart(self):
        journal = self.begin()
        real_move = reint.move
        def crash(source,target):
            real_move(source,target)
            if source == self.current: raise SystemExit('power loss after original saved')
        with patch.object(reint,'move',side_effect=crash):
            with self.assertRaises(SystemExit): self.instance.install(journal)
        self.assertFalse(self.current.exists())
        self.assertTrue(self.controller.block.exists())
        self.assertIsNotNone(self.guard.lease)
        # Reconstruct journal reader as a freshly restarted daemon would.
        self.instance = reint.Reintegration({'media_root':str(self.media),'restore_root':str(self.restores)},self.root,self.controller)
        saved = self.instance.status()['operations'][0]
        self.rollback(saved)
        self.assert_restored_original()

    def test_crash_after_install_before_catalogue_update(self):
        journal = self.begin()
        real_move = reint.move
        def crash(source,target):
            real_move(source,target)
            if target == self.current: raise SystemExit('power loss after replacement')
        with patch.object(reint,'move',side_effect=crash):
            with self.assertRaises(SystemExit): self.instance.install(journal)
        self.assertEqual(reint.inventory(self.current),self.receipt['files'])
        self.rollback(journal)
        self.assert_restored_original()

    def test_resume_failure_holds_pbs_and_rollback_recovers(self):
        journal = self.begin()
        self.controller.fail = ('start','vtltape@11.service')
        with self.assertRaises(RuntimeError): self.instance.install(journal)
        self.assertEqual(set(self.controller.states.values()),{'inactive'})
        self.assertIsNotNone(self.guard.lease)
        self.controller.fail = None
        self.rollback(journal)
        self.assert_restored_original()

    def test_partial_stop_and_busy_pbs_do_not_replace_media(self):
        journal = self.begin(); self.guard.busy = True
        with self.assertRaises(RuntimeError): self.instance.install(journal)
        self.assertEqual(reint.inventory(self.current),self.original)
        self.guard.busy = False
        self.rollback(journal)
        self.assert_restored_original()

    def test_copy_failure_and_disk_full_keep_original(self):
        journal = self.begin()
        with patch.object(reint.shutil,'copytree',side_effect=OSError('disk full')):
            with self.assertRaises(OSError): self.instance.install(journal)
        self.assertEqual(reint.inventory(self.current),self.original)
        self.rollback(journal)
        self.assert_restored_original()

    def test_ordinary_maintenance_cannot_resume_an_unresolved_reintegration(self):
        journal = self.begin()
        self.instance.hold(journal)
        self.controller.reintegrating = False
        with self.assertRaisesRegex(RuntimeError,'Reintegration'): self.controller.leave('operator')
        self.assertFalse(self.controller.status()['ready'])
        self.assertTrue(self.controller.block.exists())

    def test_active_jobs_and_tampered_backup_refused(self):
        with self.instance.db() as db:
            db.execute('INSERT INTO jobs VALUES (?,?,?,?,?,?,?)',('other','export','TAPE02','running',1,None,None))
        with self.assertRaisesRegex(RuntimeError,'jobs'): self.begin()
        with self.instance.db() as db: db.execute("DELETE FROM jobs WHERE id='other'")
        journal = self.instance.install(self.begin())
        (Path(journal['backup'])/'mam').write_text('tamper')
        with self.assertRaisesRegex(RuntimeError,'previous copy'): self.rollback(journal)
        self.assertEqual(reint.inventory(self.current),self.receipt['files'])
        self.assertTrue(self.controller.block.exists())

    def test_partial_service_stop_recovers_without_starting_missing_media(self):
        journal = self.begin()
        self.controller.fail = ('stop','vtltape@11.service')
        with self.assertRaises(RuntimeError): self.instance.install(journal)
        self.controller.fail = None
        self.rollback(journal)
        self.assert_restored_original()

    def test_crash_during_rollback_can_be_retried_without_overwriting_either_copy(self):
        journal = self.instance.install(self.begin())
        real_move = reint.move
        def crash(source,target):
            real_move(source,target)
            if target.name == 'replaced-on-rollback': raise SystemExit('power loss during rollback')
        with patch.object(reint,'move',side_effect=crash):
            with self.assertRaises(SystemExit): self.rollback(journal)
        self.assertFalse(self.current.exists())
        self.rollback(journal)
        self.assert_restored_original()

    def test_missing_lease_prevents_any_file_swap(self):
        journal = self.begin()
        self.instance.hold(journal)
        self.guard.lock.close(); self.guard.lock = None; self.guard.lease = None
        with self.assertRaises(RuntimeError): self.instance.install(journal)
        self.assertEqual(reint.inventory(self.current),self.original)
        self.assertTrue(self.controller.block.exists())

    def test_final_marker_cleanup_is_recoverable(self):
        journal = self.instance.install(self.begin())
        result = self.rollback(journal)
        reint.maintenance.atomic_json(self.instance.pending, {'id':journal['id']})
        self.rollback(result)
        self.assert_restored_original()

    def test_pbs_verification_requires_exactly_one_matching_slot(self):
        guard = pbs.Guard({'changer':'cold-vtl'})
        empty = {'entry-kind':'drive','entry-id':0}
        slot = {'entry-kind':'slot','entry-id':1,'label-text':'TAPE01'}
        for inventory in [[empty], [empty,slot,slot]]:
            with patch.object(pbs,'run',return_value=json.dumps(inventory)):
                with self.assertRaisesRegex(RuntimeError,'missing or duplicated'): guard.media_empty('TAPE01')
        with patch.object(pbs,'run',return_value=json.dumps([empty,slot])):
            guard.media_empty('TAPE01')

    def test_rollback_preserves_later_remote_inventory_but_clears_stale_local_matches(self):
        journal = self.instance.install(self.begin())
        with self.instance.db() as db:
            db.execute('INSERT INTO remote_generations VALUES (?,?,?)', ('TAPE01',3,json.dumps({'manifest_sha256':'b'*64,'local_match':True})))
        self.rollback(journal)
        with self.instance.db() as db:
            rows = db.execute('SELECT generation,metadata FROM remote_generations ORDER BY generation').fetchall()
        self.assertEqual([r[0] for r in rows],[2,3])
        self.assertTrue(all(not json.loads(r[1])['local_match'] for r in rows))



class VeeamReintegrationTests(unittest.TestCase):
    def test_veeam_reintegration_requires_existing_maintenance(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            controller = FakeController(root, FakeGuard(root))
            controller.config['client'] = 'veeam'
            instance = reint.Reintegration({'media_root':str(root/'media'),'restore_root':str(root/'restores')}, root, controller)
            with self.assertRaisesRegex(RuntimeError, 'Veeam'):
                instance.hold({'actor':'admin','barcode':'TAPE01'})
            self.assertEqual(controller.commands, [])
            self.assertFalse(controller.path.exists())
            self.assertFalse(controller.block.exists())

if __name__ == '__main__': unittest.main()
