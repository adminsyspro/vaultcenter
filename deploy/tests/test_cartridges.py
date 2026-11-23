import fcntl
import json
from pathlib import Path
import sqlite3
import struct
import tempfile
import unittest
from unittest.mock import patch
from test_maintenance import module, FakeController, FakeGuard

PATH = Path(__file__).parents[1] / 'cold-vtl/cartridge-controller.py'


class CartridgeTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(PATH.exists(), 'cartridge creation controller must exist')
        self.mod = module('cartridges', 'cold-vtl/cartridge-controller.py')
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.media = self.root / 'media'; self.media.mkdir()
        self.config_dir = self.root / 'config'; self.config_dir.mkdir()
        self.device = self.config_dir / 'device.conf'
        self.device.write_text('VERSION: 5\n\nLibrary: 10 CHANNEL: 00 TARGET: 00 LUN: 00\n Home directory: '+str(self.media)+'\n\nDrive: 11 CHANNEL: 00 TARGET: 01 LUN: 00\n Library ID: 10 Slot: 01\n Vendor identification: IBM\n Product identification: ULT3580-TD8\n')
        self.contents = self.config_dir / 'library_contents.10'
        self.contents.write_text('# retained comment\nDrive 1:\nPicker 1:\nMAP 1:\nSlot 1: EXIST001\nSlot 2:\nSlot 3:\n')
        self.guard = FakeGuard(self.root)
        self.controller = FakeController(self.root, self.guard)
        self.addCleanup(lambda: self.guard.lock.close() if self.guard.lock else None)
        self.instance = self.mod.Cartridges({'media_root': str(self.media), 'mhvtl_config': str(self.config_dir)}, self.root, self.controller, self.mktape)
        with sqlite3.connect(self.root/'catalog.db') as db:
            db.executescript('CREATE TABLE vtls(id TEXT, media_root TEXT, lifecycle TEXT); CREATE TABLE cartridges(vtl_id TEXT, barcode TEXT, capacity_bytes INTEGER, bytes_written INTEGER DEFAULT 0, generation INTEGER DEFAULT 1, state TEXT, media_type TEXT, segment_size_mib INTEGER DEFAULT 256, multipart_part_size_mib INTEGER DEFAULT 256, UNIQUE(vtl_id,barcode)); CREATE TABLE jobs(status TEXT); CREATE TABLE cartridge_deletions(vtl_id TEXT,barcode TEXT,request_id TEXT,deleted_at TEXT DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(vtl_id,barcode));')
            db.execute('INSERT INTO vtls VALUES (?,?,?)', ('00000000-0000-4000-8000-000000000001', str(self.media), 'ready'))
        self.request = {'vtl_id': '00000000-0000-4000-8000-000000000001', 'barcode': 'NEW001L8', 'media_type': 'LTO8', 'capacity_bytes': 2147483648, 'segment_size_mib': 256, 'multipart_part_size_mib': 256}
        self.calls = []

    def mktape(self, args):
        self.calls.append(args)
        target = Path(args[args.index('-H')+1]) / args[args.index('-m')+1]
        target.mkdir()
        attrs = {0: (2147483648).to_bytes(8, 'big'), 1: (2147483648).to_bytes(8, 'big'), 0x405: b'\x5e', 0x408: b'\x00', 0x806: b'NEW001L8'.ljust(31, b' ')+b'\x00', 0x1623: b'\x00'}
        def tlv(values):
            return b''.join(struct.pack('=HH', k, len(v))+v for k,v in values.items())
        (target/'mam').write_bytes(struct.pack('=II', 6, 4)+tlv(attrs))
        (target/'mhvtl_data').write_bytes(tlv({8: b'U-832  \0', 9: b'Ultrium 8/32T'.ljust(32,b'\0')}))
        (target/'data.0').write_bytes(b'')
        (target/'indx.0').write_bytes(b'')
        (target/'meta.0').write_bytes(bytes(512))

    def create(self):
        return self.instance.handle('create', self.request)

    def count(self):
        with sqlite3.connect(self.root/'catalog.db') as db:
            return db.execute('SELECT count(*) FROM cartridges').fetchone()[0]

    def test_options_show_compatibility_before_maintenance(self):
        result = self.instance.handle('options', self.request)
        self.assertEqual(result['media_types'], ['LTO7', 'LTO8'])
        self.assertEqual(result['free_slots'], 2)
        self.assertFalse(result['ready'])
        self.assertTrue(result['reason'])
        self.assertFalse(self.instance.journals.exists())
        self.assertEqual(list(self.media.iterdir()), [])
        self.controller.enter('admin')
        self.assertTrue(self.instance.handle('options', self.request)['ready'])

    def test_large_segments_and_multipart_geometry_are_persisted(self):
        self.controller.enter('admin')
        self.request.update(segment_size_mib=1048576, multipart_part_size_mib=256)
        result = self.create()
        self.assertEqual(result['segment_size_mib'],1048576)
        self.assertEqual(result['multipart_part_size_mib'],256)
        self.assertEqual(self.create(),result)
        self.request['multipart_part_size_mib'] = 512
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(len(self.calls),1)

    def test_multipart_geometry_refused_before_any_media_write(self):
        self.controller.enter('admin')
        for segment,part in [(1048577,256),(160001,16),(1048576,16),(256,15),(256,1025),(256,True)]:
            with self.subTest(segment=segment,part=part):
                request = {**self.request,'segment_size_mib':segment,'multipart_part_size_mib':part}
                with self.assertRaises(RuntimeError): self.instance.handle('create',request)
        self.assertEqual(self.calls,[])

    def test_old_journal_resumes_with_default_parts_without_changing_segment_choice(self):
        self.controller.enter('admin')
        real_catalogue = self.instance.catalogue
        with patch.object(self.instance,'catalogue',side_effect=SystemExit('power loss')):
            with self.assertRaises(SystemExit): self.create()
        path = self.instance.journals/'NEW001L8.json'
        journal = json.loads(path.read_text())
        journal['request'].pop('multipart_part_size_mib',None)
        path.write_text(json.dumps(journal))
        marker = self.media/'NEW001L8'/'.vaultcenter-create-pending'
        recorded = json.loads(marker.read_text())
        recorded.pop('multipart_part_size_mib',None)
        marker.write_text(json.dumps(recorded))
        options = self.instance.handle('options',self.request)
        self.assertEqual(options['pending_creation']['multipart_part_size_mib'],256)
        self.assertEqual(options['pending_creation']['segment_size_mib'],256)
        self.request['multipart_part_size_mib'] = 256
        result = self.create()
        self.assertEqual(result['multipart_part_size_mib'],256)
        self.assertEqual(result['segment_size_mib'],256)
        self.assertEqual(len(self.calls),1)

    def test_creation_installs_physical_media_slot_and_catalogue(self):
        self.controller.enter('admin')
        result = self.create()
        self.assertEqual((result['media_type'], result['segment_size_mib'], result['capacity_bytes']), ('LTO8',256,2147483648))
        self.assertTrue((self.media/'NEW001L8'/'mam').exists())
        self.assertFalse((self.media/'NEW001L8'/'.vaultcenter-create-pending').exists())
        self.assertIn('Slot 2: NEW001L8\n', self.contents.read_text())
        self.assertIn('# retained comment\n', self.contents.read_text())
        self.assertEqual(self.count(), 1)
        self.assertEqual(self.controller.read()['phase'], 'maintenance')
        self.assertEqual(self.create(), result)
        self.assertEqual(len(self.calls), 1)
        self.assertIn('-H', self.calls[0])

    def test_create_requires_maintenance_pbs_and_idle_workers_jobs(self):
        with self.assertRaises(RuntimeError): self.create()
        self.controller.enter('admin')
        self.controller.offline = True
        with self.assertRaises(RuntimeError): self.create()
        self.controller.offline = False
        self.controller.worker = True
        with self.assertRaises(RuntimeError): self.create()
        self.controller.worker = False
        with sqlite3.connect(self.root/'catalog.db') as db: db.execute("INSERT INTO jobs VALUES ('running')")
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(self.calls, [])

    def test_validation_and_other_vtl_cannot_create_or_select_paths(self):
        self.controller.enter('admin')
        for key, value in [('vtl_id','00000000-0000-4000-8000-000000000003'), ('barcode','../BAD'), ('media_type','LTO6'), ('capacity_bytes',1), ('capacity_bytes',True), ('segment_size_mib',15), ('segment_size_mib',1048577)]:
            with self.subTest(key=key,value=value):
                request = {**self.request,key:value}
                with self.assertRaises(RuntimeError): self.instance.handle('create', request)
        self.assertEqual(self.calls, [])

    def test_existing_media_slots_symlinks_and_unknown_drives_refused(self):
        self.controller.enter('admin')
        target = self.media/'NEW001L8'; target.mkdir()
        (target/'precious').write_text('keep')
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual((target/'precious').read_text(),'keep')
        (target/'precious').unlink(); target.rmdir()
        target.symlink_to(self.root/'missing')
        with self.assertRaises(RuntimeError): self.create()
        target.unlink()
        self.device.write_text(self.device.read_text().replace('ULT3580-TD8','UNKNOWN'))
        result = self.instance.handle('options', self.request)
        self.assertFalse(result['ready'])
        self.assertEqual(result['media_types'], [])

    def test_hpe_ultrium_8_is_a_validated_drive(self):
        self.device.write_text(self.device.read_text().replace('Vendor identification: IBM','Vendor identification: HPE').replace('ULT3580-TD8','Ultrium 8-SCSI'))
        self.assertEqual(self.instance.layout()[0],['LTO7','LTO8'])

    def test_partial_mktape_never_registers_or_overwrites_on_retry(self):
        self.controller.enter('admin')
        def partial(args):
            self.mktape(args)
            (Path(args[args.index('-H')+1])/'NEW001L8'/'mam').write_bytes(b'broken')
            raise RuntimeError('mktape failed')
        self.instance.runner = partial
        with self.assertRaises(RuntimeError): self.create()
        self.instance.runner = self.mktape
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(len(self.calls),1)
        self.assertEqual(self.count(),0)
        self.assertNotIn('Slot 2: NEW001L8',self.contents.read_text())
        with self.assertRaises(RuntimeError): self.controller.leave('admin')

    def test_crash_after_mktape_is_recovered_without_recreating_media(self):
        self.controller.enter('admin')
        def crash(args):
            self.mktape(args)
            raise SystemExit('power loss')
        self.instance.runner = crash
        with self.assertRaises(SystemExit): self.create()
        self.instance.runner = self.mktape
        self.create()
        self.assertEqual(len(self.calls),1)
        self.assertEqual(self.count(),1)

    def test_recovery_rejects_changed_parameters(self):
        self.controller.enter('admin')
        def crash(args):
            self.mktape(args)
            raise SystemExit('power loss')
        self.instance.runner = crash
        with self.assertRaises(SystemExit): self.create()
        self.request['segment_size_mib'] = 512
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(self.count(),0)

    def test_crash_after_placement_retries_catalogue_only(self):
        self.controller.enter('admin')
        original = self.instance.catalogue
        with patch.object(self.instance,'catalogue',side_effect=SystemExit('power loss')):
            with self.assertRaises(SystemExit): self.create()
        self.assertIn('Slot 2: NEW001L8',self.contents.read_text())
        self.assertEqual(self.count(),0)
        self.assertTrue((self.media/'NEW001L8'/'.vaultcenter-create-pending').exists())
        options = self.instance.handle('options', self.request)
        self.assertEqual(options['pending_creation'], {k:self.request[k] for k in ['barcode','media_type','capacity_bytes','segment_size_mib','multipart_part_size_mib']})
        self.assertTrue(options['ready'])
        self.create()
        self.assertEqual(len(self.calls),1)
        self.assertEqual(self.count(),1)

    def test_pending_options_recover_last_slot_after_catalogue_commit(self):
        self.contents.write_text('Drive 1:\nPicker 1:\nMAP 1:\nSlot 1:\n')
        self.controller.enter('admin')
        real_save = self.instance.save
        def crash(journal):
            if journal['phase'] == 'completed': raise SystemExit('power loss')
            real_save(journal)
        with patch.object(self.instance,'save',side_effect=crash):
            with self.assertRaises(SystemExit): self.create()
        options = self.instance.handle('options', self.request)
        self.assertEqual(options['free_slots'],0)
        self.assertTrue(options['ready'])
        self.assertEqual(options['pending_creation']['barcode'],'NEW001L8')
        self.assertEqual(self.count(),1)
        self.create()
        self.assertEqual(self.count(),1)
        self.assertEqual(len(self.calls),1)

    def test_maintenance_lock_serializes_create(self):
        self.controller.enter('admin')
        with (self.root/'maintenance-controller.lock').open('a+') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(self.calls,[])

    def test_changed_slot_during_creation_is_never_overwritten(self):
        self.controller.enter('admin')
        def changed(args):
            self.mktape(args)
            self.contents.write_text(self.contents.read_text().replace('Slot 2:', 'Slot 2: OTHER001'))
        self.instance.runner = changed
        with self.assertRaises(RuntimeError): self.create()
        self.assertIn('Slot 2: OTHER001',self.contents.read_text())
        self.assertEqual(self.count(),0)
        self.assertTrue(self.instance.pending.exists())

    def test_destination_created_during_rename_is_not_replaced(self):
        self.controller.enter('admin')
        real_rename = self.mod.rename_new
        def race(source,target):
            target.mkdir()
            real_rename(source,target)
        with patch.object(self.mod,'rename_new',side_effect=race):
            with self.assertRaises(OSError): self.create()
        self.assertEqual(list((self.media/'NEW001L8').iterdir()),[])
        self.assertEqual(self.count(),0)
        self.assertNotIn('Slot 2: NEW001L8',self.contents.read_text())

    def test_second_provisioned_vtl_and_root_mismatch_refuse_installed_adapter(self):
        self.controller.enter('admin')
        with self.instance.db() as db:
            db.execute('INSERT INTO vtls VALUES (?,?,?)', ('other',None,'draft'))
        self.assertTrue(self.instance.handle('options',self.request)['ready'])
        with self.instance.db() as db:
            db.execute("UPDATE vtls SET lifecycle='ready' WHERE id='other'")
        with self.assertRaises(RuntimeError): self.create()
        with self.instance.db() as db:
            db.execute("DELETE FROM vtls WHERE id='other'")
            db.execute("UPDATE vtls SET media_root='/wrong-root'")
        with self.assertRaises(RuntimeError): self.create()
        self.assertEqual(self.calls,[])


if __name__ == '__main__': unittest.main()
