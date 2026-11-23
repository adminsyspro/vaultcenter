"""Durable per-library batches; one existing journalled media operation per step."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re

OVERHEAD = 16 * 1024**2


def free_bytes(path):
    value = os.statvfs(path)
    return value.f_bavail * value.f_frsize


def pending_batch(root):
    values = [json.loads(p.read_text()) for p in (root/'cartridge-batches').glob('*.json')]
    pending = [v for v in values if v['phase'] != 'completed']
    if len(pending) > 1:
        raise RuntimeError('Several incomplete batches: recovery required.')
    return pending[0] if pending else None


@contextmanager
def allocation_lock(instance):
    # Only allocation accounting is global. Never hold it while mktape runs.
    with (instance.database.parent/'cartridge-allocation.lock').open('a+') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield


def storage_budget(instance):
    filesystem = str(os.statvfs(instance.media).f_fsid)
    reserved = 0
    with instance.db() as db:
        libraries = db.execute('SELECT id,media_root FROM vtls WHERE media_root IS NOT NULL').fetchall()
        for library in libraries:
            media = Path(library['media_root'])
            root = instance.root if library['id'] == instance.vtl_id else instance.database.parent/'vtls'/library['id']
            receipt = root/'cartridge-capacity-volume.json'
            recorded = json.loads(receipt.read_text())['filesystem'] if receipt.exists() else None
            try:
                attached = str(os.statvfs(media).f_fsid) == filesystem
            except FileNotFoundError:
                attached = False
            if not attached and recorded != filesystem:
                if recorded is None and not media.exists():
                    promised = db.execute('SELECT 1 FROM cartridges WHERE vtl_id=? LIMIT 1',(library['id'],)).fetchone()
                    if promised or pending_batch(root):
                        raise RuntimeError('Capacity cannot be verified: restore the volume of the other VTL before a new allocation.')
                continue
            capacities = {r['barcode']:r['capacity_bytes'] for r in db.execute(
                'SELECT barcode,capacity_bytes FROM cartridges WHERE vtl_id=? AND NOT EXISTS(SELECT 1 FROM cartridge_deletions x WHERE x.vtl_id=cartridges.vtl_id AND x.barcode=cartridges.barcode)',(library['id'],))}
            # Reservations survive a crash before the cartridge catalog commit.
            for file in (root/'cartridge-creations').glob('*.json'):
                journal = json.loads(file.read_text())
                if journal['phase'] != 'completed':
                    capacities[journal['barcode']] = journal['request']['capacity_bytes']
            batch = pending_batch(root)
            if batch:
                for barcode in batch['barcodes']:
                    capacities[barcode] = batch['request']['capacity_bytes']
            for barcode, capacity in capacities.items():
                if not re.fullmatch('[A-Z0-9-]{6,16}',barcode):
                    raise RuntimeError('Invalid catalogue barcode.')
                path = media/barcode
                if path.is_symlink():
                    raise RuntimeError('Media is a symbolic link: capacity cannot be verified.')
                used = 0
                metadata = 0
                if attached and path.exists():
                    for file in path.iterdir():
                        if file.is_symlink() or not file.is_file():
                            raise RuntimeError('Media cannot be verified for the capacity calculation.')
                        if file.name == 'data.0':
                            used += file.stat().st_blocks * 512
                        elif file.name in ('mam','mhvtl_data','meta.0','indx.0'):
                            metadata += file.stat().st_blocks * 512
                reserved += max(0,capacity - used) + max(0,OVERHEAD-metadata)
    stats = os.statvfs(instance.media)
    free = free_bytes(instance.media)
    return {'total_bytes':stats.f_blocks*stats.f_frsize,'free_bytes':free,'reserved_bytes':reserved,'available_bytes':max(0,free-reserved),'overhead_per_cartridge':OVERHEAD}


def record_volume(instance, atomic):
    with instance.db() as db:
        for library in db.execute('SELECT id,media_root FROM vtls WHERE media_root IS NOT NULL'):
            root = instance.root if library['id']==instance.vtl_id else instance.database.parent/'vtls'/library['id']
            receipt = root/'cartridge-capacity-volume.json'
            if receipt.exists() or not root.is_dir():
                continue
            try:
                stats=os.statvfs(library['media_root'])
            except FileNotFoundError:
                continue
            if not stats.f_flag & os.ST_RDONLY:
                atomic(receipt,{'filesystem':str(stats.f_fsid)})


def require_space(instance, additional=0):
    budget = storage_budget(instance)
    if budget['free_bytes'] < budget['reserved_bytes'] + additional:
        raise RuntimeError('Not enough space on the storage, counting the capacity of existing cartridges and the batch.')
    return budget


def allocate_barcodes(instance, media_type, count):
    with instance.db() as db:
        used = {row[0] for row in db.execute('SELECT barcode FROM cartridges WHERE vtl_id=?',(instance.vtl_id,))}
    used.update(p.stem for p in instance.journals.glob('*.json'))
    used.update(p.name for p in instance.media.iterdir())
    used.update(e[2] for e in instance.layout()[2] if e[2])
    for file in (instance.root/'cartridge-batches').glob('*.json'):
        used.update(json.loads(file.read_text())['barcodes'])
    suffix = 'L'+media_type[-1]
    series = {}
    for barcode in used:
        match = re.fullmatch(r'(.*?)(\d+)'+suffix,barcode)
        if match:
            prefix,digits = match.groups()
            n,width,total = series.get(prefix,(0,3,0))
            series[prefix] = (max(n,int(digits)),max(width,len(digits)),total+1)
    prefix = sorted(series,key=lambda p:(-series[p][2],p))[0] if series else 'CVT'
    number,width,_ = series.get(prefix,(0,3,0))
    result = []
    while len(result) < count:
        number += 1
        candidate = prefix+str(number).zfill(width)+suffix
        if len(candidate) > 16:
            raise RuntimeError('The barcode series has reached its limit.')
        if candidate not in used:
            result.append(candidate)
    return result


def step(instance, request, atomic, mark_data):
    fields = {'vtl_id','request_id','count','media_type','capacity_bytes','segment_size_mib','multipart_part_size_mib'}
    if set(request) != fields or not isinstance(request['request_id'],str) or not re.fullmatch('[a-f0-9]{32}',request['request_id']):
        raise RuntimeError('Invalid batch request.')
    count = request['count']
    if type(count) is not int or not 1 <= count <= 64:
        raise RuntimeError('Choose between 1 and 64 cartridges.')
    instance.installed(request)
    instance.ready()
    instance.validate_creation({**request,'barcode':'CVT001L'+str(request['media_type'])[-1]})
    directory = instance.root/'cartridge-batches'
    path = directory/(request['request_id']+'.json')
    with allocation_lock(instance):
        pending = pending_batch(instance.root)
        if pending and pending['request']['request_id'] != request['request_id']:
            raise RuntimeError('Resume the interrupted batch before a new creation.')
        if path.exists():
            journal = json.loads(path.read_text())
            if journal.get('mode','maintenance')!=instance.creation_mode:
                raise RuntimeError('Resume the batch in its original mode.')
            if journal['request'] != request:
                raise RuntimeError('Resume the batch with exactly the same settings.')
            if journal['phase'] == 'completed':
                with instance.db() as db:
                    if any(db.execute('SELECT 1 FROM cartridge_deletions WHERE vtl_id=? AND barcode=?', (instance.vtl_id,b)).fetchone() for b in journal['barcodes']):
                        raise RuntimeError('This batch contains locally deleted cartridges; create a new batch.')
                return journal
        else:
            if instance.pending.exists():
                raise RuntimeError('Resume the interrupted cartridge before creating a batch.')
            slots = instance.live_slots if instance.live_slots is not None else sum(e[0]=='Slot' and not e[2] for e in instance.layout()[2])
            if count > slots:
                raise RuntimeError('The batch exceeds the number of free slots.')
            require_space(instance,count*(request['capacity_bytes']+OVERHEAD))
            journal = {'request':request,'barcodes':allocate_barcodes(instance,request['media_type'],count),'completed':0,'phase':'creating','mode':instance.creation_mode}
            record_volume(instance,atomic)
            mark_data(instance.root,instance.scoped)
            directory.mkdir(mode=0o700,exist_ok=True)
            atomic(path,journal)
        require_space(instance)
    # Keep the same per-cartridge journal on retries, including after catalog commit.
    barcode = journal['barcodes'][journal['completed']]
    instance.batch_request_id = request['request_id']
    try:
        instance.create({k:v for k,v in {**request,'barcode':barcode}.items() if k not in ('count','request_id')})
    finally:
        instance.batch_request_id = None
    journal['completed'] += 1
    if journal['completed'] == count:
        journal['phase'] = 'completed'
    with allocation_lock(instance):
        atomic(path,journal)
    return journal
