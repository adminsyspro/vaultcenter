#!/usr/bin/env python3
"""Create blank media for the installed mhVTL adapter, under maintenance.

The layout/CLI validator targets mhVTL 59f32ee50b3269c118965bffa29cf38b1553268f.
Request paths are never accepted. Interrupted work resumes only with identical
parameters and verified media; incomplete media is retained for operator repair.
"""
import ctypes
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import struct
import subprocess
import sys
import uuid
import cartridge_batches as batches
import cartridge_online as online
import cartridge_deletion as deletion
import export_readiness

helper = Path(__file__).with_name('cold-vtl-maintenance')
if not helper.exists():
    helper = Path(__file__).with_name('maintenance-controller.py')
loader = importlib.machinery.SourceFileLoader('maintenance', str(helper))
spec = importlib.util.spec_from_loader(loader.name, loader)
maintenance = importlib.util.module_from_spec(spec)
loader.exec_module(maintenance)

LEGACY = '00000000-0000-4000-8000-000000000001'
MIB = 1048576
VALIDATED_DRIVES = {('IBM', 'ULT3580-TD8'), ('HPE', 'Ultrium 8-SCSI')}
MAX_CAPACITY_MIB = 16777216
FIELDS = ('vtl_id', 'barcode', 'media_type', 'capacity_bytes', 'segment_size_mib', 'multipart_part_size_mib')
MARKER = '.vaultcenter-create-pending'


def normalized_request(request):
    # Pre-multipart journals retain their original segment size and imply 256 MiB parts.
    return {**request, 'multipart_part_size_mib': request.get('multipart_part_size_mib', 256)}


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def safe_path(path, directory=False):
    if not path.is_absolute() or path.resolve() != path or path.is_symlink():
        raise RuntimeError('Configuration path is not canonical or is a symbolic link.')
    info = path.stat()
    if not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode) and info.st_nlink == 1):
        raise RuntimeError('Path missing or file format not supported.')


def rename_new(source, target):
    """Atomic exclusive directory rename: never replace even an empty directory."""
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == 'linux':
        result = libc.renameat2(-100, os.fsencode(source), -100, os.fsencode(target), 1)
    elif sys.platform == 'darwin':
        result = libc.renamex_np(os.fsencode(source), os.fsencode(target), 4)
    else:
        raise RuntimeError('Exclusive rename unavailable on this system.')
    if result:
        raise OSError(ctypes.get_errno(), 'Exclusive placement refused')
    sync_dir(source.parent)
    sync_dir(target.parent)


def attributes(data):
    result = {}
    while data:
        if len(data) < 4:
            raise RuntimeError('Incomplete mhVTL metadata; manual recovery required.')
        key, size = struct.unpack('=HH', data[:4])
        if len(data) < 4 + size or key in result:
            raise RuntimeError('Invalid mhVTL metadata; manual recovery required.')
        result[key], data = data[4:4+size], data[4+size:]
    return result


def blank_inventory(path, request):
    safe_path(path, True)
    names = {'mam', 'mhvtl_data', 'data.0', 'indx.0', 'meta.0'}
    marker = path / MARKER
    if marker.exists() or marker.is_symlink():
        safe_path(marker)
        if normalized_request(json.loads(marker.read_text())) != request:
            raise RuntimeError('The creation marker does not match the original settings.')
    if {p.name for p in path.iterdir()} - {MARKER} != names:
        raise RuntimeError('Incomplete mhVTL cartridge; manual recovery required.')
    files = {}
    for name in sorted(names):
        file = path / name
        safe_path(file)
        if file.stat().st_size > 65536:
            raise RuntimeError('The media already exists and is not a blank cartridge.')
        files[name] = file.read_bytes()
    mam = files['mam']
    if len(mam) < 8 or struct.unpack('=II', mam[:8]) != (6, 4):
        raise RuntimeError('Unknown or incomplete mhVTL MAM version.')
    attrs = attributes(mam[8:])
    private = attributes(files['mhvtl_data'])
    density = {'LTO7': b'\x5c', 'LTO8': b'\x5e'}[request['media_type']]
    capacity = request['capacity_bytes'].to_bytes(8, 'big')
    if (attrs.get(0) != capacity or attrs.get(1) != capacity
            or attrs.get(0x405) != density or attrs.get(0x408) != b'\0'
            or attrs.get(0x806, b'').rstrip(b'\0 ') != request['barcode'].encode()
            or attrs.get(0x1623) != b'\0' or len(private.get(9, b'')) != 32
            or files['data.0'] or files['indx.0'] or files['meta.0'] != bytes(512)):
        raise RuntimeError('The media does not match the expected settings and blank format.')
    return {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}


