"""Scoped storage coordinator. A data lock is permanent; no media adoption."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import sqlite3
import subprocess
import time
import uuid
from storage_paths import directory, inspect_path, verify_identity
from vtl_state import VtlState, atomic

PUBLIC = ('mode','vtl_id','phase','revision','path','capacity','operation','message',
          'data_locked','change_allowed','change_lock_reason')


def overlaps(left, right):
    a,b = PurePosixPath(left), PurePosixPath(right)
    return a == b or a in b.parents or b in a.parents


def volume_path(candidate):
    identity = candidate['identity']
    return str(PurePosixPath(identity['mount_root']) / PurePosixPath(candidate['path']).relative_to(identity['mountpoint']))


class VtlStorage:
    def __init__(self, config):
        self.config = config
        self.store = VtlState(config['host_state'],config['state_dir'])

    def forbidden(self):
        return [str(self.store.root),str(self.store.state_dir),self.config['mount_root'],
                *self.config.get('forbidden',[])]

    def observe(self, vtl_id, value, ignore_gate=False):
        """Return a change refusal; never infer emptiness from an inaccessible path."""
        with closing(sqlite3.connect((self.store.state_dir/'catalog.db').as_uri()+'?mode=ro',uri=True,timeout=5)) as db:
            if db.execute('SELECT 1 FROM cartridges WHERE vtl_id=? LIMIT 1',(vtl_id,)).fetchone():
                self.store.mark_data(vtl_id)
            active = any(db.execute(query,(vtl_id,)).fetchone() for query in (
                "SELECT 1 FROM jobs WHERE vtl_id=? AND status IN ('queued','running') LIMIT 1",
                "SELECT 1 FROM archive_requests WHERE vtl_id=? AND phase NOT IN ('archived','restored','failed') LIMIT 1",
                "SELECT 1 FROM bucket_rotations WHERE vtl_id=? AND phase NOT IN ('switched','failed') LIMIT 1"))
        root = self.store.operation_root(vtl_id)
        active = active or any((root/name).exists() for name in ('reintegration-pending.json','cartridge-creation-pending.json','cartridge-deletion-pending.json'))
        source = value['path'] or self.config.get('initial_media',{}).get(vtl_id)
        unavailable = False
        if source:
            try:
                if value.get('identity'): verify_identity(source,value['identity'])
                with directory(source) as fd:
                    if os.listdir(fd): self.store.mark_data(vtl_id)
            except (ValueError,OSError): unavailable = True
        if (root/'data-locked').exists(): return 'data_present'
        if value['phase'] == 'recovery_required': return 'recovery_required'
        if active or not ignore_gate and (value['phase'] == 'applying' or (root/'storage-block').exists()): return 'operation_in_progress'
        if unavailable: return 'storage_unavailable'
        return None

    def status(self, vtl_id):
        value = self.store.load(vtl_id)
        reason = self.observe(vtl_id,value)
        value['data_locked'] = self.store.load(vtl_id)['data_locked']
        value['capacity'] = None
        if value['phase'] == 'ready':
            try:
                value['capacity'] = verify_identity(value['path'],value['identity'])
                self.verify_binding(vtl_id,value['path'],value['identity'])
                if (self.store.operation_root(vtl_id)/'storage-block').exists():
                    raise ValueError('Storage finalisation requires recovery for this VTL.')
            except (ValueError,OSError,subprocess.SubprocessError) as error:
                value.update(phase='unavailable',message=str(error))
                reason = reason or 'storage_unavailable'
        value.update(change_allowed=reason is None, change_lock_reason=reason)
        return {key:value.get(key) for key in PUBLIC}

    def verify_current(self, vtl_id): return self.status(vtl_id)

    def assert_change(self, vtl_id):
        status = self.status(vtl_id)
        if not status['change_allowed']:
            if status['data_locked']:
                raise ValueError('Path locked: this VTL contains or previously contained data. Moving it requires a dedicated migration.')
            raise ValueError('The storage of this VTL cannot be changed: '+str(status['change_lock_reason']))

    def assert_unclaimed(self, vtl_id, candidate):
        for file in (self.store.root/'vtls').glob('*/storage.json'):
            value = json.loads(file.read_text())
            if value['vtl_id'] == vtl_id: continue
            for other in (value, value.get('candidate'), value.get('previous')):
                if not other or not other.get('path') or not other.get('identity'): continue
                identity = other['identity']
                same_volume = identity['uuid'] == candidate['identity']['uuid']
                if (overlaps(other['path'],candidate['path']) or
                    same_volume and (identity['inode'] == candidate['identity']['inode'] or
                                     overlaps(volume_path(other),volume_path(candidate)))):
                    raise ValueError('This folder or its volume is already reserved for another VTL.')

    def check(self, vtl_id, path):
        with self.store.operation_lock(vtl_id):
            self.assert_change(vtl_id)
            candidate = inspect_path(path,self.forbidden())
            with self.store.registry_lock(): self.assert_unclaimed(vtl_id,candidate)
            checked = {**candidate,'vtl_id':vtl_id,'check_id':uuid.uuid4().hex,
                       'revision':self.store.load(vtl_id)['revision'],'expires':time.time()+300}
            atomic(self.store.path(vtl_id)/'check.json',checked)
            return {key:checked[key] for key in ('vtl_id','path','filesystem','total_bytes','available_bytes','check_id')}

    def lock_data(self, vtl_id):
        with self.store.operation_lock(vtl_id):
            value = self.store.load(vtl_id)
            if value['phase'] != 'ready' and value['phase'] != 'unconfigured':
                raise ValueError('A storage transition must finish before creating media.')
            self.store.mark_data(vtl_id)

    def apply(self, vtl_id, request):
        if not isinstance(request,dict) or set(request) != {'path','revision','request_id','check_id','actor'}:
            raise ValueError('Invalid storage request.')
        if type(request['revision']) is not int or request['revision'] < 0: raise ValueError('Invalid revision.')
        for field in ('request_id','check_id','actor'):
            if not isinstance(request[field],str) or not 1 <= len(request[field]) <= 128: raise ValueError('Invalid identifier.')
        fingerprint = hashlib.sha256(json.dumps({'vtl_id':vtl_id,**request},sort_keys=True).encode()).hexdigest()
        with self.store.operation_lock(vtl_id):
            current = self.store.load(vtl_id)
            if current.get('request_id') == request['request_id']:
                if current['fingerprint'] != fingerprint: raise ValueError('Identifier already used for another request.')
                return self.status(vtl_id)
            self.assert_change(vtl_id)
            if current['revision'] != request['revision']: raise ValueError('Configuration changed elsewhere. Reload.')
            file = self.store.path(vtl_id)/'check.json'
            if not file.exists(): raise ValueError('Check the path for this VTL before saving.')
            checked = json.loads(file.read_text())
            if any(checked[key] != request[key] for key in ('path','revision','check_id')) or checked['vtl_id'] != vtl_id or checked['expires'] < time.time():
                raise ValueError('Verification expired or belongs to another VTL.')
            verify_identity(request['path'],checked['identity'])
            candidate = inspect_path(request['path'],self.forbidden())
            if candidate['identity'] != checked['identity']: raise ValueError('The folder changed after verification.')
            self.require_idle(vtl_id)
            self.assert_change(vtl_id)
            with self.store.registry_lock():
                self.assert_unclaimed(vtl_id,candidate)
                root = self.store.operation_root(vtl_id)
                current.pop('previous',None); current.pop('candidate',None)
                value = {**current,'phase':'applying','operation':{'id':request['request_id'],'phase':'prepared'},
                         'previous':current,'candidate':candidate,'request_id':request['request_id'],'fingerprint':fingerprint}
                self.store.save(vtl_id,value)
                atomic(root/'storage-block',{'reason':'storage configuration','vtl_id':vtl_id})
                atomic(root/'storage-services-held',{'reason':'explicit resume required'})
            return self.status(vtl_id)

    def command(self, arguments, timeout=30):
        result = subprocess.run(arguments,capture_output=True,text=True,timeout=timeout)
        if result.returncode: raise ValueError('System command refused; check this VTL’s logs.')
        return result.stdout

    def require_idle(self, vtl_id):
        value = json.loads(self.command(['docker','exec',self.config['container'],
            '/usr/local/libexec/cold-vtl-vtl','storage-idle','--vtl',vtl_id]))
        if value.get('vtl_id') != vtl_id or value.get('stopped') is not True or value.get('lease_verified') is not True:
            raise ValueError('Stop this VTL under verified PBS maintenance before making the change.')

    def verify_binding(self, vtl_id, path, identity):
        from storage_mounts import Mounts
        return Mounts(self.config).verify(vtl_id,path,identity)

    def execute(self, vtl_id):
        from storage_mounts import Mounts
        with self.store.operation_lock(vtl_id):
            value = self.store.load(vtl_id)
            root = self.store.operation_root(vtl_id)
            if value['phase'] == 'ready' and (root/'storage-block').exists():
                self.verify_binding(vtl_id,value['path'],value['identity'])
                self.require_idle(vtl_id)
                (root/'storage-block').unlink()
                return
            if value['phase'] not in ('applying','recovery_required'): return
            try:
                atomic(root/'storage-block',{'reason':'storage configuration','vtl_id':vtl_id})
                atomic(root/'storage-services-held',{'reason':'explicit resume required'})
                self.require_idle(vtl_id)
                previous=value['previous']; candidate=value['candidate']
                reason=self.observe(vtl_id,previous,ignore_gate=True)
                # The transition's own gate is expected. Other activity is checked
                # by the runtime and the SQL queries before accepting the request.
                if reason is not None:
                    raise ValueError('Data is present or previous storage cannot be verified.')
                verify_identity(candidate['path'],candidate['identity'])
                with directory(candidate['path']) as fd:
                    if os.listdir(fd): raise ValueError('The destination now contains data.')
                with self.store.registry_lock(): self.assert_unclaimed(vtl_id,candidate)
                value.update(phase='applying',message=None)
                value['operation']['phase']='publishing';self.store.save(vtl_id,value)
                Mounts(self.config).publish(vtl_id,candidate['path'],candidate['identity'])
                self.verify_binding(vtl_id,candidate['path'],candidate['identity'])
                value.update(phase='ready',path=candidate['path'],identity=candidate['identity'],revision=previous['revision']+1)
                value['operation']['phase']='verified';self.store.save(vtl_id,value)
                (root/'storage-block').unlink(missing_ok=True)
            except Exception as error:
                value.update(phase='recovery_required',message=str(error) if isinstance(error,ValueError) else 'Application interrupted; check this VTL’s log.')
                self.store.save(vtl_id,value)

    def dispatch(self, request):
        if not isinstance(request,dict): raise ValueError('Invalid request.')
        action=request.get('action');vtl=request.get('vtl_id')
        self.store.validate(vtl)
        if action in ('status','verify_current') and set(request)=={'action','vtl_id'}: return self.status(vtl)
        if action=='check' and set(request)=={'action','vtl_id','path'}: return self.check(vtl,request['path'])
        if action=='lock_data' and set(request)=={'action','vtl_id'}:
            self.lock_data(vtl);return {'vtl_id':vtl,'data_locked':True}
        if action=='apply': return self.apply(vtl,{k:v for k,v in request.items() if k not in ('action','vtl_id')})
        raise ValueError('Unknown storage action.')
