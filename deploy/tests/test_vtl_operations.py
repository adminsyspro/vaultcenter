"""Helpers must retain VTL ownership even for identical barcodes."""
import json
from pathlib import Path
import sqlite3
import unittest
from unittest.mock import patch
import test_cartridges as cartridge_tests
import test_reintegration as reintegration_tests
import test_archive as archive_tests
reint=reintegration_tests.reint

A='00000000-0000-4000-8000-000000000001'
B='00000000-0000-4000-8000-000000000003'

class VtlOperationTests(unittest.TestCase):
    def archive_fixture(self):
        case=archive_tests.ArchiveTests();case.setUp();self.addCleanup(case.tearDown)
        with case.instance.db() as db:
            for table in ('jobs','cartridges','archive_requests'):
                db.execute(f'ALTER TABLE {table} ADD COLUMN vtl_id TEXT DEFAULT "{A}"')
        case.instance.vtl_id=A
        return case

    def test_busy_archive_b_does_not_block_a(self):
        case=self.archive_fixture()
        foreign=case.job('submit')
        with case.instance.db() as db:
            db.execute("UPDATE jobs SET vtl_id=?,subject='other-bucket' WHERE id=?",(B,foreign['job_id']))
        case.instance.execute('submit',case.submit())
        with case.instance.db() as db:
            self.assertEqual(db.execute('SELECT status FROM jobs WHERE id=?',(foreign['job_id'],)).fetchone()[0],'queued')

    def test_foreign_archive_job_refusal_does_not_mark_b_failed(self):
        case=self.archive_fixture();foreign=case.job('submit')
        with case.instance.db() as db:
            db.execute("UPDATE jobs SET vtl_id=?,subject='other-bucket' WHERE id=?",(B,foreign['job_id']))
        with self.assertRaises(RuntimeError):case.instance.execute('submit',foreign)
        with case.instance.db() as db:
            self.assertEqual(db.execute('SELECT status FROM jobs WHERE id=?',(foreign['job_id'],)).fetchone()[0],'queued')
        self.assertEqual(case.calls,[])

    def test_archive_intent_cannot_cross_vtls(self):
        case=self.archive_fixture();request=case.submit()
        with case.instance.db() as db:
            db.execute('UPDATE archive_requests SET vtl_id=? WHERE job_id=?',(B,request['job_id']))
        with self.assertRaisesRegex(RuntimeError,'Confirmation missing'):
            case.instance.execute('submit',request)
        self.assertEqual(case.calls,[])

    def test_observer_revalidates_runtime_before_remote_work(self):
        case=self.archive_fixture()
        with patch.object(archive_tests.archive.maintenance,'revalidate',side_effect=RuntimeError('stale runtime')):
            with self.assertRaisesRegex(RuntimeError,'stale runtime'):case.instance.observe()
        self.assertEqual(case.calls,[])

    def cartridge_fixture(self):
        case=cartridge_tests.CartridgeTests();case.setUp();self.addCleanup(case.doCleanups)
        (case.root/'b-media').mkdir()
        with sqlite3.connect(case.root/'catalog.db') as db:
            db.execute('ALTER TABLE jobs ADD COLUMN vtl_id TEXT')
            db.execute('INSERT INTO vtls VALUES (?,?,?)',(B,str(case.root/'b-media'),'ready'))
            db.execute("INSERT INTO jobs VALUES('running',?)",(B,))
            db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES(?,?,42,'writable')",(B,'NEW001L8'))
        config={'vtl_id':A,'media_root':str(case.media),'mhvtl_config':str(case.config_dir),
                'runtime':{'library':{'id':10},'drives':[{'id':11}]}}
        case.instance=case.mod.Cartridges(config,case.root,case.controller,case.mktape)
        return case

    def test_same_barcode_stays_in_its_vtl_and_busy_b_does_not_block_a(self):
        case=self.cartridge_fixture();case.controller.enter('admin');case.create()
        with sqlite3.connect(case.root/'catalog.db') as db:
            rows=dict(db.execute('SELECT vtl_id,capacity_bytes FROM cartridges WHERE barcode=?',('NEW001L8',)))
        self.assertEqual(rows,{A:2147483648,B:42})
        self.assertTrue((case.media/'NEW001L8/mam').exists())

    def test_generated_mhvtl_inventory_header_allows_creation(self):
        case=self.cartridge_fixture()
        case.instance.contents.write_text('VERSION: 2\n'+case.instance.contents.read_text())
        case.controller.enter('admin');case.create()
        self.assertTrue((case.media/'NEW001L8/mam').exists())

    def test_data_lock_precedes_media_creation(self):
        case=self.cartridge_fixture();case.controller.enter('admin')
        original=case.instance.runner
        def create(args):
            self.assertTrue((case.root/'data-locked').exists())
            original(args)
        case.instance.runner=create;case.create()
        self.assertTrue((case.root/'data-locked').exists())

    def test_foreign_restore_job_is_not_used_by_reintegration(self):
        case=reintegration_tests.ReintegrationTests();case.setUp();self.addCleanup(case.tearDown)
        with case.instance.db() as db:
            for table in ('jobs','cartridges','remote_generations'):db.execute(f'ALTER TABLE {table} ADD COLUMN vtl_id TEXT DEFAULT "{A}"')
            db.execute('UPDATE jobs SET vtl_id=? WHERE id=?',(B,case.restore_id))
        instance=reint.Reintegration({'vtl_id':A,'media_root':str(case.media),'restore_root':str(case.restores)},case.root,case.controller)
        with self.assertRaises(RuntimeError):instance.begin(case.request)
        self.assertFalse(instance.pending.exists())
        self.assertEqual(reint.inventory(case.current),case.original)