class Cartridges:
    def __init__(self, config, root=maintenance.ROOT, controller=None, runner=None):
        self.root = root
        self.scoped = config.get('vtl_id')
        self.vtl_id = self.scoped or LEGACY
        self.library_id = config.get('runtime',{}).get('library',{}).get('id',10)
        self.drive_id = config.get('runtime',{}).get('drives',[{'id':11}])[0]['id']
        self.media = Path(config.get('media_root', '/opt/mhvtl'))
        self.database = Path(config.get('database', str(root / 'catalog.db')))
        self.config_dir = Path(config.get('mhvtl_config', '/etc/mhvtl'))
        self.contents = self.config_dir / f'library_contents.{self.library_id}'
        self.journals = root / 'cartridge-creations'
        self.pending = root / 'cartridge-creation-pending.json'
        self.controller = controller or maintenance.Controller(config, root)
        self.runner = runner or self.run_mktape
        self.lock_fd = None
        self.creation_mode = None
        self.live_slots = None

    @contextmanager
    def db(self):
        safe_path(self.database)
        connection = sqlite3.connect(self.database, timeout=15)
        connection.row_factory = sqlite3.Row
        connection.execute('PRAGMA foreign_keys=ON')
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    def installed(self, request):
        if request.get('vtl_id') != self.vtl_id:
            raise RuntimeError('This VTL does not match the installed mhVTL library.')
        safe_path(self.media, True)
        with self.db() as db:
            query="SELECT id,media_root,lifecycle FROM vtls WHERE id=?" if self.scoped else "SELECT id,media_root,lifecycle FROM vtls WHERE id=? OR lifecycle!='draft' OR media_root IS NOT NULL"
            rows = db.execute(query, (self.vtl_id,)).fetchall()
            if len(rows) != 1 or rows[0]['id'] != self.vtl_id or rows[0]['lifecycle'] != 'ready' or rows[0]['media_root'] != str(self.media):
                raise RuntimeError('Ambiguous media root or VTL activation; creation refused.')

    def layout(self):
        safe_path(self.config_dir, True)
        device = self.config_dir / 'device.conf'
        safe_path(device)
        text = device.read_text()
        libraries = re.findall(r'^Library:\s*(\d+)\b', text, re.M)
        drives = re.findall(r'^Drive:\s*(\d+)\b', text, re.M)
        # Maintenance controls precisely these units; extra devices are unvalidated.
        if libraries != [str(self.library_id)] or drives != [str(self.drive_id)]:
            raise RuntimeError('mhVTL topology not supported by the installed controller.')
        library, drive = re.split(r'^Drive:', text, maxsplit=1, flags=re.M)
        homes = re.findall(r'^\s*Home directory:\s*(.*?)\s*$', library, re.M)
        if homes != [str(self.media)]:
            raise RuntimeError('The mhVTL Home directory does not match the configured media root.')
        if not re.search(rf'^\s*Library ID:\s*{self.library_id}\s+Slot:\s*0?1\s*$', drive, re.M):
            raise RuntimeError('The drive does not match the installed library.')
        vendors = re.findall(r'^\s*Vendor identification:\s*(.*?)\s*$', drive, re.M)
        products = re.findall(r'^\s*Product identification:\s*(.*?)\s*$', drive, re.M)
        if len(vendors) != 1 or len(products) != 1 or (vendors[0], products[0]) not in VALIDATED_DRIVES:
            raise RuntimeError('No validated media type for the installed drive.')
        safe_path(self.contents)
        content = self.contents.read_text()
        entries, seen = [], set()
        version_seen = False
        for index, line in enumerate(content.splitlines(keepends=True)):
            if not line.strip() or line.lstrip().startswith('#'):
                continue
            if not entries and not version_seen and line.strip() == 'VERSION: 2':
                version_seen = True
                continue
            match = re.fullmatch(r'(Drive|Picker|MAP|Slot)\s+(\d+):\s*([A-Z0-9-]*)\s*', line)
            if not match or (match[1], int(match[2])) in seen:
                raise RuntimeError('Invalid or ambiguous mhVTL inventory.')
            kind, number, barcode = match[1], int(match[2]), match[3]
            if number < 1:
                raise RuntimeError('Invalid slot number.')
            seen.add((kind, number))
            entries.append((kind, number, barcode, index))
        content, entries = online.overlay(self,content,entries)
        return ['LTO7', 'LTO8'], content, entries

    def ready(self):
        if deletion.pending(self.root):
            raise RuntimeError('Complete interrupted local deletion before creating a cartridge.')
        maintenance.storage_available(self.root)
        if self.controller.read()['phase']=='online':
            if self.controller.block.exists() or set(self.controller.services().values())!={'active'}:
                raise RuntimeError('Services are transitioning: online creation is unavailable.')
            observed=online.status(self)
            self.creation_mode='online'
            self.live_slots=observed['free_slots']
        else:
            self.controller.check()
            self.creation_mode='maintenance'
            self.live_slots=None
        self.controller.no_storage()
        if (self.root/'reintegration-pending.json').exists():
            raise RuntimeError('Recover the reintegration before creating a cartridge.')
        self.controller.no_workers()
        with self.db() as db:
            if db.execute("SELECT count(*) FROM jobs WHERE status IN ('queued','running')"+(' AND vtl_id=?' if self.scoped else ''),(self.vtl_id,) if self.scoped else ()).fetchone()[0]:
                raise RuntimeError('Wait for all VTL jobs to finish.')

    def options(self, request):
        result = {'configured': True, 'online_creation': False, 'ready': False, 'reason': None, 'media_types': [],
                  'default_media_type': None, 'default_capacity_mib': 2048,
                  'default_segment_size_mib': 1048576, 'min_capacity_mib': 1,
                  'max_capacity_mib': MAX_CAPACITY_MIB, 'min_segment_size_mib': 16,
                  'max_segment_size_mib': 1048576, 'default_multipart_part_size_mib': 256,
                  'min_multipart_part_size_mib': 16, 'max_multipart_part_size_mib': 1024,
                  'free_slots': 0, 'pending_creation': None, 'pending_batch': None, 'storage': None}
        try:
            self.installed(request)
            types, _, entries = self.layout()
            result.update(media_types=types, default_media_type=types[-1],
                          free_slots=sum(e[0] == 'Slot' and not e[2] for e in entries))
            if self.pending.exists():
                safe_path(self.pending)
                barcode = json.loads(self.pending.read_text())['barcode']
                if not isinstance(barcode,str) or not re.fullmatch('[A-Z0-9-]{6,16}',barcode):
                    raise RuntimeError('Invalid creation journal.')
                path = self.journals/(barcode+'.json')
                safe_path(path)
                pending = normalized_request(json.loads(path.read_text())['request'])
                if pending['vtl_id'] != request['vtl_id']:
                    raise RuntimeError('Creation journal of another VTL.')
                result['pending_creation'] = {key:pending[key] for key in FIELDS if key != 'vtl_id'}
            result['pending_batch'] = batches.pending_batch(self.root)
            result['storage'] = batches.storage_budget(self)
            self.ready()
            result['online_creation']=self.creation_mode=='online'
            if self.live_slots is not None:
                result['free_slots']=self.live_slots
            if not result['free_slots'] and not self.pending.exists() and not result['pending_batch']:
                raise RuntimeError('No free slots in the library.')
            result['ready'] = True
        except Exception as error:
            result['reason'] = str(error)
        return result

    def run_mktape(self, args):
        # The child retains flock if the API/helper is killed while mktape runs.
        result = subprocess.run(args, capture_output=True, text=True, timeout=120,
                                pass_fds=(self.lock_fd,), check=False)
        if result.returncode:
            raise RuntimeError('mktape failed; retry with the same settings to recover.')

    def save(self, journal):
        maintenance.atomic_json(self.journals/(journal['barcode']+'.json'), journal)

    def catalogue(self, request):
        with self.db() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT * FROM cartridges WHERE vtl_id=? AND barcode=?', (self.vtl_id, request['barcode'])).fetchone()
            if row:
                if any(row[key] != request[key] for key in FIELDS):
                    raise RuntimeError('This cartridge exists with different settings.')
            else:
                db.execute("INSERT INTO cartridges(vtl_id,barcode,media_type,capacity_bytes,segment_size_mib,multipart_part_size_mib,state) VALUES(?,?,?,?,?,?,'writable')", tuple(request[k] for k in FIELDS))
                row = db.execute('SELECT * FROM cartridges WHERE vtl_id=? AND barcode=?', (self.vtl_id, request['barcode'])).fetchone()
            return dict(row)

    def validate_creation(self, request):
        types, _, _ = self.layout()
        barcode = request.get('barcode')
        if not isinstance(barcode, str) or not re.fullmatch('[A-Z0-9-]{6,16}', barcode):
            raise RuntimeError('Invalid barcode (6 to 16 uppercase letters, numbers or hyphens).')
        if request.get('media_type') not in types:
            raise RuntimeError('Media type incompatible with the installed drive.')
        capacity, segment = request.get('capacity_bytes'), request.get('segment_size_mib')
        if type(capacity) is not int or not MIB <= capacity <= MAX_CAPACITY_MIB*MIB or capacity % MIB:
            raise RuntimeError('Capacity must be a whole number of MiB within the permitted limits.')
        if type(segment) is not int or not 16 <= segment <= 1048576:
            raise RuntimeError('Segment size must be between 16 MiB and 1 TiB.')
        part = request['multipart_part_size_mib']
        if type(part) is not int or not 16 <= part <= 1024 or segment > part * 10000:
            raise RuntimeError('Multipart parts must be 16 to 1024 MiB, with at most 10,000 parts per segment.')
        return request

    def create(self, request):
        request = normalized_request(request)
        self.installed(request)
        self.validate_creation(request)
        types, before, entries = self.layout()
        barcode = request['barcode']
        capacity = request['capacity_bytes']
        pending_batch = batches.pending_batch(self.root)
        if pending_batch and pending_batch['request']['request_id'] != getattr(self,'batch_request_id',None):
            raise RuntimeError('Resume the interrupted batch before another creation.')
        request = {key: request[key] for key in FIELDS}
        self.ready()
        maintenance.mark_data(self.root,self.scoped)
        self.journals.mkdir(exist_ok=True, mode=0o700)
        safe_path(self.journals, True)
        path = self.journals/(barcode+'.json')
        target = self.media/barcode
        if self.pending.exists() and json.loads(self.pending.read_text())['barcode'] != barcode:
            raise RuntimeError('An interrupted creation must be recovered with its original settings.')
        if path.exists():
            safe_path(path)
            journal = json.loads(path.read_text())
            if normalized_request(journal['request']) != request:
                raise RuntimeError('Retry creation with exactly the same settings.')
        else:
            with self.db() as db:
                if db.execute('SELECT 1 FROM cartridges WHERE vtl_id=? AND barcode=?', (self.vtl_id,barcode)).fetchone():
                    raise RuntimeError('This cartridge already exists in the catalogue.')
            if target.exists() or target.is_symlink() or any(e[2] == barcode for e in entries):
                raise RuntimeError('This media or barcode already exists in mhVTL.')
            free = next((e for e in entries if e[0] == 'Slot' and not e[2]), None)
            if not free:
                raise RuntimeError('No free slots in the library.')
            lines = before.splitlines(keepends=True)
            lines[free[3]] = 'Slot %d: %s\n' % (free[1],barcode)
            journal = {'id':str(uuid.uuid4()), 'barcode':barcode, 'request':request,
                       'phase':'prepared', 'mode':self.creation_mode, 'before':before, 'after':''.join(lines), 'slot':free[1]}
            with batches.allocation_lock(self):
                batches.require_space(self,0 if pending_batch else capacity+batches.OVERHEAD)
                batches.record_volume(self,maintenance.atomic_json)
                self.save(journal)
        if journal.get('mode','maintenance') != self.creation_mode:
            raise RuntimeError('Resume this creation in its original mode (online or maintenance).')
        if journal['phase'] == 'completed':
            with self.db() as db:
                if db.execute('SELECT 1 FROM cartridge_deletions WHERE vtl_id=? AND barcode=?', (self.vtl_id,barcode)).fetchone():
                    raise RuntimeError('This cartridge was deleted locally; create a new cartridge.')
                row = db.execute('SELECT * FROM cartridges WHERE vtl_id=? AND barcode=?',(self.vtl_id,barcode)).fetchone()
            if not row or any(row[k] != request[k] for k in FIELDS) or not target.is_dir() or target.is_symlink():
                raise RuntimeError('The completed creation no longer matches the catalogue or the media.')
            if self.pending.exists():
                self.pending.unlink(); sync_dir(self.root)
            return dict(row)
        maintenance.atomic_json(self.pending, {'barcode':barcode})
        stage_parent = self.media/'.cold-vtl-creation'
        stage_parent.mkdir(exist_ok=True, mode=0o700)
        safe_path(stage_parent, True)
        stage = stage_parent/journal['id']
        stage.mkdir(exist_ok=True, mode=0o700)
        safe_path(stage, True)
        staged = stage/barcode
        if 'files' not in journal:
            if target.exists() or target.is_symlink():
                raise RuntimeError('Destination already present; manual recovery required.')
            if not staged.exists() and not staged.is_symlink():
                self.runner(['mktape','-l',str(self.library_id),'-H',str(stage),'-m',barcode,'-s',str(capacity//MIB),'-t','data','-d',request['media_type']])
            journal['files'] = blank_inventory(staged,request)
            owner = self.media.stat()
            os.chown(staged,owner.st_uid,owner.st_gid)
            os.chmod(staged,0o770)
            for file in staged.iterdir():
                os.chown(file,owner.st_uid,owner.st_gid)
                os.chmod(file,0o660)
                with file.open('rb') as stream: os.fsync(stream.fileno())
            sync_dir(staged)
            journal['phase'] = 'created'
            self.save(journal)
        self.ready()
        if staged.exists():
            if blank_inventory(staged,request) != journal['files']:
                raise RuntimeError('The prepared media has changed; manual recovery required.')
            # Discovery on API restart must not auto-import provisional media.
            marker = staged / MARKER
            if not marker.exists():
                with marker.open('x') as stream:
                    json.dump(request,stream); stream.flush(); os.fsync(stream.fileno())
                sync_dir(staged)
            rename_new(staged,target)
        if journal.get('mode')=='online':
            if journal['phase'] not in ('publishing','placed'):
                if blank_inventory(target,request) != journal['files']:
                    raise RuntimeError('The installed media does not match the creation journal.')
                journal['phase']='publishing'
                self.save(journal)
            online.insert(self,journal['id'],barcode)
            journal['phase']='placed'
            self.save(journal)
            return self.complete(request,journal,target)
        if blank_inventory(target,request) != journal['files']:
            raise RuntimeError('The installed media does not match the creation journal.')
        _, current, _ = self.layout()
        if current == journal['before']:
            info = self.contents.stat()
            temporary = self.contents.with_name('.cartridge-'+journal['id']+'.tmp')
            # A leftover temporary file is never trusted or truncated.
            if temporary.exists() or temporary.is_symlink():
                safe_path(temporary)
                if temporary.read_text() != journal['after']:
                    raise RuntimeError('Incomplete temporary inventory; manual recovery required.')
            else:
                with temporary.open('x') as stream:
                    stream.write(journal['after']); stream.flush(); os.fsync(stream.fileno())
                os.chmod(temporary,stat.S_IMODE(info.st_mode))
                os.chown(temporary,info.st_uid,info.st_gid)
            temporary.replace(self.contents)
            sync_dir(self.contents.parent)
        elif current != journal['after']:
            raise RuntimeError('The inventory has changed; no slot will be overwritten.')
        journal['phase'] = 'placed'
        self.save(journal)
        return self.complete(request,journal,target)

    def complete(self, request, journal, target):
        with batches.allocation_lock(self):
            result = self.catalogue(request)
            (target / MARKER).unlink(missing_ok=True)
            sync_dir(target)
            journal['phase'] = 'completed'
            self.save(journal)
            self.pending.unlink()
            sync_dir(self.root)
        return result

    def handle(self, action, request):
        if action == 'export-readiness':
            maintenance.revalidate(self.controller.config)
            maintenance.storage_available(self.root)
            return export_readiness.check(self, request)
        if action == 'options':
            return self.options(request)
        if action == 'deletion-options':
            return deletion.options(self, request)
        if action not in ('create','batch','delete-local'):
            raise RuntimeError('Unsupported action.')
        with (self.root/('operation.lock' if self.scoped else 'maintenance-controller.lock')).open('a+') as lock:
            try:
                fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            except BlockingIOError:
                raise RuntimeError('A maintenance operation is already in progress.')
            maintenance.revalidate(self.controller.config)
            self.lock_fd = lock.fileno()
            if action == 'delete-local':
                return deletion.delete(self, request, safe_path, rename_new, sync_dir, maintenance.atomic_json)
            if action == 'batch':
                return batches.step(self,request,maintenance.atomic_json,maintenance.mark_data)
            return self.create(request)


def main():
    os.umask(0o077)
    action = sys.argv[1] if len(sys.argv) == 2 else ''
    config = maintenance.load_config()
    return Cartridges(config,Path(config.get('state_root',str(maintenance.ROOT)))).handle(action,json.loads(sys.stdin.buffer.read(8192)))


if __name__ == '__main__':
    try:
        print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'error':str(error)}))
        sys.exit(1)
