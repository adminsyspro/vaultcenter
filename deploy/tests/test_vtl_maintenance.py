"""Scoped PBS leases must isolate device associations and preserve ownership."""
import fcntl
import json
from pathlib import Path
import tempfile
import unittest
import subprocess
from types import SimpleNamespace
from unittest.mock import patch
from test_maintenance import pbs, vtl

A='00000000-0000-4000-8000-000000000021'
B='00000000-0000-4000-8000-000000000031'

class VtlGuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        self.config={'vtl_id':A,'changer':'a','changer_path':'/dev/tape/by-id/changer-a','changer_serial':'A0',
            'drives':[{'name':'drive-a','path':'/dev/tape/by-id/drive-a','serial':'A1','changer-drivenum':0}]}
        self.guard=pbs.Guard(self.config,self.root/'lease.json')
        self.drives=[{'name':'drive-a','path':'/dev/tape/by-id/drive-a','serial':'A1','changer':'a','changer-drivenum':0},
                     {'name':'drive-b','path':'/dev/tape/by-id/drive-b','serial':'B1','changer':'b','changer-drivenum':0}]
        self.calls=[]
        def run(args,timeout=15):
            self.calls.append(args)
            if args[:3]==['proxmox-tape','drive','list']:return json.dumps(self.drives)
            if args[:3]==['proxmox-tape','changer','list']:return json.dumps([{'name':'a','path':self.config['changer_path'],'serial':'A0'}])
            if args[:3]==['proxmox-tape','changer','config']:return json.dumps({'name':'a','path':self.config['changer_path']})
            if args[:3]==['proxmox-tape','changer','status']:return '[{"entry-kind":"drive","entry-id":0,"state":"locked"}]'
            if args[:3]==['proxmox-backup-manager','task','list']:return '[{"worker_type":"tape-backup","worker_id":"drive-b"}]'
            raise AssertionError(args)
        self.run=patch.object(pbs,'run',side_effect=run);self.run.start();self.addCleanup(self.run.stop)

    def test_guard_uses_scoped_state_selected_after_module_import(self):
        with patch.object(pbs,'STATE',self.root/'scoped/lease.json'):
            guard=pbs.Guard(self.config)
        self.assertEqual(guard.state,self.root/'scoped/lease.json')

    def test_pbs_activity_on_b_does_not_block_a(self):
        self.guard.idle()
        self.drives[1]['path']='/dev/changed-b'
        self.guard.idle()

    def test_pbs_drive_mapping_change_blocks_only_a(self):
        self.drives[0]['changer']='b'
        with self.assertRaises(RuntimeError):self.guard.idle()

    def test_unexpected_drive_in_same_changer_is_not_ignored(self):
        self.drives.append({'name':'extra-a','path':'/dev/extra','serial':'AX','changer':'a','changer-drivenum':1})
        with self.assertRaises(RuntimeError):self.guard.idle()

    def test_all_a_drives_locked_or_attempt_rolled_back(self):
        self.config['drives'].append({'name':'second','path':'/dev/tape/by-id/drive-z','serial':'A2','changer-drivenum':1})
        opened=[]
        def open_lock(path):
            if path.endswith('drive-z'):raise RuntimeError('busy')
            stream=(self.root/'physical-lock').open('a+');fcntl.flock(stream,fcntl.LOCK_EX|fcntl.LOCK_NB);opened.append(stream);return stream
        with patch.object(self.guard,'association'),patch.object(self.guard,'open_drive_lock',side_effect=open_lock):
            with self.assertRaises(RuntimeError):self.guard.acquire()
        self.assertTrue(opened[0].closed)
        self.assertIsNone(self.guard.lock)

    def test_foreign_vtl_lease_is_rejected(self):
        self.guard.lease='00000000-0000-4000-8000-000000000099'
        with self.assertRaises(RuntimeError):self.guard.handle_scoped({'vtl_id':B,'action':'status'})
        result=self.guard.handle_scoped({'vtl_id':A,'action':'status'})
        self.assertEqual(result['vtl_id'],A)
        self.assertEqual(self.guard.lease,'00000000-0000-4000-8000-000000000099')

    def test_scoped_error_preserves_vtl_identity_for_reconnect_retry(self):
        response=pbs.respond(self.guard,{'vtl_id':A,'action':'unsupported'})
        self.assertFalse(response['ok'])
        self.assertEqual(response['vtl_id'],A)
        self.assertIn('error',response)

    def test_bootstrap_reserves_future_drive_before_pbs_registration(self):
        import uuid
        self.drives=self.drives[1:]
        lease=str(uuid.uuid4())
        def lock(path):return (self.root/'future-drive-lock').open('a+')
        with patch.object(self.guard,'open_drive_lock',side_effect=lock):
            result=self.guard.handle_scoped({'vtl_id':A,'action':'bootstrap','lease':lease})
        self.addCleanup(self.guard.lock.close)
        self.assertTrue(result['locked'])
        status=self.guard.handle_scoped({'vtl_id':A,'action':'status'})
        self.assertFalse(status['registered'])
        self.assertTrue(json.loads((self.root/'lease.json').read_text())['bootstrap'])

    def test_bootstrap_cannot_be_reopened_after_verified_publication(self):
        import uuid
        lease=str(uuid.uuid4())
        stream=(self.root/'lock').open('a+');self.addCleanup(stream.close)
        with patch.object(self.guard,'acquire',side_effect=lambda:setattr(self.guard,'lock',stream)):
            result=self.guard.handle_scoped({'vtl_id':A,'action':'bootstrap','lease':lease})
        self.assertTrue(result['locked'])
        self.guard.handle_scoped({'vtl_id':A,'action':'verify','lease':lease})
        self.guard.handle_scoped({'vtl_id':A,'action':'release','lease':lease})
        restarted=pbs.Guard(self.config,self.root/'lease.json')
        with self.assertRaises(RuntimeError):restarted.handle_scoped({'vtl_id':A,'action':'bootstrap','lease':str(uuid.uuid4())})

