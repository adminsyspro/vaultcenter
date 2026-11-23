import fcntl
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import sys
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'cold-vtl'))
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,ROOT/path)
    mod=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod
vtl=module('vtl','cold-vtl/maintenance-controller.py')
pbs=module('pbs','pbs/maintenance-guard.py')

class FakeGuard(pbs.Guard):
    def __init__(self,root):
        super().__init__({},root/'pbs-state.json')
        self.root=root
        self.busy=False
    def acquire(self):
        file=(self.root/'pbs-drive.lock').open('a+')
        try: fcntl.flock(file,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:
            file.close()
            raise RuntimeError('busy PBS drive')
        self.lock=file
    def media_empty(self, barcode=None): pass
    def idle(self):
        if self.busy: raise RuntimeError('active PBS task')

class FakeController(vtl.Controller):
    def __init__(self,root,guard):
        super().__init__({},root)
        self.guard=guard
        self.states={unit:'active' for unit in vtl.UNITS}
        self.commands=[]
        self.fail=None
        self.worker=False
        self.offline=False
    def services(self): return self.states.copy()
    def no_workers(self):
        if self.worker: raise RuntimeError('active worker')
    def stopped(self):
        if any(value!='inactive' for value in self.states.values()): raise RuntimeError('services active')
    def remote(self,action,lease=None):
        if self.offline: raise RuntimeError('PBS unavailable')
        return {'ok':True,**self.guard.handle(action,lease)}
    def run(self,args):
        _,action,unit=args
        self.commands.append((action,unit))
        if self.fail==(action,unit): raise RuntimeError('simulated service failure')
        if action=='start' and self.block.exists(): raise RuntimeError('systemd condition blocks startup')
        self.states[unit]='active' if action=='start' else 'inactive'
        return ''

class MaintenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
        self.guard=FakeGuard(self.root)
        self.controller=FakeController(self.root,self.guard)
    def tearDown(self):
        if self.guard.lock: self.guard.lock.close()
        self.temp.cleanup()
    def test_lease_proof_remains_valid_during_storage_gate_and_rejects_lost_lease(self):
        self.controller.enter('operator')
        (self.root/'storage-managed').touch();(self.root/'storage-block').touch()
        self.assertTrue(self.controller.check_lease()['pbs_locked'])
        with self.assertRaises(ValueError):self.controller.check()
        self.guard.lease = 'someone-else'
        with self.assertRaises(RuntimeError):self.controller.check_lease()

    def test_cycle_locks_pbs_and_restores_initial_services(self):
        original=self.controller.services()
        result=self.controller.enter('operator')
        self.assertTrue(result['ready'])
        self.assertTrue(self.controller.block.exists())
        contender=FakeGuard(self.root)
        with self.assertRaises(RuntimeError): contender.acquire()
        self.assertEqual(self.controller.enter('operator')['lease'],result['lease'])
        self.controller.leave('operator')
        self.assertEqual(self.controller.services(),original)
        self.assertIsNone(self.guard.lease)
        self.assertFalse(self.controller.block.exists())
        self.assertEqual(self.controller.leave('operator')['phase'],'online')
    def test_resume_waits_for_iscsi_reconnection_while_holding_pbs(self):
        self.controller.enter('operator')
        original = self.controller.remote
        attempts = []
        def remote(action, lease=None):
            if action == 'verify':
                attempts.append(action)
                self.assertIsNotNone(self.guard.lock)
                if len(attempts) == 1:
                    raise RuntimeError("PBS check failed: error opening changer: No such device or address")
            return original(action, lease)
        with patch.object(self.controller, 'remote', side_effect=remote), patch.object(vtl.time, 'sleep'):
            self.assertEqual(self.controller.leave('operator')['phase'], 'online')
        self.assertEqual(len(attempts), 2)
        self.assertIsNone(self.guard.lease)

    def test_reconnection_timeout_keeps_services_stopped_and_pbs_reserved(self):
        self.controller.enter('operator')
        original = self.controller.remote
        def remote(action, lease=None):
            if action == 'verify':
                raise RuntimeError("PBS check failed: error opening changer: No such device or address")
            return original(action, lease)
        with patch.object(self.controller, 'remote', side_effect=remote), patch.object(vtl.time, 'monotonic', side_effect=[0,31]):
            with self.assertRaisesRegex(RuntimeError, 'No such device'):
                self.controller.leave('operator')
        self.assertEqual(set(self.controller.states.values()), {'inactive'})
        self.assertIsNotNone(self.guard.lease)
        self.assertTrue(self.controller.block.exists())

    def test_busy_pbs_refuses_without_stopping_services(self):
        self.guard.busy=True
        with self.assertRaisesRegex(RuntimeError,'active PBS'): self.controller.enter('operator')
        self.assertFalse(self.controller.commands)
        self.assertFalse(self.controller.block.exists())
        self.assertIsNone(self.guard.lease)
        self.controller.leave('operator')
        self.assertEqual(self.controller.read()['phase'],'online')
    def test_pbs_lock_busy_refuses_without_touching_services(self):
        holder=(self.root/'pbs-drive.lock').open('a+')
        fcntl.flock(holder,fcntl.LOCK_EX|fcntl.LOCK_NB)
        try:
            with self.assertRaisesRegex(RuntimeError,'busy PBS'): self.controller.enter('operator')
            self.assertFalse(self.controller.commands)
        finally: holder.close()
    def test_partial_stop_keeps_guard_and_is_recoverable(self):
        self.controller.fail=('stop','vtltape@11.service')
        with self.assertRaises(RuntimeError): self.controller.enter('operator')
        self.assertTrue(self.controller.block.exists())
        self.assertIsNotNone(self.guard.lease)
        self.assertEqual(self.controller.read()['phase'],'failed')
        self.controller.fail=None
        self.assertEqual(self.controller.leave('operator')['phase'],'online')
    def test_resume_failure_stops_services_and_retains_guard(self):
        self.controller.enter('operator')
        self.controller.fail=('start','vtltape@11.service')
        with self.assertRaises(RuntimeError): self.controller.leave('operator')
        self.assertEqual(set(self.controller.states.values()),{'inactive'})
        self.assertTrue(self.controller.block.exists())
        self.assertIsNotNone(self.guard.lease)
        self.controller.fail=None
        self.assertEqual(self.controller.leave('operator')['phase'],'online')
    def test_daemon_restart_keeps_maintenance_and_guard(self):
        self.controller.enter('operator')
        recovered=FakeController(self.root,self.guard)
        recovered.states=self.controller.states
        self.assertTrue(recovered.check()['ready'])
        recovered.leave('operator')
        self.assertEqual(recovered.read()['phase'],'online')
    def test_pbs_guard_restart_reacquires_persisted_lease(self):
        self.controller.enter('operator')
        lease=self.guard.lease
        self.guard.lock.close()
        self.guard=FakeGuard(self.root)
        self.guard.restore()
        self.assertEqual(self.guard.lease,lease)
        contender=FakeGuard(self.root)
        with self.assertRaises(RuntimeError): contender.acquire()
        self.guard.handle('release',lease)
        self.guard=FakeGuard(self.root)
        self.guard.restore()
        self.assertIsNone(self.guard.lock)
    def test_wrong_lease_cannot_release_or_replace(self):
        self.controller.enter('operator')
        other='00000000-0000-0000-0000-000000000001'
        for action in ['acquire','release']:
            with self.assertRaises(RuntimeError): self.guard.handle(action,other)
        self.assertIsNotNone(self.guard.lock)
    def test_worker_blocks_entry_and_exit(self):
        self.controller.worker=True
        with self.assertRaisesRegex(RuntimeError,'active worker'): self.controller.enter('operator')
        self.assertFalse(self.controller.commands)
        self.controller.worker=False
        self.controller.enter('operator')
        self.controller.worker=True
        with self.assertRaisesRegex(RuntimeError,'active worker'): self.controller.leave('operator')
        self.assertTrue(self.controller.check()['ready'])
    def test_pbs_unavailable_or_lock_lost_blocks_resume(self):
        self.controller.enter('operator')
        self.controller.offline=True
        with self.assertRaises(RuntimeError): self.controller.leave('operator')
        self.assertTrue(self.controller.block.exists())
        self.controller.offline=False
        self.guard.handle('release',self.guard.lease)
        with self.assertRaisesRegex(RuntimeError,'lost'): self.controller.leave('operator')
        self.assertFalse(self.controller.status()['ready'])
        self.assertEqual(set(self.controller.states.values()),{'inactive'})
    def test_interrupted_entry_can_be_recovered(self):
        self.controller.enter('operator')
        state=self.controller.read();state['phase']='entering';self.controller.save(state)
        self.assertFalse(self.controller.status()['ready'])
        self.assertEqual(self.controller.leave('operator')['phase'],'online')
    def test_mixed_initial_service_state_is_not_changed(self):
        self.controller.states['tgt.service']='inactive'
        with self.assertRaisesRegex(RuntimeError,'mixed state'): self.controller.enter('operator')
        self.assertFalse(self.controller.commands)
        self.assertIsNone(self.guard.lease)

if __name__=='__main__': unittest.main()
