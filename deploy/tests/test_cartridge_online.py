import json
import sqlite3
import unittest
from unittest.mock import patch
import test_cartridges


class OnlineCartridgeTests(test_cartridges.CartridgeTests):
    # Only inherit the fixture, not the offline assertions.
    def setUp(self):
        super().setUp()
        self.inserted = {}
        self.before = self.contents.read_text()

    def status(self, instance):
        return {'version':1,'library_id':10,'free_slots':2-len(self.inserted)}

    def insert(self, instance, operation_id, barcode):
        previous = self.inserted.get(operation_id)
        if previous and previous != barcode:
            raise RuntimeError('conflict')
        self.inserted[operation_id] = barcode
        return {'result':'already_inserted' if previous else 'inserted','barcode':barcode,'operation_id':operation_id}

    def test_online_creation_preserves_services_and_config(self):
        with patch.object(self.mod.online,'status',side_effect=self.status), patch.object(self.mod.online,'insert',side_effect=self.insert):
            options=self.instance.options(self.request)
            self.assertTrue(options['ready'])
            self.assertTrue(options['online_creation'])
            result=self.create()
            self.assertEqual(result['barcode'],'NEW001L8')
            self.assertEqual(self.controller.read()['phase'],'online')
            self.assertEqual(self.contents.read_text(),self.before)
            self.assertEqual(len(self.inserted),1)
            self.assertEqual(self.create(),result)

    def test_online_retry_after_write_and_lost_confirmation(self):
        def lost(instance, op, barcode):
            self.insert(instance,op,barcode)
            (self.media/barcode/'data.0').write_bytes(b'already written by PBS')
            raise RuntimeError('response lost')
        with patch.object(self.mod.online,'status',side_effect=self.status):
            with patch.object(self.mod.online,'insert',side_effect=lost):
                with self.assertRaisesRegex(RuntimeError,'response lost'): self.create()
            self.assertEqual(self.count(),0)
            with patch.object(self.mod.online,'insert',side_effect=self.insert):
                self.create()
            self.assertEqual((self.media/'NEW001L8'/'data.0').read_bytes(),b'already written by PBS')
            self.assertEqual(len(self.calls),1)
            self.assertEqual(self.count(),1)

    def test_online_pending_cannot_switch_to_offline_mode(self):
        with patch.object(self.mod.online,'status',side_effect=self.status), patch.object(self.mod.online,'insert',side_effect=RuntimeError('lost')):
            with self.assertRaises(RuntimeError): self.create()
        with self.assertRaisesRegex(RuntimeError,'interrupted'):
            self.controller.enter('admin')
        with patch.object(self.controller,'read',return_value={'phase':'maintenance'}), patch.object(self.controller,'check'):
            with self.assertRaisesRegex(RuntimeError,'mode'): self.create()

    def test_online_worker_or_maintenance_transition_blocks(self):
        with patch.object(self.mod.online,'status',side_effect=self.status):
            self.controller.worker=True
            with self.assertRaises(RuntimeError): self.create()
            self.controller.worker=False
            self.controller.save({'phase':'entering'})
            with self.assertRaises(RuntimeError): self.create()
            self.assertEqual(self.calls,[])

# unittest also sees inherited tests; retain only the new scenarios in this class.
for name in dir(test_cartridges.CartridgeTests):
    if name.startswith('test_') and name not in OnlineCartridgeTests.__dict__:
        setattr(OnlineCartridgeTests,name,None)

class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.fixture=test_cartridges.CartridgeTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.instance=self.fixture.instance
        self.online=self.fixture.mod.online
        self.op='00000000-0000-4000-8000-000000000001'

    def test_correlated_response_and_no_shell(self):
        from types import SimpleNamespace
        response={'version':1,'library_id':10,'operation_id':self.op,'free_slots':2}
        with patch.object(self.online.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout=json.dumps(response))) as run:
            self.assertEqual(self.online.command(self.instance,'status',self.op)['free_slots'],2)
            args,kwargs=run.call_args
            self.assertEqual(args[0],['vtlcmd','10','vaultcenter-media','status',self.op])
            self.assertEqual(kwargs['env']['MHVTL_CONFIG_PATH'],str(self.instance.config_dir))
            self.assertNotIn('shell',kwargs)
        for change in ({'library_id':20},{'operation_id':'foreign'},{'free_slots':True},{'version':2}):
            with self.subTest(change=change), patch.object(self.online.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout=json.dumps({**response,**change}))):
                with self.assertRaises(RuntimeError):self.online.command(self.instance,'status',self.op)

    def test_offline_overlay_and_foreign_receipt(self):
        receipt=self.instance.media/'.vaultcenter-media-10'
        receipt.write_text(f'VAULTCENTER_MEDIA1 10\n{self.op} NEW001L8\n')
        content=self.fixture.contents.read_text()
        _,overlaid,entries=self.instance.layout()
        self.assertIn('Slot 2: NEW001L8',overlaid)
        self.assertEqual(self.fixture.contents.read_text(),content,'Reading receipts must not write config')
        self.assertEqual(sum(e[0]=='Slot' and not e[2] for e in entries),1)
        receipt.write_text(f'VAULTCENTER_MEDIA1 20\n{self.op} NEW001L8\n')
        with self.assertRaises(RuntimeError):self.instance.layout()

import test_cartridge_batches


class OnlineBatchTests(unittest.TestCase):
    setUp=test_cartridge_batches.BatchTests.setUp
    mktape=test_cartridge_batches.BatchTests.mktape
    step=test_cartridge_batches.BatchTests.step
    request_batch=test_cartridge_batches.BatchTests.request_batch
    count=test_cartridge_batches.BatchTests.count

    def test_live_batch_replay_does_not_consume_slots_twice(self):
        placed={};before=self.contents.read_text()
        def insert(instance, operation_id, barcode):
            placed[operation_id]=barcode
            return {'result':'inserted'}
        with patch.object(self.mod.online,'status',side_effect=lambda i:{'free_slots':2-len(placed)}), patch.object(self.mod.online,'insert',side_effect=insert):
            first=self.step()
            self.assertEqual(first['completed'],1)
            self.assertEqual(first['mode'],'online')
            last=self.step()
            self.assertEqual(last['phase'],'completed')
            self.assertEqual(self.step(),last)
            self.assertEqual(len(placed),2)
            self.assertEqual(self.count(),2)
            self.assertEqual(self.contents.read_text(),before)
            self.assertEqual(self.controller.commands,[])

    def test_live_slot_count_prevents_a_batch_before_mktape(self):
        with patch.object(self.mod.online,'status',return_value={'free_slots':1}):
            with self.assertRaisesRegex(RuntimeError,'slots'):self.step()
        self.assertEqual(self.calls,[])