class VtlMaintenanceTests(unittest.TestCase):
    def test_slow_drive_stop_keeps_the_pbs_lease_until_graceful_completion(self):
        from test_maintenance import FakeGuard, FakeController
        import vtl_runtime
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);guard=FakeGuard(root)
            self.addCleanup(lambda: guard.lock and guard.lock.close())
            controller=FakeController(root,guard)
            drive='vaultcenter-drive-'+A+'-21.service'
            controller.units=['vaultcenter-library-'+A+'.service',drive,'vaultcenter-target-'+A+'.service']
            controller.states=dict.fromkeys(controller.units,'active')
            controller.vtl_id=A
            def execute(args,**kwargs):
                self.assertIsNotNone(guard.lock)
                self.assertTrue(controller.block.exists())
                if args==['systemctl','stop',drive] and kwargs['timeout']<=190:
                    raise subprocess.TimeoutExpired(args,kwargs['timeout'])
                controller.states[args[2]]='inactive'
                return SimpleNamespace(returncode=0,stdout='',stderr='')
            # Exercise both launch paths: maintenance and activation rollback.
            for runner in (lambda args:vtl.Controller.run(controller,args),vtl_runtime.run):
                controller.states=dict.fromkeys(controller.units,'active')
                controller.run=runner
                with patch.object(subprocess,'run',side_effect=execute):
                    try:result=controller.enter('test')
                    except subprocess.TimeoutExpired:self.fail('Maintenance abandons the drive before its graceful stop can finish')
                    self.assertEqual(result['phase'],'maintenance')
                    self.assertTrue(result['pbs_locked'])
                    self.assertTrue(all(s=='inactive' for s in controller.states.values()))
                guard.handle('release',result['lease'])
                controller.path.unlink();controller.block.unlink()

    def test_only_scoped_drive_stops_get_a_long_command_deadline(self):
        import vtl_runtime
        with tempfile.TemporaryDirectory() as tmp:
            controller=vtl.Controller({},Path(tmp))
            for runner in (controller.run,vtl_runtime.run):
                for args in (['systemctl','start','vaultcenter-drive-'+A+'-21.service'],
                             ['systemctl','stop','vaultcenter-library-'+A+'.service'],
                             ['systemctl','stop','tgt.service']):
                    with patch.object(subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout='',stderr='')) as command:
                        runner(args)
                        self.assertEqual(command.call_args.kwargs['timeout'],40)

    def test_scoped_controller_does_not_stop_global_units(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            units=['vaultcenter-library-'+A+'.service','vaultcenter-drive-'+A+'-21.service','vaultcenter-target-'+A+'.service']
            c=vtl.Controller({'vtl_id':A,'units':units},root)
            self.assertEqual(c.units,units)
            states={u:'active' for u in units};commands=[]
            def command(args):
                commands.append(args)
                if args[0]=='systemctl' and args[1] in ('stop','start'):states[args[2]]='inactive' if args[1]=='stop' else 'active'
                return ''
            def remote(action,lease=None):return {'ok':True,'vtl_id':A,'lease':lease or c.read().get('lease'),'locked':True}
            with patch.object(c,'services',side_effect=lambda:states.copy()),patch.object(c,'no_workers'),patch.object(c,'stopped'),patch.object(c,'remote',side_effect=remote),patch.object(c,'run',side_effect=command),patch.object(vtl,'storage_available'):
                result=c.enter('admin')
            self.assertTrue(result['ready']);self.assertEqual({x[-1] for x in commands},set(units))
            self.assertTrue(all('tgt.service' not in x for x in commands))

class VeeamMaintenanceTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name).resolve()

    def veeam_controller(self):
        (self.root/'vtls'/A).mkdir(parents=True)
        controller=vtl.Controller({'vtl_id':A,'client':'veeam','units':['vaultcenter-library-a.service']},self.root/'vtls'/A)
        controller.services=lambda:{'vaultcenter-library-a.service':'active'}
        controller.no_workers=lambda:None;controller.no_reintegration=lambda:None
        controller.stopped=lambda:None;controller.run=lambda args:''
        return controller

    def test_veeam_enter_requires_confirmation(self):
        c=self.veeam_controller()
        with self.assertRaisesRegex(RuntimeError,'Veeam'):c.enter('admin')
        self.assertEqual(c.read()['phase'],'online')
        self.assertFalse(c.path.exists())
        self.assertFalse(c.block.exists())

    def test_veeam_enter_and_leave_use_local_lease(self):
        c=self.veeam_controller()
        with patch('subprocess.run') as ssh:
            c.check=lambda:{'ready':True}
            c.enter('admin',confirm_idle=True)
            self.assertTrue(c.block.exists())
            lease=json.loads((self.root/'vtls'/A/'client-lease.json').read_text())['lease']
            self.assertEqual(lease,c.read()['lease'])
            self.assertFalse(any(call.args[0][0]=='ssh' for call in ssh.call_args_list))

    def test_veeam_full_cycle_returns_online(self):
        c=self.veeam_controller()
        c.check=lambda:{'ready':True}
        c.enter('admin',confirm_idle=True)
        c.leave('admin')
        self.assertEqual(c.read()['phase'],'online')
        self.assertFalse(c.block.exists())
        self.assertIsNone(json.loads((self.root/'vtls'/A/'client-lease.json').read_text())['lease'])

    def test_veeam_leave_completes_after_lease_already_released(self):
        c=self.veeam_controller()
        c.check=lambda:{'ready':True}
        c.enter('admin',confirm_idle=True)
        from client_lease import LocalLease
        LocalLease(self.root/'vtls'/A,A).handle('release',c.read()['lease'])
        # leave() died after release: block already removed, phase still leaving.
        c.block.unlink();state=c.read();state['phase']='leaving';c.save(state)
        c.leave('admin')
        self.assertEqual(c.read()['phase'],'online')

    def test_veeam_lease_failures_never_mention_pbs(self):
        c=self.veeam_controller()
        c.check=lambda:{'ready':True}
        c.enter('admin',confirm_idle=True)
        from client_lease import LocalLease
        lease=LocalLease(self.root/'vtls'/A,A)
        lease.handle('release',c.read()['lease'])
        lease.handle('acquire','00000000-0000-4000-8000-0000000000ff')
        with self.assertRaises(RuntimeError) as caught:c.leave('admin')
        self.assertNotIn('PBS',str(caught.exception))
        with self.assertRaises(RuntimeError) as caught:c.check_lease()
        self.assertNotIn('PBS',str(caught.exception))

    def test_veeam_status_reports_client(self):
        self.assertEqual(self.veeam_controller().status()['client'],'veeam')
        self.assertEqual(vtl.Controller({},self.root).status()['client'],'pbs')

if __name__=='__main__':unittest.main()