if __name__=='__main__':unittest.main()

class WorkerRuntimeTests(unittest.TestCase):
    def test_configured_but_inactive_runtime_cannot_launch_worker(self):
        import test_vtl_controller as runtime_tests
        case=runtime_tests.VtlControllerTests();case.setUp();self.addCleanup(case.doCleanups)
        prepared=case.r.prepare(runtime_tests.A,1);case.r.apply(runtime_tests.A,prepared['token'],'admin')
        with self.assertRaisesRegex(ValueError,'activated'):
            case.m.worker_launch(runtime_tests.A,2,['bucket-status'],root=case.root)


    def test_runtime_revision_change_aborts_worker(self):
        import test_vtl_controller as runtime_tests
        case=runtime_tests.VtlControllerTests();case.setUp();self.addCleanup(case.doCleanups)
        prepared=case.r.prepare(runtime_tests.A,1);case.r.apply(runtime_tests.A,prepared['token'],'admin')
        with self.assertRaises(ValueError):
            case.m.worker_launch(runtime_tests.A,1,['bucket-status'],root=case.root)
        self.assertFalse(any(command[0]=='systemd-run' for command in case.commands))

class ReintegrationDestinationTests(unittest.TestCase):
    def test_reintegration_pins_destination_and_does_not_mark_other_copy_matching(self):
        case=reintegration_tests.ReintegrationTests();case.setUp();self.addCleanup(case.tearDown)
        with case.instance.db() as db:
            for table in ('jobs','cartridges','remote_generations'):
                db.execute(f'ALTER TABLE {table} ADD COLUMN vtl_id TEXT DEFAULT "{A}"')
            for table in ('jobs','remote_generations'):
                db.execute('ALTER TABLE '+table+' ADD COLUMN destination_id TEXT DEFAULT "old-bucket"')
            db.execute('INSERT INTO remote_generations VALUES(?,?,?,?,?)',('TAPE01',2,json.dumps({'manifest_sha256':'b'*64,'local_match':False}),A,'new-bucket'))
        instance=reint.Reintegration({'vtl_id':A,'media_root':str(case.media),'restore_root':str(case.restores)},case.root,case.controller)
        journal=instance.begin(case.request);instance.install(journal)
        with instance.db() as db:
            values={r['destination_id']:json.loads(r['metadata']) for r in db.execute('SELECT * FROM remote_generations')}
        self.assertTrue(values['old-bucket']['local_match'])
        self.assertFalse(values['new-bucket']['local_match'])
        self.assertEqual(values['new-bucket']['manifest_sha256'],'b'*64)
