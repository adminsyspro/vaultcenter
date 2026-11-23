"""Durable requests and fail-closed host transitions with real disk state."""
import importlib.util
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

HOST = Path(__file__).resolve().parents[1] / 'host'
sys.path.insert(0, str(HOST))

class StorageControllerTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue((HOST/'storage_controller.py').exists(), 'Host storage controller is missing')
        spec=importlib.util.spec_from_file_location('storage_controller', HOST/'storage_controller.py')
        self.module=importlib.util.module_from_spec(spec); spec.loader.exec_module(self.module)
        self.tmp=tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve(); self.state=self.root/'state'; self.state.mkdir(); (self.state/'storage-bootstrap').write_text('{}')
        self.media=self.root/'old'; self.media.mkdir(); self.target=self.root/'tapes'; self.target.mkdir()
        self.db=sqlite3.connect(self.state/'catalog.db')
        self.db.executescript("CREATE TABLE cartridges(barcode TEXT); CREATE TABLE jobs(status TEXT); CREATE TABLE archive_requests(phase TEXT); CREATE TABLE bucket_rotations(phase TEXT);")
        self.addCleanup(self.db.close)
        self.config={'state_dir':str(self.state),'media_dir':str(self.media),'compose_file':str(self.root/'compose.yaml'),'project':'vaultcenter','container':'vaultcenter-appliance-1','socket':str(self.root/'socket'),'bootstrap':True}
        (self.root/'compose.yaml').write_text('services: {}')
        self.c=self.module.Controller(self.config,self.root/'host');self.addCleanup(self.c.release)
        self.mount=patch('storage_paths.mount_identity',return_value={'uuid':'disk-A','filesystem':'ext4','mount_root':'/','mountpoint':str(self.root)})
        self.mount.start(); self.addCleanup(self.mount.stop)
        self.runtime=patch.object(self.c,'runtime',side_effect=self.fake_runtime); self.runtime.start(); self.addCleanup(self.runtime.stop)
        self.mounted=str(self.media)

    def fake_runtime(self, action, path=None):
        if action=='recreate': self.mounted=path
        if action=='binding':
            st=Path(self.mounted).stat();return {'media_source':self.mounted,'inode':st.st_ino,'device':st.st_dev}
        if action=='inspect': return {'media_source':self.mounted,'stopped':True,'healthy':True}
        return None

    def request(self, identifier='request-1'):
        checked=self.c.check(str(self.target))
        return {'path':str(self.target),'revision':0,'request_id':identifier,'check_id':checked['check_id'],'actor':'admin'}

    def test_apply_persists_then_completes_mount_without_touching_data(self):
        request=self.request(); result=self.c.apply(request)
        self.assertEqual(result['phase'],'applying')
        self.assertTrue((self.state/'storage-block').exists())
        self.c.execute()
        self.assertEqual(self.c.status()['phase'],'ready')
        self.assertEqual(self.c.status()['path'],str(self.target))
        self.assertEqual(self.c.status()['revision'],1)
        self.assertEqual(list(self.target.iterdir()),[])
        self.assertTrue((self.state/'storage-services-held').exists())
        override=json.loads((self.root/'host/mount.json').read_text())
        mount=override['services']['appliance']['volumes'][0]
        self.assertEqual(mount['source'],str(self.target)); self.assertFalse(mount['bind']['create_host_path'])

    def test_lost_response_replay_returns_same_operation_without_reapply(self):
        request=self.request(); first=self.c.apply(request)
        self.assertEqual(self.c.apply(request)['operation']['id'],first['operation']['id'])
        self.c.execute()
        self.assertEqual(self.c.apply(request)['operation']['id'],first['operation']['id'])
        altered={**request,'path':str(self.media)}
        with self.assertRaises(ValueError): self.c.apply(altered)

    def test_replaced_check_target_and_stale_revision_are_rejected(self):
        request=self.request(); self.target.rename(self.root/'saved'); self.target.mkdir()
        with self.assertRaises(ValueError): self.c.apply(request)
        request=self.request(); request['revision']=9
        with self.assertRaises(ValueError): self.c.apply(request)

    def test_existing_media_catalogue_jobs_and_lock_block_apply(self):
        request=self.request(); (self.media/'data').write_text('preserve')
        with self.assertRaises(ValueError): self.c.apply(request)
        (self.media/'data').unlink()
        for table,column,value in [('cartridges','barcode','ABC001L8'),('jobs','status','running'),('archive_requests','phase','uncertain')]:
            self.db.execute(f'INSERT INTO {table}({column}) VALUES(?)',(value,)); self.db.commit()
            with self.assertRaises(ValueError): self.c.apply(request)
            self.db.execute(f'DELETE FROM {table}'); self.db.commit()
        import fcntl
        with (self.state/'maintenance-controller.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(ValueError): self.c.apply(request)

    def test_interrupted_operation_keeps_gate_until_recovery(self):
        self.c.apply(self.request())
        with patch.object(self.c,'runtime',side_effect=RuntimeError('docker unavailable')):
            self.c.execute()
        self.assertEqual(self.c.status()['phase'],'recovery_required')
        self.assertTrue((self.state/'storage-block').exists())
        restarted=self.module.Controller(self.config,self.root/'host')
        with patch.object(restarted,'runtime',side_effect=self.fake_runtime):
            restarted.execute()
            self.assertEqual(restarted.status()['phase'],'ready')

    def test_disappeared_mount_is_not_reported_ready(self):
        self.c.apply(self.request()); self.c.execute()
        with patch('storage_paths.mount_identity',return_value={'uuid':'system','filesystem':'ext4','mount_root':'/','mountpoint':'/'}):
            self.assertEqual(self.c.status()['phase'],'unavailable')

    def test_check_identity_cannot_be_replaced_between_verify_and_inspect(self):
        request=self.request()
        original=self.module.inspect_path
        def replacement(path, forbidden):
            self.target.rename(self.root/'old-target'); self.target.mkdir()
            return original(path, forbidden)
        with patch.object(self.module,'inspect_path',side_effect=replacement):
            with self.assertRaises(ValueError): self.c.apply(request)
        self.assertEqual(self.c.status()['phase'],'unconfigured')

    def test_restored_host_mount_does_not_validate_stale_container_mount(self):
        self.c.apply(self.request());self.c.execute()
        with patch.object(self.c,'runtime',return_value={'media_source':str(self.target),'inode':999,'device':999,'stopped':True,'healthy':True}):
            self.assertEqual(self.c.status()['phase'],'unavailable')

    def test_post_bootstrap_recovery_checks_lease_independently_of_storage_gate(self):
        self.c.config['bootstrap']=False
        def command(args, timeout=30):
            if args[-1]=='{{.State.Running}}': return 'false'
            if args[-1]=='check-lease': return '{"ready":true,"pbs_locked":true,"blocked":true}'
            if (self.state/'storage-block').exists(): raise ValueError('storage gate blocks ordinary maintenance check')
            return '{"ready":true,"pbs_locked":true,"blocked":true}'
        with patch.object(self.c,'command',side_effect=command):
            self.c.apply(self.request());self.c.execute()
        self.assertEqual(self.c.status()['phase'],'ready')

    def test_ready_journal_with_block_can_finish_cleanup_after_crash(self):
        self.c.apply(self.request())
        original=Path.unlink
        def interrupted(path,*args,**kwargs):
            if path.name=='storage-block':raise KeyboardInterrupt('power loss')
            return original(path,*args,**kwargs)
        with patch.object(Path,'unlink',interrupted):
            with self.assertRaises(KeyboardInterrupt):self.c.execute()
        self.assertTrue((self.state/'storage-block').exists())
        restarted=self.module.Controller(self.config,self.root/'host')
        with patch.object(restarted,'runtime',side_effect=self.fake_runtime):restarted.execute()
        self.assertFalse((self.state/'storage-block').exists())

    def test_terminal_archive_and_rotation_history_does_not_block_empty_storage(self):
        for phase in ('archived','restored','failed'):
            self.db.execute('INSERT INTO archive_requests(phase) VALUES(?)',(phase,))
        self.db.execute("INSERT INTO bucket_rotations(phase) VALUES('switched')");self.db.commit()
        self.c.apply(self.request()); self.c.execute()
        self.assertEqual(self.c.status()['phase'],'ready')

    def test_recreated_container_can_take_time_to_accept_systemd_queries(self):
        self.c.apply(self.request())
        pending=[True]
        def runtime(action,path=None):
            if action=='inspect' and pending:
                pending.pop()
                raise ValueError('systemd bus starting')
            return self.fake_runtime(action,path)
        with patch.object(self.c,'runtime',side_effect=runtime),patch.object(self.module.time,'sleep'):
            self.c.execute()
        self.assertEqual(self.c.status()['phase'],'ready')

if __name__=='__main__': unittest.main()
