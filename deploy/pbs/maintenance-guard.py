#!/usr/bin/env python3
"""PBS-side maintenance lease. Holds PBS's own flock for the configured tape drive."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import signal
import socket
import subprocess
import sys
import time
import uuid

CONFIG = Path('/etc/cold-vtl-guard.json')
STATE = Path('/var/lib/cold-vtl-guard/lease.json')
SOCKET = '/run/cold-vtl-guard/control.sock'


def atomic_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as file:
        json.dump(data, file)
        file.flush()
        os.fsync(file.fileno())
    temporary.replace(path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def run(args, timeout=15):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=False)
    if result.returncode:
        raise RuntimeError('PBS check failed: ' + args[0] + ': ' + result.stderr.strip()[:400])
    return result.stdout


class Guard:
    def __init__(self, config, state=None):
        self.config, self.state = config, state if state is not None else STATE
        self.lease = None
        self.lock = None
        self.bootstrap = False

    def inventory(self):
        """Read this VTL's live slot map without touching a maintenance lease."""
        if not self.config.get('vtl_id'):
            raise RuntimeError('Inventory requires a scoped VTL')
        changer = self.config['changer']
        configured = json.loads(run(['proxmox-tape', 'changer', 'config', changer,
                                     '--output-format', 'json'], timeout=3))
        if configured.get('path') != self.config['changer_path']:
            raise RuntimeError('PBS changer association changed.')
        stamp = int(time.time() * 1000)
        raw = run(['proxmox-tape', 'changer', 'status', changer, '--cache', 'false',
                   '--output-format', 'json'], timeout=4)
        if len(raw) > 1_048_576:
            raise RuntimeError('PBS inventory too large.')
        entries = json.loads(raw)
        if not isinstance(entries, list) or len(entries) > 4160:
            raise RuntimeError('Invalid PBS inventory.')
        kinds = {'slot': 'storage', 'import-export': 'import_export'}
        slots = [dict(number=e['entry-id'], kind=kinds[e['entry-kind']],
                      barcode=e.get('label-text'), position_known=True, observed_at=stamp)
                 for e in entries if e.get('entry-kind') in kinds]
        return dict(ok=True, vtl_id=self.config['vtl_id'], observed_at=stamp, slots=slots)

    def open_drive_lock(self, drive_path):
        # PBS uses systemd's path escaping, without resolving the configured symlink.
        name = run(['systemd-escape', '--path', drive_path]).strip()
        path = Path('/run/proxmox-backup/drive-lock') / name
        import pwd
        user = pwd.getpwnam('backup')
        path.parent.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
        os.chown(path.parent.parent, user.pw_uid, user.pw_gid)
        os.chmod(path.parent.parent, 0o755)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o750)
        os.chown(path.parent, user.pw_uid, user.pw_gid)
        file = path.open('a+')
        os.chown(path, user.pw_uid, user.pw_gid)
        os.chmod(path, 0o660)
        try:
            fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            file.close()
            raise RuntimeError('The PBS drive is in use; finish the task before maintenance.')
        return file

    def acquire(self):
        if not self.config.get('vtl_id'):
            self.lock=self.open_drive_lock(self.config['drive_path']);return
        self.association(live=False,allow_missing=self.bootstrap)
        stack=ExitStack()
        try:
            for drive in sorted(self.config['drives'],key=lambda d:d['path']):
                stream=self.open_drive_lock(drive['path']);stack.callback(stream.close)
            self.association(live=False,allow_missing=self.bootstrap)
            self.lock=stack
        except Exception:
            stack.close();raise

    def association(self, live=False, allow_missing=False):
        drives=json.loads(run(['proxmox-tape','drive','list','--output-format','json']))
        actual={d['name']:d for d in drives if d.get('changer')==self.config['changer']}
        expected={d['name']:d for d in self.config['drives']}
        if set(actual)-set(expected) or (not allow_missing and set(actual)!=set(expected)):raise RuntimeError('The drives of this VTL changed in PBS.')
        for drive in drives:
            if any(drive.get('path')==d['path'] and (drive.get('name')!=d['name'] or drive.get('changer')!=self.config['changer']) for d in expected.values()):raise RuntimeError('An expected drive is associated elsewhere in PBS.')
        registered=set(actual)==set(expected)
        for name,drive in expected.items():
            if name not in actual:continue
            observed=actual[name]
            if observed.get('path')!=drive['path'] or observed.get('changer-drivenum',0)!=drive['changer-drivenum']:
                raise RuntimeError('PBS drive association changed.')
            if live and observed.get('serial')!=drive['serial']:raise RuntimeError('PBS drive identity not confirmed.')
        if allow_missing:
            known=json.loads(run(['proxmox-tape','changer','list','--output-format','json']))
            if not any(c.get('name')==self.config['changer'] for c in known):return False
        changer=json.loads(run(['proxmox-tape','changer','config',self.config['changer'],'--output-format','json']))
        if changer.get('path')!=self.config['changer_path']:raise RuntimeError('PBS changer association changed.')
        if live:
            changers=json.loads(run(['proxmox-tape','changer','list','--output-format','json']))
            found=[c for c in changers if c.get('name')==self.config['changer'] and c.get('serial')==self.config['changer_serial']]
            if len(found)!=1:raise RuntimeError('PBS changer identity not confirmed.')
        return registered

    def handle_scoped(self, request):
        if not isinstance(request,dict) or set(request)-{'vtl_id','action','lease','barcode'} or request.get('vtl_id')!=self.config.get('vtl_id'):
            raise RuntimeError('Request or lease belongs to another VTL.')
        bootstrap=request.get('action')=='bootstrap' or self.bootstrap
        registered=self.association(live=request.get('action')=='verify',allow_missing=bootstrap and request.get('action')!='verify')
        return {'vtl_id':self.config['vtl_id'],'registered':registered,'bootstrap':bootstrap,**self.handle(request['action'],request.get('lease'),request.get('barcode'))}

    def idle(self):
        if self.config.get('vtl_id'):
            # PBS tape jobs serialize on the exact drive path locks held above.
            # Checking every unrelated task would incorrectly block library B.
            self.association(live=True);self.media_empty();return
        # A fixed configuration digest prevents silently guarding the wrong device after edits.
        import hashlib
        if hashlib.sha256(Path('/etc/proxmox-backup/tape.cfg').read_bytes()).hexdigest() != self.config['tape_config_sha256']:
            raise RuntimeError('The PBS drive configuration has changed; check the maintenance adapter.')
        tasks = json.loads(run(['proxmox-backup-manager', 'task', 'list', '--limit', '1000', '--output-format', 'json']))
        if not isinstance(tasks, list) or tasks:
            raise RuntimeError('A PBS task is active; wait for it to finish before maintenance.')
        self.media_empty()

    def media_empty(self, barcode=None):
        status = json.loads(run(['proxmox-tape', 'changer', 'status', self.config['changer'], '--cache', 'false', '--output-format', 'json']))
        drives = [entry for entry in status if entry.get('entry-kind') == 'drive']
        if self.config.get('vtl_id'):
            expected={d['changer-drivenum'] for d in self.config['drives']}
            if {d['entry-id'] for d in drives}!=expected or any(d.get('label-text') is not None or d.get('loaded-slot') is not None for d in drives):
                raise RuntimeError('Unload the drives of this VTL before maintenance.')
        elif len(drives) != 1 or not set(drives[0]).issubset({'entry-id', 'entry-kind', 'state'}):
            raise RuntimeError('Unload the cartridge from the drive in PBS before maintenance.')
        if barcode and len([e for e in status if e.get('entry-kind') == 'slot' and e.get('label-text') == barcode]) != 1:
            raise RuntimeError('The reintegrated cartridge is missing or duplicated in the PBS changer.')

    def restore(self):
        if self.state.exists():
            value = json.loads(self.state.read_text())
            if value.get('lease'):
                self.bootstrap=bool(value.get('bootstrap'))
                self.acquire()
                self.lease = value['lease']

    def handle(self, action, lease, barcode=None):
        if barcode is not None and (action != 'verify' or not re.fullmatch(r'[A-Z0-9-]{6,16}', barcode)):
            raise RuntimeError('Invalid verification barcode')
        if action == 'status':
            return {'lease': self.lease, 'locked': self.lock is not None}
        if not re.fullmatch(r'[0-9a-f-]{36}', lease or ''):
            raise RuntimeError('Invalid maintenance lease')
        if action == 'bootstrap':
            if not self.config.get('vtl_id') or (self.state.parent/'ever-verified').exists():
                raise RuntimeError('Bootstrap is permanently closed for this VTL.')
            self.bootstrap=True
            paths=[self.config['changer_path'],*[d['path'] for d in self.config['drives']]]
            if any(Path(path).exists() for path in paths):raise RuntimeError('A device is already visible: normal maintenance required.')
        if action in ('acquire','bootstrap'):
            if self.lease:
                if lease != self.lease:
                    raise RuntimeError('Another PBS maintenance lock already exists.')
                return {'lease': self.lease, 'locked': True}
            try:
                self.acquire()
                if action=='acquire':self.idle()
                atomic_json(self.state, {'lease': lease,'bootstrap':self.bootstrap})
                self.lease = lease
            except Exception:
                if self.lock:
                    self.lock.close()
                    self.lock = None
                raise
            return {'lease': self.lease, 'locked': True}
        if action == 'verify':
            if self.lease != lease or self.lock is None:
                raise RuntimeError('Maintenance lease mismatch')
            self.media_empty(barcode)
            if self.config.get('vtl_id'):
                atomic_json(self.state.parent/'ever-verified',{'vtl_id':self.config['vtl_id']})
                self.bootstrap=False
                atomic_json(self.state,{'lease':lease,'bootstrap':False})
            return {'lease': self.lease, 'locked': True, 'reachable': True}
        if action == 'release':
            if self.lease and self.lease != lease:
                raise RuntimeError('Maintenance lease mismatch')
            atomic_json(self.state, {'lease': None})
            self.lease = None
            self.bootstrap = False
            if self.lock:
                self.lock.close()
                self.lock = None
            return {'lease': None, 'locked': False}
        raise RuntimeError('Unsupported guard command')


