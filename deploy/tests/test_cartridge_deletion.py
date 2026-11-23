import json
import sqlite3
from unittest.mock import patch
import unittest
import test_cartridges


class DeletionTests(unittest.TestCase):
    mktape = test_cartridges.CartridgeTests.mktape
    create = test_cartridges.CartridgeTests.create
    count = test_cartridges.CartridgeTests.count
    # Reuse real file/mhVTL fixtures without inheriting their test cases.
    def setUp(self):
        test_cartridges.CartridgeTests.setUp(self)
        self.controller.enter('admin')
        self.create()
        self.tape = self.media / 'NEW001L8'
        (self.tape / 'data.0').write_bytes(b'backup data' * 1000)
        with self.instance.db() as db:
            db.executescript('''                CREATE TABLE remote_generations(vtl_id TEXT, barcode TEXT, metadata TEXT);
                INSERT INTO remote_generations VALUES('00000000-0000-4000-8000-000000000001',
                'NEW001L8','{"generation":7,"local_match":true}');''')
        self.deletion = {'vtl_id': self.request['vtl_id'], 'request_id': 'a'*32,
                         'barcodes': ['NEW001L8'], 'confirmation': 'delete-local'}

    def delete(self):
        return self.instance.handle('delete-local', self.deletion)

    def test_local_files_and_slots_removed_but_catalogue_and_s3_links_survive(self):
        ledger = self.media / '.vaultcenter-media-10'
        ledger.write_text('VAULTCENTER_MEDIA1 10\n11111111-1111-4111-8111-111111111111 NEW001L8\n22222222-2222-4222-8222-222222222222 EXIST001\n')
        result = self.delete()
        self.assertEqual(result['phase'], 'completed')
        self.assertFalse(self.tape.exists())
        self.assertNotIn('NEW001L8', self.contents.read_text())
        self.assertIn('EXIST001', self.contents.read_text())
        self.assertNotIn('NEW001L8', ledger.read_text())
        self.assertIn('EXIST001', ledger.read_text())
        self.assertEqual(self.count(), 1)
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT count(*) FROM cartridge_deletions').fetchone()[0], 1)
            self.assertEqual(db.execute('SELECT metadata FROM remote_generations').fetchone()[0], '{"generation":7,"local_match":true}')
        self.assertEqual(self.delete(), result)
        with self.assertRaises(RuntimeError):
            self.create()

    def test_rejects_unconfirmed_foreign_or_duplicate_selection_without_mutation(self):
        for changes in ({'confirmation': ''}, {'vtl_id': 'foreign'}, {'barcodes': []},
                        {'barcodes': ['NEW001L8', 'NEW001L8']}, {'barcodes': ['../data']},
                        {'barcodes': ['NEW001L8', 'MISSING1']}):
            with self.subTest(changes=changes):
                with self.assertRaises(RuntimeError):
                    self.instance.handle('delete-local', {**self.deletion, **changes})
                self.assertTrue((self.tape / 'data.0').exists())

    def test_rejects_live_services_loaded_media_and_active_job(self):
        self.controller.leave('admin')
        with self.assertRaises(RuntimeError): self.delete()
        self.controller.enter('admin')
        before = self.contents.read_text()
        self.contents.write_text(before.replace('Drive 1:', 'Drive 1: NEW001L8'))
        with self.assertRaises(RuntimeError): self.delete()
        self.contents.write_text(before)
        with self.instance.db() as db: db.execute("INSERT INTO jobs VALUES ('running')")
        with self.assertRaises(RuntimeError): self.delete()
        self.assertTrue(self.tape.exists())

    def test_rejects_symlink_and_hardlink_before_deleting_any_file(self):
        external = self.root / 'keep'; external.write_text('keep me')
        link = self.tape / 'foreign'
        link.symlink_to(external)
        with self.assertRaises(RuntimeError): self.delete()
        link.unlink()
        import os
        os.link(external, link)
        with self.assertRaises(RuntimeError): self.delete()
        self.assertEqual(external.read_text(), 'keep me')
        self.assertTrue((self.tape / 'data.0').exists())

    def test_resumes_after_rename_and_blocks_services_until_finished(self):
        real_rename = self.mod.rename_new
        def interrupted(*args):
            real_rename(*args)
            raise SystemExit('power loss after rename')
        with patch.object(self.mod, 'rename_new', side_effect=interrupted):
            with self.assertRaises(SystemExit): self.delete()
        self.assertFalse(self.tape.exists())
        with self.assertRaises(RuntimeError): self.controller.leave('admin')
        with self.assertRaises(RuntimeError): self.create()
        with self.assertRaises(RuntimeError):
            self.instance.handle('delete-local', {**self.deletion, 'request_id': 'b'*32})
        self.assertEqual(self.delete()['phase'], 'completed')
        self.controller.leave('admin')

    def interrupt_after_catalogue(self):
        atomic = self.mod.maintenance.atomic_json
        def interrupted(path, value):
            atomic(path, value)
            if value.get('phase') == 'catalogued':
                raise SystemExit('power loss after catalogue')
        with patch.object(self.mod.maintenance, 'atomic_json', side_effect=interrupted):
            with self.assertRaises(SystemExit): self.delete()

    def test_changed_inventory_blocks_recovery_after_catalogue_commit(self):
        self.interrupt_after_catalogue()
        after = self.contents.read_text()
        self.contents.write_text(after.replace('Slot 2:', 'Slot 2: NEW001L8'))
        with self.assertRaises(RuntimeError): self.delete()
        self.contents.write_text(after)
        self.assertEqual(self.delete()['phase'], 'completed')

    def test_resumes_partial_unlinks_without_touching_sibling_media(self):
        self.interrupt_after_catalogue()
        stage = self.media / ('.cold-vtl-deletion-' + 'a'*32) / 'NEW001L8'
        (stage / 'data.0').unlink()
        sibling = self.media / 'OTHER001'; sibling.mkdir()
        (sibling / 'data.0').write_text('unrelated backup')
        self.assertEqual(self.delete()['phase'], 'completed')
        self.assertFalse(stage.exists())
        self.assertEqual((sibling / 'data.0').read_text(), 'unrelated backup')
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT count(*) FROM remote_generations').fetchone()[0], 1)

    def test_deletion_releases_virtual_capacity_without_reusing_barcode(self):
        from cartridge_batches import storage_budget, allocate_barcodes
        before = storage_budget(self.instance)['reserved_bytes']
        self.delete()
        self.assertGreater(before, 2 * 1024**3)
        self.assertEqual(storage_budget(self.instance)['reserved_bytes'], 0)
        self.assertNotIn('NEW001L8', allocate_barcodes(self.instance, 'LTO8', 3))

    def test_batch_deletes_every_selected_tape_and_preserves_unselected_slots(self):
        import shutil
        second = self.media / 'SECOND01'
        shutil.copytree(self.tape, second)
        (second / 'mam').write_bytes((second / 'mam').read_bytes().replace(b'NEW001L8', b'SECOND01'))
        with self.instance.db() as db:
            db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES(?,'SECOND01',2147483648,'writable')", (self.instance.vtl_id,))
        self.contents.write_text(self.contents.read_text().replace('Slot 3:', 'Slot 3: SECOND01'))
        self.deletion['barcodes'].append('SECOND01')
        self.assertEqual(self.delete()['phase'], 'completed')
        self.assertFalse(second.exists())
        self.assertFalse(self.tape.exists())
        self.assertIn('Slot 1: EXIST001', self.contents.read_text())
        self.assertNotIn('SECOND01', self.contents.read_text())
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT count(*) FROM cartridge_deletions').fetchone()[0], 2)

    def test_scoped_deletion_preserves_same_barcode_and_active_job_in_another_vtl(self):
        self.instance.scoped = self.instance.vtl_id
        foreign = self.root / 'foreign-media'; foreign.mkdir()
        tape = foreign / 'NEW001L8'; tape.mkdir()
        (tape / 'data.0').write_text('another VTL backup')
        with self.instance.db() as db:
            db.execute("ALTER TABLE jobs ADD COLUMN vtl_id TEXT")
            db.execute("INSERT INTO jobs(status,vtl_id) VALUES('running','other-vtl')")
            db.execute("INSERT INTO vtls VALUES('other-vtl',?,'ready')", (str(foreign),))
            db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES('other-vtl','NEW001L8',1048576,'writable')")
        self.assertEqual(self.delete()['phase'], 'completed')
        self.assertEqual((tape / 'data.0').read_text(), 'another VTL backup')
        with self.instance.db() as db:
            self.assertFalse(db.execute("SELECT 1 FROM cartridge_deletions WHERE vtl_id='other-vtl'").fetchone())
            self.assertEqual(db.execute("SELECT status FROM jobs WHERE vtl_id='other-vtl'").fetchone()[0], 'running')

    def test_online_receipt_keeps_unselected_tape_in_its_original_slot(self):
        ledger = self.media / '.vaultcenter-media-10'
        ledger.write_text('VAULTCENTER_MEDIA1 10\n11111111-1111-4111-8111-111111111111 OTHER002\n')
        before = self.instance.layout()[2]
        self.assertEqual(next(e[1] for e in before if e[2] == 'OTHER002'), 3)
        self.delete()
        after = self.instance.layout()[2]
        self.assertEqual(next(e[1] for e in after if e[2] == 'OTHER002'), 3)
        self.assertIn('Slot 2:\n', self.contents.read_text())
