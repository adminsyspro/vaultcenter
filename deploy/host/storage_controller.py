#!/usr/bin/env python3
"""Restricted host bridge for empty tape storage selection; no data migration."""
import argparse
from contextlib import closing
import fcntl
import hashlib
import json
import os
from pathlib import Path
import socket
import socketserver
import sqlite3
import struct
import subprocess
import sys
import threading
import time
import uuid
from storage_paths import inspect_path, verify_identity, directory

MAX_MESSAGE = 65536

def atomic(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp')
    with temporary.open('w') as stream:
        os.chmod(temporary, 0o600)
        json.dump(value, stream); stream.flush(); os.fsync(stream.fileno())
    os.replace(temporary, path)
    with directory(str(path.parent)) as fd: os.fsync(fd)

class Controller:
    def __init__(self, config, root):
        self.config, self.root = config, Path(root)
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.state = Path(config['state_dir'])
        self.file = self.root / 'storage.json'
        self.lock = None
        self.mutex = threading.RLock()
        if not self.file.exists(): atomic(self.file, {'mode':'managed','phase':'unconfigured','revision':0,'path':None,'identity':None,'operation':None,'message':None})

    def read(self): return json.loads(self.file.read_text())

    def status(self):
        value = self.read(); value['capacity'] = None
        if value['phase'] == 'ready':
            try:
                value['capacity'] = verify_identity(value['path'], value['identity'])
                self.verify_binding(value['path'])
                if (self.state/'storage-block').exists():
                    raise ValueError('Storage finalisation interrupted: run the recovery on the VM.')
            except (ValueError,OSError,subprocess.SubprocessError) as error: value.update(phase='unavailable', message=str(error) if isinstance(error,ValueError) else 'The appliance mount cannot be verified.')
        # Request payloads and mount identity are private host bookkeeping.
        return {key:value.get(key) for key in ('mode','phase','revision','path','capacity','operation','message')}

    def forbidden(self):
        return [str(self.root), str(self.state), str(Path(self.config['compose_file']).parent / 'config'), *self.config.get('forbidden', [])]

    def check(self, path):
        result = inspect_path(path, self.forbidden())
        token = uuid.uuid4().hex
        atomic(self.root / 'check.json', {'check_id':token,'path':result['path'],'identity':result['identity'],'revision':self.read()['revision'],'expires':time.time()+300})
        return {key:result[key] for key in ('path','filesystem','total_bytes','available_bytes')} | {'check_id':token}

    def acquire(self):
        if self.lock is not None: raise ValueError('A storage application is already in progress.')
        lock = (self.state/'maintenance-controller.lock').open('a+')
        try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            lock.close(); raise ValueError('A maintenance operation is in progress.')
        self.lock = lock

    def release(self):
        if self.lock: self.lock.close(); self.lock = None

    def idle(self, current):
        for name in ('reintegration-pending.json','cartridge-creation-pending.json','cartridge-deletion-pending.json'):
            if (self.state/name).exists(): raise ValueError('A tape operation must be recovered before this change.')
        database = self.state/'catalog.db'
        if not database.is_file(): raise ValueError('The catalogue must be initialised before choosing storage.')
        with closing(sqlite3.connect(database.as_uri()+'?mode=ro',uri=True,timeout=5)) as db:
            for query in ('SELECT count(*) FROM cartridges', "SELECT count(*) FROM jobs WHERE status IN ('queued','running')", "SELECT count(*) FROM archive_requests WHERE phase NOT IN ('archived','restored','failed')", "SELECT count(*) FROM bucket_rotations WHERE phase NOT IN ('switched','failed')"):
                if db.execute(query).fetchone()[0]: raise ValueError('Tapes or operations present: changing the path requires a dedicated migration.')
        if current.get('identity'):
            verify_identity(current['path'],current['identity'])
        source = current['path'] or self.config['media_dir']
        with directory(source) as fd:
            if os.listdir(fd): raise ValueError('The current storage contains data; automatic move not allowed.')

    def require_maintenance(self):
        # Bootstrap is a one-shot root-owned installation mode, never API input.
        # The installer enables it only before any tape services are exposed.
        if self.config.get('bootstrap') and (self.state/'storage-bootstrap').exists():
            return
        running=self.command(['docker','inspect',self.config['container'],'--format','{{.State.Running}}']).strip()=='true'
        if running:
            command=['docker','exec',self.config['container'],'/usr/local/libexec/cold-vtl-maintenance','check-lease']
        else:
            command=['docker','compose','--project-name',self.config['project'],'-f',self.config['compose_file'],'run','--rm','-T','--no-deps','--entrypoint','/usr/local/libexec/cold-vtl-maintenance','appliance','check-lease']
        output = self.command(command)
        value = json.loads(output)
        if not value.get('ready') or not value.get('pbs_locked') or not value.get('blocked'):
            raise ValueError('Enter verified PBS maintenance before changing storage.')

    def apply(self, request):
        if set(request) != {'path','revision','request_id','check_id','actor'} or not isinstance(request['revision'],int) or isinstance(request['revision'],bool):
            raise ValueError('Invalid storage request.')
        for field in ('request_id','check_id','actor'):
            if not isinstance(request[field],str) or not 1 <= len(request[field]) <= 128: raise ValueError('Invalid identifier.')
        fingerprint = hashlib.sha256(json.dumps(request,sort_keys=True).encode()).hexdigest()
        with self.mutex:
            current = self.read()
            if current.get('request_id') == request['request_id']:
                if current.get('fingerprint') != fingerprint: raise ValueError('This identifier belongs to another request.')
                return self.status()
            if current['phase'] in ('applying','recovery_required'): raise ValueError('An application must be completed or recovered.')
            if current['revision'] != request['revision']: raise ValueError('Configuration changed elsewhere. Reload before saving.')
            checked = json.loads((self.root/'check.json').read_text())
            if checked['check_id'] != request['check_id'] or checked['path'] != request['path'] or checked['revision'] != request['revision'] or checked['expires'] < time.time():
                raise ValueError('Check the path again before saving.')
            verify_identity(request['path'],checked['identity'])
            destination = inspect_path(request['path'],self.forbidden())
            if destination['identity'] != checked['identity']:
                raise ValueError('The folder has changed since it was verified.')
            self.acquire()
            try:
                self.idle(current); self.require_maintenance()
                runtime = self.runtime('inspect')
                if not runtime['stopped']: raise ValueError('Stop the tape services and workers before changing storage.')
                atomic(self.state/'storage-block', {'reason':'storage application'})
                atomic(self.state/'storage-services-held', {'reason':'explicit resume required'})
                value = {**current,'phase':'applying','message':None,'operation':{'id':request['request_id'],'phase':'prepared'},'previous':current,'candidate':destination,'request_id':request['request_id'],'fingerprint':fingerprint}
                atomic(self.file,value)
                return self.status()
            except Exception:
                self.release(); raise

    def command(self, args, timeout=30):
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            print('Storage command failed:',args[:3],result.stderr[-2048:],file=sys.stderr,flush=True)
            raise ValueError('The system command failed; check the storage service log.')
        return result.stdout

    def compose(self):
        return ['docker','compose','--project-name',self.config['project'],'-f',self.config['compose_file'],'-f',str(self.root/'mount.json')]

    def verify_binding(self,path):
        observed=self.runtime('binding')
        with directory(path) as fd:
            actual=os.fstat(fd)
            if observed['media_source']!=path or observed['inode']!=actual.st_ino or observed['device']!=actual.st_dev:
                raise ValueError('The container uses another mount. Recreate the appliance with the verified volume.')

    def runtime(self, action, path=None):
        if action=='binding':
            item=json.loads(self.command(['docker','inspect',self.config['container']]))[0]
            source=next(m['Source'] for m in item['Mounts'] if m['Destination']=='/opt/mhvtl')
            inode,device=map(int,self.command(['docker','exec',self.config['container'],'stat','-c','%i %d','/opt/mhvtl']).split())
            return {'media_source':source,'inode':inode,'device':device}
        if action == 'stop':
            self.command(['docker','stop','--time','300',self.config['container']],330)
        elif action == 'recreate':
            self.command(self.compose()+['config','--quiet'])
            self.command(self.compose()+['up','-d','--no-deps','--force-recreate','appliance'],150)
        elif action == 'inspect':
            item=json.loads(self.command(['docker','inspect',self.config['container']]))[0]
            source=next(m['Source'] for m in item['Mounts'] if m['Destination']=='/opt/mhvtl')
            stopped=True
            for unit in ('tgt.service','vtltape@11.service','vtllibrary@10.service'):
                state=self.command(['docker','exec',self.config['container'],'systemctl','show','--property=ActiveState','--value',unit]).strip()
                stopped = stopped and state == 'inactive'
            processes=self.command(['docker','exec',self.config['container'],'ps','-eo','comm=']).splitlines()
            stopped = stopped and not any(p.strip() in ('cold-vtl-worker','vtltape','vtllibrary','tgtd') for p in processes)
            ino=int(self.command(['docker','exec',self.config['container'],'stat','-c','%i','/opt/mhvtl']).strip())
            return {'media_source':source,'inode':ino,'stopped':stopped,'healthy':item['State'].get('Health',{}).get('Status')=='healthy'}

    def execute(self):
        with self.mutex:
            value=self.read()
            if value['phase']=='ready' and (self.state/'storage-block').exists():
                self.acquire()
                try:
                    verify_identity(value['path'],value['identity']); self.verify_binding(value['path'])
                    self.require_maintenance()
                    if not self.runtime('inspect')['stopped']:
                        raise ValueError('Tape services active: finalisation refused.')
                    (self.state/'storage-block').unlink()
                finally: self.release()
                return
            if value['phase'] not in ('applying','recovery_required'): return
            if self.lock is None: self.acquire()
            try:
                value.update(phase='applying',message=None); atomic(self.file,value)
                target=value['candidate']; verify_identity(target['path'],target['identity'])
                self.require_maintenance()
                self.runtime('stop')
                self.idle(value['previous'])
                verify_identity(target['path'],target['identity'])
                with directory(target['path']) as fd:
                    if os.listdir(fd): raise ValueError('The destination now contains data.')
                value['operation']['phase']='stopped'; atomic(self.file,value)
                override={'services':{'appliance':{'volumes':[{'type':'bind','source':target['path'],'target':'/opt/mhvtl','bind':{'create_host_path':False}}]}}}
                atomic(self.root/'mount.json',override)
                self.runtime('recreate',target['path'])
                value['operation']['phase']='recreated'; atomic(self.file,value)
                deadline=time.monotonic()+100
                while True:
                    try:
                        observed=self.runtime('inspect')
                        if observed['healthy']: break
                    except (ValueError,OSError,subprocess.SubprocessError):
                        pass
                    if time.monotonic()>deadline: raise ValueError('The console does not respond after the storage change.')
                    time.sleep(1)
                verify_identity(target['path'],target['identity'])
                if observed['media_source']!=target['path'] or not observed['stopped'] or observed.get('inode',target['identity']['inode'])!=target['identity']['inode']:
                    raise ValueError('The mount or the service stop is not confirmed.')
                self.verify_binding(target['path'])
                value.update(phase='ready',path=target['path'],identity=target['identity'],revision=value['previous']['revision']+1)
                value['operation']['phase']='verified'; atomic(self.file,value)
                (self.state/'storage-block').unlink(missing_ok=True)
            except Exception as error:
                value.update(phase='recovery_required',message=str(error) if isinstance(error,ValueError) else 'Application interrupted; check the storage service on the VM.')
                atomic(self.file,value)
            finally: self.release()

    def dispatch(self, request):
        action=request.get('action')
        if action=='status' and set(request)=={'action'}: return self.status()
        if action=='check' and set(request)=={'action','path'}: return self.check(request['path'])
        if action=='apply': return self.apply({k:v for k,v in request.items() if k!='action'})
        raise ValueError('Unknown storage action.')

class Server(socketserver.ThreadingMixIn,socketserver.UnixStreamServer):
    daemon_threads=True
    def __init__(self,path,controller):
        self.controller=controller; self.slots=threading.BoundedSemaphore(8)
        super().__init__(path,Handler)
    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False): request.close(); return
        super().process_request(request,address)
    def process_request_thread(self,request,address):
        try: super().process_request_thread(request,address)
        finally: self.slots.release()

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(15)
        queued=False
        try:
            _,uid,_=struct.unpack('3i',self.connection.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12))
            if uid!=0: raise ValueError('Access restricted to the VaultCenter service.')
            raw=self.rfile.readline(MAX_MESSAGE+1)
            if len(raw)>MAX_MESSAGE or not raw.endswith(b'\n'): raise ValueError('Request too large or incomplete.')
            request=json.loads(raw)
            if not isinstance(request,dict): raise ValueError('Invalid request.')
            result=self.server.controller.dispatch(request)
            queued=request.get('action')=='apply' and result['phase']=='applying'
            response={'ok':True,'result':result}
        except Exception as error:
            response={'ok':False,'error':{'code':'storage_conflict','message':str(error) if isinstance(error,ValueError) else 'The storage service cannot process this request.'}}
        try:
            self.wfile.write(json.dumps(response).encode()+b'\n'); self.wfile.flush()
        finally:
            if queued:
                args=(request['vtl_id'],) if self.server.controller.config.get('isolated') else ()
                threading.Thread(target=self.server.controller.execute,args=args,daemon=True).start()