def respond(guard,request):
    try:
        result=guard.handle_scoped(request) if guard.config.get('vtl_id') else guard.handle(request['action'],request.get('lease'),request.get('barcode'))
        return {'ok':True,**result}
    except Exception as error:
        identity={'vtl_id':guard.config['vtl_id']} if guard.config.get('vtl_id') else {}
        return {'ok':False,**identity,'error':str(error)}


def serve():
    guard = Guard(json.loads(CONFIG.read_text()))
    guard.restore()
    Path(SOCKET).parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as server:
        server.bind(SOCKET)
        server.listen(8)
        # Type=notify makes boot ordering wait until a persisted lease has been reacquired.
        address = os.environ.get('NOTIFY_SOCKET')
        if address:
            with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as notification:
                notification.connect('\0' + address[1:] if address.startswith('@') else address)
                notification.sendall(b'READY=1')
        while True:
            connection, _ = server.accept()
            with connection:
                connection.settimeout(5)
                try:
                    request = json.loads(connection.makefile('rb').readline(4096))
                    response = respond(guard,request)
                except Exception as error:
                    response = {'ok': False, 'error': str(error),**({'vtl_id':guard.config['vtl_id']} if guard.config.get('vtl_id') else {})}
                try:
                    connection.sendall(json.dumps(response).encode() + b'\n')
                except (BrokenPipeError, ConnectionResetError):
                    pass  # The lease must survive a disconnected SSH client.


