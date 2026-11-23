"""Catalogue-independent recovery preflight and receipt validation.

The journal names an immutable source job, never a client-supplied filesystem
path. All local changes remain owned by the reintegration maintenance journal.
"""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import re
import shutil
import struct
import time


def cartridges(instance):
    path = Path(__file__).with_name('cold-vtl-cartridges')
    if not path.exists():
        path = Path(__file__).with_name('cartridge-controller.py')
    loader = importlib.machinery.SourceFileLoader('recovery_cartridges', str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module, module.Cartridges(instance.config, instance.root, instance.controller)


def local_state(instance, barcode):
    with instance.db() as db:
        row = instance.query(db, 'SELECT * FROM cartridges WHERE barcode=?', (barcode,)).fetchone()
        deleted = instance.query(db, 'SELECT * FROM cartridge_deletions WHERE barcode=?', (barcode,)).fetchone()
    current = instance.media / barcode
    exists = current.exists() or current.is_symlink()
    # Missing files without a tombstone do not prove that a catalogue identity is absent.
    return row, deleted, exists or (row is not None and deleted is None)


def check(instance, request):
    barcode = request.get('barcode', '')
    required = request.get('required_bytes')
    if not isinstance(barcode, str) or not re.fullmatch('[A-Z0-9-]{6,16}', barcode) or type(required) is not int or required < 0:
        raise RuntimeError('Invalid recovery settings.')
    module, device = cartridges(instance)
    device.installed({'vtl_id': device.vtl_id})
    _, _, entries = device.layout()
    row, deleted, collision = local_state(instance, barcode)
    slots = [e for e in entries if e[0] == 'Slot' and e[2] == barcode]
    free = sum(e[0] == 'Slot' and not e[2] for e in entries)
    if any(e[2] == barcode for e in entries) and not collision:
        raise RuntimeError('Inconsistent mhVTL inventory: media missing but slot occupied.')
    available = shutil.disk_usage(instance.media).free
    download = Path(request.get('download_root', str(instance.restores)))
    while not download.exists() and download != download.parent:
        download = download.parent
    download_available = shutil.disk_usage(download).free
    same_volume = download.stat().st_dev == instance.media.stat().st_dev
    margin = 16 * 1024 * 1024
    installation_required = required * (2 if same_volume and 'download_root' in request else 1) + margin
    download_required = required + margin if 'download_root' in request else 0
    space_ok = available >= installation_required and download_available >= download_required
    reason = None
    from cartridge_batches import pending_batch
    if instance.pending.exists() or device.pending.exists() or (instance.root/'cartridge-deletion-pending.json').exists() or pending_batch(instance.root):
        reason = 'Complete the interrupted local recovery before a new installation.'
    elif collision and request.get('replace') is not True:
        reason = 'A local cartridge has the same barcode: confirm the replacement separately, with verified retention of the previous copy.'
    elif collision and (row is None or deleted is not None or not (instance.media/barcode).is_dir() or len(slots) != 1):
        reason = 'Inconsistent local copy; automatic replacement refused.'
    elif not collision and not free:
        reason = 'No free slots in the target library.'
    elif download_available < download_required:
        reason = 'Insufficient space in the temporary download directory.'
    elif not space_ok:
        reason = 'Insufficient space on the target library storage.'
    return {'can_restore': reason is None, 'conflict': collision,
            'local_identity': 'conflict' if collision else 'absent', 'space_ok': space_ok,
            'free_slots': free, 'required_bytes': required, 'available_bytes': available, 'reason': reason,
            'installation_required_bytes': installation_required,
            'download_required_bytes': download_required,
            'download_available_bytes': download_available if 'download_root' in request else None,
            'shared_volume': same_volume if 'download_root' in request else False}


def settings(module, source, barcode):
    mam = (source/'mam').read_bytes()
    if len(mam) < 8 or struct.unpack('=II', mam[:8]) != (6, 4):
        raise RuntimeError('Unsupported MAM version; nothing was installed.')
    attrs = module.attributes(mam[8:])
    capacity = int.from_bytes(attrs.get(1, b''), 'big')
    density = {b'\x5c':'LTO7', b'\x5e':'LTO8'}.get(attrs.get(0x405))
    if not capacity or capacity > 2**63-1 or not density or attrs.get(0x806, b'').rstrip(b'\0 ') != barcode.encode():
        raise RuntimeError('Invalid MAM identity or capacity for the cartridge.')
    return {'capacity_bytes': capacity, 'media_type': density}


def begin(instance, request, inventory, directory, atomic):
    job_id = request.get('job_id', '')
    recovery_id = request.get('recovery_job_id', '')
    if not all(isinstance(v,str) and re.fullmatch('[0-9a-f-]{36}',v) for v in (job_id,recovery_id)):
        raise RuntimeError('Invalid recovery identifier.')
    with instance.db() as db:
        instance.no_other_jobs(db, job_id)
        job = instance.job(db, job_id)
        row = db.execute('SELECT * FROM recovery_requests WHERE job_id=? AND target_job_id=?', (recovery_id,job_id)).fetchone()
        if row is None or row['target_vtl_id'] != (instance.vtl_id or '00000000-0000-4000-8000-000000000001') or row['phase'] != 'installing':
            raise RuntimeError('Recovery missing or belongs to another library.')
        selected = json.loads(row['request'])
        receipt = json.loads(row['receipt'] or '{}')
        source_job = db.execute('SELECT * FROM jobs WHERE id=? AND vtl_id=?', (recovery_id,row['source_vtl_id'])).fetchone()
        if (source_job is None or source_job['kind'] != 'recovery' or source_job['status'] != 'running'
                or source_job['destination_id'] != selected['destination_id'] or source_job['configuration_revision'] is None
                or source_job['subject'] != selected['barcode'] or source_job['generation'] != selected['generation']
                or job['kind'] not in ('recovery','recovery_install') or job['status'] not in ('queued','running')
                or job['subject'] != selected['barcode'] or job['generation'] != selected['generation']):
            raise RuntimeError('Recovery destination or generation not pinned.')
        barcode = selected['barcode']
        if (receipt.get('status') != 'restored' or receipt.get('barcode') != barcode
                or receipt.get('generation') != selected['generation'] or receipt.get('manifest_sha256') != selected['manifest_sha256']
                or not receipt.get('files')):
            raise RuntimeError('Integrity receipt missing or belongs to another remote copy.')
        source = Path(source_job['destination'])
        source_root = instance.restores
        if instance.vtl_id:
            source_root = instance.database.parent/'restores'/row['source_vtl_id']
        directory(source_root)
        if source.parent != source_root or inventory(source) != sorted(receipt['files'],key=lambda f:f['path']):
            raise RuntimeError('The restored folder has changed or belongs to another VTL.')
        if instance.media == source_root or instance.media in source_root.parents or source_root in instance.media.parents:
            raise RuntimeError('Media and restore roots are not separate.')
        original = instance.query(db,'SELECT * FROM cartridges WHERE barcode=?',(barcode,)).fetchone()
        generations = [dict(r) for r in instance.query(db,'SELECT * FROM remote_generations WHERE barcode=?',(barcode,))]
        deleted = instance.query(db,'SELECT * FROM cartridge_deletions WHERE barcode=?',(barcode,)).fetchone()
    required = sum(f['size_bytes'] for f in receipt['files'])
    preflight = check(instance,dict(selected,required_bytes=required))
    if not preflight['can_restore']:
        raise RuntimeError(preflight['reason'])
    module, device = cartridges(instance)
    media_settings = settings(module,source,barcode)
    new_media = not preflight['conflict']
    if not new_media:
        directory(instance.media/barcode)
        inventory(instance.media/barcode)
    journal = {'vtl_id':instance.vtl_id,'id':job_id,'barcode':barcode,'generation':selected['generation'],
               'recovery_job_id':recovery_id,'destination_id':selected['destination_id'],'source_vtl_id':row['source_vtl_id'],
               'source':str(source),'actor':request['actor'],'created_at':int(time.time()),'order':time.time_ns(),
               'original':dict(original) if original else None,'original_deleted':dict(deleted) if deleted else None,
               'generations':generations,'receipt':receipt,'phase':'preparing','new_media':new_media,
               'layout_frozen':False,'media_settings':media_settings}
    instance.save(journal)
    atomic(instance.pending,{'id':job_id})
    return journal


def freeze_layout(instance, journal):
    """Snapshot only after the PBS lease and stopped services prevent movement."""
    if not journal.get('new_media'):
        return
    instance.controller.check()
    instance.controller.stopped()
    _, device = cartridges(instance)
    _, overlaid, entries = device.layout()
    if any(e[2] == journal['barcode'] for e in entries):
        raise RuntimeError('A slot with the same barcode appeared before maintenance.')
    free = next((e for e in entries if e[0] == 'Slot' and not e[2]), None)
    if free is None:
        raise RuntimeError('No free slot after the library stopped.')
    lines = overlaid.splitlines(keepends=True)
    lines[free[3]] = f"Slot {free[1]}: {journal['barcode']}\n"
    journal.update(layout_before=device.contents.read_text(), layout_after=''.join(lines), layout_frozen=True)
    instance.save(journal)


def layout(instance, journal, rollback=False, prepare_only=False):
    if not journal.get('new_media') or not journal.get('layout_frozen'):
        return
    module, device = cartridges(instance)
    from cartridge_deletion import replace_text
    before, after = journal['layout_before'], journal['layout_after']
    if rollback:
        # A completed installation may have been followed by ordinary PBS tape
        # moves. Under the renewed maintenance lease, remove only this tape and
        # retain unrelated slot changes. Persist the removal for crash retries.
        if not journal.get('rollback_layout_frozen'):
            instance.controller.check()
            instance.controller.stopped()
            _, overlaid, entries = device.layout()
            positions = [e for e in entries if e[2] == journal['barcode']]
            if len(positions) > 1 or any(e[0] != 'Slot' for e in positions):
                raise RuntimeError('Slot of the cartridge to remove is missing or ambiguous.')
            lines = overlaid.splitlines(keepends=True)
            for entry in positions:
                lines[entry[3]] = f'Slot {entry[1]}:\n'
            journal.update(rollback_layout_before=device.contents.read_text(),
                           rollback_layout_after=''.join(lines), rollback_layout_frozen=True)
            instance.save(journal)
        before, after = journal['rollback_layout_before'], journal['rollback_layout_after']
    if not prepare_only:
        replace_text(device.contents,before,after,module.safe_path,module.sync_dir)


def catalogue(instance, db, journal, required):
    barcode = journal['barcode']
    vtl = instance.vtl_id or '00000000-0000-4000-8000-000000000001'
    if journal.get('new_media') and journal['original'] is None:
        value = journal['media_settings']
        db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,media_type,generation,bytes_written,state) VALUES(?,?,?,?,?,?,'verified')",
                   (vtl,barcode,value['capacity_bytes'],value['media_type'],journal['generation'],required))
    else:
        instance.query(db,"UPDATE cartridges SET generation=?,bytes_written=?,state='verified',updated_at=CURRENT_TIMESTAMP WHERE barcode=?",(journal['generation'],required,barcode))
        if journal.get('media_settings'):
            value = journal['media_settings']
            instance.query(db,'UPDATE cartridges SET capacity_bytes=?,media_type=? WHERE barcode=?',(value['capacity_bytes'],value['media_type'],barcode))
    if journal.get('new_media'):
        instance.query(db,'DELETE FROM cartridge_deletions WHERE barcode=?',(barcode,))


def restore_settings(instance, db, journal):
    old = journal.get('original')
    if old and 'capacity_bytes' in old and 'media_type' in old:
        instance.query(db,'UPDATE cartridges SET capacity_bytes=?,media_type=? WHERE barcode=?',(old['capacity_bytes'],old['media_type'],journal['barcode']))
    deleted = journal.get('original_deleted')
    if deleted:
        instance.query(db,'UPDATE cartridge_deletions SET request_id=? WHERE barcode=?',(deleted['request_id'],journal['barcode']))
        if 'deleted_at' in deleted:
            instance.query(db,'UPDATE cartridge_deletions SET deleted_at=? WHERE barcode=?',(deleted['deleted_at'],journal['barcode']))