def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--config',default='/etc/vaultcenter-host/storage.json'); parser.add_argument('--recover',action='store_true')
    parser.add_argument('--vtl'); parser.add_argument('--prepare-mounts',action='store_true')
    args=parser.parse_args(); os.umask(0o077)
    config=json.loads(Path(args.config).read_text())
    if config.get('isolated'):
        from vtl_storage import VtlStorage
        from storage_mounts import Mounts
        controller=VtlStorage(config)
        if args.prepare_mounts:
            mounts=Mounts(config)
            mounts.restore_states(json.loads(file.read_text()) for file in (controller.store.root/'vtls').glob('*/storage.json'))
            return
        if args.recover:
            controller.execute(args.vtl);print(json.dumps(controller.status(args.vtl)));return
    else:
        if args.prepare_mounts: raise SystemExit('Isolated storage is not configured.')
        controller=Controller(config,config.get('host_state','/var/lib/vaultcenter-host'))
        if args.recover: controller.execute(); print(json.dumps(controller.status())); return
    path=Path(config.get('socket','/run/vaultcenter-host/storage.sock')); path.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    # The service owns this path. Never unlink arbitrary paths from client input.
    path.unlink(missing_ok=True)
    with Server(str(path),controller) as server: server.serve_forever()

if __name__=='__main__': main()
