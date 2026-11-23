import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from concurrent.futures import Future
from unittest.mock import patch
from test_maintenance import pbs
from test_telemetry import collector, UUID, t
import pbs_inventory as inventory

class InventoryTests(unittest.TestCase):
    def test_binding_loader_rejects_cross_vtl_and_writable_manifests(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);config=root/'telemetry.json';manifest=root/'runtime.json'
            config.write_text(json.dumps({'vtls':[{'vtl_id':UUID}]}));config.chmod(0o600)
            manifest.write_text(json.dumps({'vtl_id':UUID,'pbs':{'pbs':'root@192.0.2.1'}}));manifest.chmod(0o600)
            self.assertEqual(t.load_config(config)['vtls'][0]['pbs']['pbs'],'root@192.0.2.1')
            manifest.chmod(0o666)
            with self.assertRaises(ValueError):t.load_config(config)
            manifest.chmod(0o600);manifest.write_text(json.dumps({'vtl_id':'another-vtl','pbs':{}}))
            with self.assertRaises(ValueError):t.load_config(config)
            manifest.unlink();manifest.symlink_to(config)
            with self.assertRaises(ValueError):t.load_config(config)

    def reply(self, **changes):
        return dict(ok=True, vtl_id=UUID, observed_at=1000, slots=[
            dict(number=1,kind='storage',barcode=None,position_known=True,observed_at=1000),
            dict(number=63,kind='import_export',barcode='CVT005L8',position_known=True,observed_at=1000),
        ], **changes)

    def test_guard_reads_only_bound_changer_without_lease_or_cached_positions(self):
        guard=pbs.Guard(dict(vtl_id=UUID,changer='test',changer_path='/dev/tape/by-id/test'))
        entries=[{'entry-id':0,'entry-kind':'drive'}, {'entry-id':63,'entry-kind':'import-export','label-text':'CVT005L8'}, {'entry-id':1,'entry-kind':'slot'}]
        with patch.object(pbs,'run',side_effect=[json.dumps({'path':'/dev/tape/by-id/test'}),json.dumps(entries)]) as run:
            value=guard.inventory()
        self.assertEqual(value['vtl_id'],UUID)
        self.assertEqual([(s['number'],s['kind'],s['barcode']) for s in value['slots']],[(63,'import_export','CVT005L8'),(1,'storage',None)])
        self.assertEqual(run.call_args.args[0],['proxmox-tape','changer','status','test','--cache','false','--output-format','json'])
        self.assertIsNone(guard.lock)
        with patch.object(pbs,'run',return_value='{"path":"wrong"}') as run:
            with self.assertRaises(RuntimeError):guard.inventory()
        self.assertEqual(run.call_count,1)

    def test_reply_scope_bounds_duplicates_and_clock(self):
        self.assertEqual(len(inventory.validated_slots(self.reply(),UUID,2000)),2)
        for mutate in [lambda x:x.update(vtl_id='other'),lambda x:x.update(ok=False),
                       lambda x:x['slots'].append(x['slots'][0]),lambda x:x.update(observed_at=9000),
                       lambda x:x['slots'][0].update(barcode='../bad'),lambda x:x['slots'][0].update(number=True),
                       lambda x:x.update(slots=x['slots']*4097)]:
            value=self.reply();mutate(value)
            with self.subTest(value=str(value)[:120]),self.assertRaises(ValueError):inventory.validated_slots(value,UUID,2000)
        with self.assertRaises(ValueError):inventory.validated_slots(self.reply(),UUID,17000)

    def test_fetch_uses_strict_scoped_ssh_and_timeout(self):
        config=dict(pbs='root@192.0.2.1',key_file='/etc/key',known_hosts='/etc/hosts')
        with patch.object(inventory.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(self.reply()))) as run:
            self.assertEqual(inventory.fetch_inventory(config,UUID),self.reply())
        args=run.call_args.args[0]
        self.assertEqual(args[-5:],['--',config['pbs'],'--vtl',UUID,'inventory'])
        self.assertIn('StrictHostKeyChecking=yes',args)
        self.assertIn('IdentitiesOnly=yes',args)
        self.assertEqual(run.call_args.kwargs['timeout'],8)

    def test_inventory_cli_is_separate_from_lease_socket_and_rejects_arguments(self):
        with patch.object(pbs,'VTL_ID',UUID),patch.object(pbs.CONFIG.__class__,'read_text',return_value=json.dumps({'vtl_id':UUID})),patch.object(pbs.Guard,'inventory',return_value=self.reply()),patch.object(pbs.socket,'socket') as socket,patch('builtins.print'):
            self.assertEqual(pbs.client(['inventory']),0)
            socket.assert_not_called()
            with self.assertRaises(RuntimeError):pbs.client(['inventory','arbitrary'])
        with patch.object(pbs,'VTL_ID',None),self.assertRaises(RuntimeError):pbs.client(['inventory'])

    def test_async_poll_does_not_wait_or_overlap_and_retries_after_failure(self):
        future=Future()
        with patch.object(inventory,'ThreadPoolExecutor') as executor:
            executor.return_value.submit.return_value=future
            poller=inventory.InventoryPoller({},UUID)
            self.assertEqual(poller.poll(0,1000),[])
            self.assertEqual(poller.poll(10,1000),[])
            self.assertEqual(executor.return_value.submit.call_count,1)
            future.set_result(self.reply())
            self.assertEqual(len(poller.poll(10,1000)),2)
            next_future=Future();executor.return_value.submit.return_value=next_future
            self.assertEqual(len(poller.poll(15,1000)),2)
            next_future.set_exception(RuntimeError('offline'))
            self.assertEqual(poller.poll(16,1000),[])
            poller.close()

    def test_slots_make_snapshot_available_but_expire_without_drive_heartbeat(self):
        config=dict(vtl_id=UUID,media_root='/nonexistent',library_queue_id=10,drives=[],pbs={})
        with patch.object(inventory,'ThreadPoolExecutor'):
            c=collector(config)
            with patch.object(c.inventory,'poll',return_value=self.reply()['slots']):c.sample(2000,2)
            value=c.snapshot(2000)
            self.assertEqual(value['status'],'available')
            self.assertEqual(value['slots'][1]['barcode'],'CVT005L8')
            expired=c.snapshot(17000)
            self.assertFalse(expired['slots'][1]['position_known'])
            self.assertIsNone(expired['slots'][1]['barcode'])
            c.inventory.close()

if __name__=='__main__':unittest.main()