def client(args):
    # Run independently: a slow changer must not delay the lease daemon.
    if args == ['inventory'] and VTL_ID is not None:
        print(json.dumps(Guard(json.loads(CONFIG.read_text())).inventory()))
        return 0
    if not args or args[0] not in ('status', 'acquire', 'release', 'verify', 'bootstrap') or len(args) > 3 or (len(args) == 3 and args[0] != 'verify'):
        raise RuntimeError('Unsupported guard command')
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(35)
        connection.connect(SOCKET)
        request={'action': args[0], 'lease': args[1] if len(args) >= 2 else None, 'barcode': args[2] if len(args) == 3 else None}
        if VTL_ID is not None:request['vtl_id']=VTL_ID
        connection.sendall(json.dumps(request).encode()+b'\n')
        result = json.loads(connection.makefile('rb').readline(8192))
    print(json.dumps(result))
    return 0 if result['ok'] else 1


VTL_ID=None

def select_vtl(args):
    global CONFIG, STATE, SOCKET, VTL_ID
    if len(args)>=2 and args[0]=='--vtl':
        value=args[1]
        if str(uuid.UUID(value))!=value:raise RuntimeError('Invalid VTL identifier.')
        CONFIG=Path('/etc/cold-vtl-guard/vtls')/(value+'.json')
        if not CONFIG.is_file():raise RuntimeError('VTL not registered in the PBS guard.')
        STATE=Path('/var/lib/cold-vtl-guard/vtls')/value/'lease.json'
        SOCKET='/run/cold-vtl-guard/'+value+'/control.sock';VTL_ID=value
        return args[2:]
    return args

if __name__ == '__main__':
    os.umask(0o077)
    try:
        args=sys.argv[1:]
        if args==['ssh']:args=shlex.split(os.environ.get('SSH_ORIGINAL_COMMAND',''))
        args=select_vtl(args)
        if args==['serve']:serve()
        else:sys.exit(client(args))
    except Exception as error:
        print(json.dumps({'ok':False,'error':str(error),**({'vtl_id':VTL_ID} if VTL_ID else {})}));sys.exit(1)
