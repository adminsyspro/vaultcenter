import json
import os
from pathlib import Path
import sqlite3
import sys
import unittest
from unittest.mock import patch
import test_cartridges as fixtures

sys.path.insert(0, str(Path(__file__).parents[1]/'cold-vtl'))


class BatchTests(unittest.TestCase):
    setUp = fixtures.CartridgeTests.setUp
    count = fixtures.CartridgeTests.count

    def mktape(self, args):
        fixtures.CartridgeTests.mktape(self,args)
        path=Path(args[args.index('-H')+1])/args[args.index('-m')+1]/'mam'
        path.write_bytes(path.read_bytes().replace(b'NEW001L8',args[args.index('-m')+1].encode()))

    def request_batch(self, **changes):
        return {**{k:v for k,v in self.request.items() if k!='barcode'},'request_id':'a'*32,'count':2,**changes}

    def step(self, **changes):
        return self.instance.handle('batch',self.request_batch(**changes))

    def test_sequential_batch_resumes_without_duplicates(self):
        self.controller.enter('admin')
        first=self.step()
        self.assertEqual(first['completed'],1)
        self.assertEqual(first['barcodes'],['CVT001L8','CVT002L8'])
        options=self.instance.handle('options',self.request)
        self.assertEqual(options['pending_batch']['completed'],1)
        with self.assertRaisesRegex(RuntimeError,'batch'):
            self.instance.handle('create',self.request)
        last=self.step()
        self.assertEqual(last['completed'],2)
        self.assertEqual(last['phase'],'completed')
        self.assertEqual(self.step(),last)
        self.assertEqual(self.count(),2)
        self.assertEqual(len(self.calls),2)

    def test_options_report_volume_size_for_the_header_gauge(self):
        stats=os.statvfs(self.instance.media)
        storage=self.instance.handle('options',self.request)['storage']
        self.assertEqual(storage['total_bytes'],stats.f_blocks*stats.f_frsize)
        self.assertGreaterEqual(storage['total_bytes'],storage['free_bytes'])

    def test_insufficient_space_refuses_before_reservation_or_media(self):
        self.controller.enter('admin')
        with patch('cartridge_batches.free_bytes',return_value=3*1024**3):
            with self.assertRaisesRegex(RuntimeError,'space'):
                self.step()
        self.assertEqual(self.calls,[])
        self.assertFalse((self.root/'cartridge-batch-pending.json').exists())

    def test_existing_capacity_and_other_vtl_reservations_share_volume(self):
        self.controller.enter('admin')
        self.instance.scoped=self.instance.vtl_id
        other=self.root/'other';other.mkdir();(other/'OLD001L8').mkdir()
        with sqlite3.connect(self.root/'catalog.db') as db:
            db.execute('ALTER TABLE jobs ADD COLUMN vtl_id TEXT')
            db.execute('INSERT INTO vtls VALUES (?,?,?)',('other',str(other),'ready'))
            db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES ('other','OLD001L8',?,'writable')",(4*1024**3,))
        with patch('cartridge_batches.free_bytes',return_value=6*1024**3):
            with self.assertRaisesRegex(RuntimeError,'space'):self.step()
        self.assertEqual(self.calls,[])

    def test_invalid_count_and_changed_retry_do_not_create(self):
        self.controller.enter('admin')
        for n in (0,-1,True,3):
            with self.subTest(n=n),self.assertRaises(RuntimeError):self.step(count=n)
        self.step()
        with self.assertRaises(RuntimeError):self.step(count=1)
        self.assertEqual(self.count(),1)

    def test_resume_after_catalogue_commit_does_not_repeat_mktape(self):
        self.controller.enter('admin')
        real=self.instance.catalogue
        def crash(request):
            real(request)
            raise SystemExit('power loss')
        with patch.object(self.instance,'catalogue',side_effect=crash):
            with self.assertRaises(SystemExit):self.step()
        self.assertEqual(self.step()['completed'],1)
        self.assertEqual(self.step()['completed'],2)
        self.assertEqual(len(self.calls),2)

    def test_stale_files_do_not_pay_for_remaining_tape_capacity(self):
        import cartridge_batches as batches
        self.controller.enter('admin');self.step();self.step()
        before=batches.storage_budget(self.instance)['reserved_bytes']
        (self.media/'CVT001L8'/'data.0.backup').write_bytes(b'x'*1024**2)
        self.assertEqual(batches.storage_budget(self.instance)['reserved_bytes'],before)

    def test_allocator_cannot_observe_gap_during_single_catalogue_commit(self):
        import fcntl
        self.controller.enter('admin')
        real=self.instance.catalogue
        def commit(request):
            row=real(request)
            with (self.root/'cartridge-allocation.lock').open('a+') as other:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(other,fcntl.LOCK_EX|fcntl.LOCK_NB)
            return row
        with patch.object(self.instance,'catalogue',side_effect=commit):
            self.instance.handle('create',self.request)

    def test_unknown_missing_other_storage_refuses_capacity_planning(self):
        self.instance.scoped=self.instance.vtl_id
        self.controller.enter('admin')
        with sqlite3.connect(self.root/'catalog.db') as db:
            db.execute('ALTER TABLE jobs ADD COLUMN vtl_id TEXT')
            db.execute('INSERT INTO vtls VALUES (?,?,?)',('other',str(self.root/'missing'),'ready'))
            db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES ('other','OLD001L8',4294967296,'writable')")
        with self.assertRaisesRegex(RuntimeError,'volume'):
            self.step()
        self.assertEqual(self.calls,[])

    def test_exact_budget_can_complete_after_metadata_is_written(self):
        import cartridge_batches as batches
        self.controller.enter('admin')
        allowance=2*(self.request['capacity_bytes']+batches.OVERHEAD)
        def available(_):
            used=sum(p.stat().st_blocks*512 for tape in self.media.iterdir() if tape.is_dir() and not tape.name.startswith('.') for p in tape.iterdir() if p.is_file())
            return allowance-used
        with patch('cartridge_batches.free_bytes',side_effect=available):
            self.step()
            self.assertEqual(self.step()['phase'],'completed')

    def test_completed_numbers_are_not_reused_after_deletion(self):
        self.controller.enter('admin')
        self.step();self.step()
        with sqlite3.connect(self.root/'catalog.db') as db:db.execute('DELETE FROM cartridges')
        self.contents.write_text('Drive 1:\nPicker 1:\nMAP 1:\nSlot 1:\nSlot 2:\n')
        result=self.step(request_id='b'*32,count=1)
        self.assertEqual(result['barcodes'],['CVT003L8'])


if __name__=='__main__':unittest.main()
