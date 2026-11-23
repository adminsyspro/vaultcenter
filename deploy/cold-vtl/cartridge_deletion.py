"""Local media removal. Never contacts S3 or changes the PBS media catalogue.

The caller holds the VTL operation lock. A durable pending record keeps service
resumption blocked until inventories, tombstones and filesystem cleanup agree.
"""
import json
import os
import re
import stat

PENDING = 'cartridge-deletion-pending.json'


def pending(root):
    path = root / PENDING
    if path.is_symlink():
        raise RuntimeError('Deletion journal is a symbolic link: recovery required.')
    return json.loads(path.read_text()) if path.exists() else None


def validate(request):
    if (set(request) != {'vtl_id', 'request_id', 'barcodes', 'confirmation'}
            or request['confirmation'] != 'delete-local'
            or not isinstance(request['request_id'], str)
            or not re.fullmatch('[a-f0-9]{32}', request['request_id'])
            or not isinstance(request['barcodes'], list)
            or not 1 <= len(request['barcodes']) <= 64
            or any(not isinstance(b, str) or not re.fullmatch('[A-Z0-9-]{6,16}', b)
                   for b in request['barcodes'])
            or len(set(request['barcodes'])) != len(request['barcodes'])):
        raise RuntimeError('Confirm the local deletion of 1 to 64 distinct barcodes.')


def ready(instance):
    instance.controller.check()
    instance.controller.stopped()
    instance.controller.no_storage()
    instance.controller.no_workers()
    from cartridge_batches import pending_batch
    if (instance.pending.exists() or pending_batch(instance.root)
            or (instance.root / 'reintegration-pending.json').exists()):
        raise RuntimeError('Complete interrupted creation or reintegration before deleting.')
    with instance.db() as db:
        if db.execute("SELECT 1 FROM jobs WHERE status IN ('queued','running')"
                      + (' AND vtl_id=?' if instance.scoped else ''),
                      (instance.vtl_id,) if instance.scoped else ()).fetchone():
            raise RuntimeError('Wait for all VTL jobs to finish.')


def options(instance, request):
    result = {'ready': False, 'reason': None, 'pending': None}
    try:
        instance.installed(request)
        value = pending(instance.root)
        if value:
            validate(value['request'])
            if value['request']['vtl_id'] != instance.vtl_id:
                raise RuntimeError('Deletion journal of another VTL.')
            result['pending'] = value['request']
        ready(instance)
        result['ready'] = True
    except Exception as error:
        result['reason'] = str(error)
    return result


def identity(path):
    info = path.stat()
    return [info.st_dev, info.st_ino]


def files(path, safe):
    safe(path, True)
    result = {}
    for file in path.iterdir():
        safe(file)
        info = file.stat()
        result[file.name] = [info.st_dev, info.st_ino, info.st_size]
    return result


def replace_text(path, before, after, safe, sync):
    safe(path)
    current = path.read_text()
    if current == after:
        return
    if current != before:
        raise RuntimeError('Inventory changed since the request: resume refused.')
    info = path.stat()
    temporary = path.with_name('.' + path.name + '.delete.tmp')
    # Rebuild an interrupted temporary, but never follow a substituted link.
    if temporary.exists() or temporary.is_symlink():
        safe(temporary)
        temporary.unlink()
    with temporary.open('x') as stream:
        stream.write(after)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(temporary, stat.S_IMODE(info.st_mode))
    os.chown(temporary, info.st_uid, info.st_gid)
    temporary.replace(path)
    sync(path.parent)


def delete(instance, request, safe, rename, sync, atomic):
    validate(request)
    instance.installed(request)
    receipt_dir = instance.root / 'cartridge-deletions'
    receipt_dir.mkdir(exist_ok=True, mode=0o700)
    safe(receipt_dir, True)
    receipt = receipt_dir / (request['request_id'] + '.json')
    journal = pending(instance.root)
    if journal and journal['request'] != request:
        raise RuntimeError('Resume the interrupted deletion with its original selection.')
    if receipt.exists() or receipt.is_symlink():
        safe(receipt)
        result = json.loads(receipt.read_text())
        if result['request'] != request:
            raise RuntimeError('This identifier belongs to another deletion.')
        if not journal:
            return result
    ready(instance)
    pending_path = instance.root / PENDING
    ledger = instance.media / f'.vaultcenter-media-{instance.library_id}'
    stage = instance.media / ('.cold-vtl-deletion-' + request['request_id'])
    if journal is None:
        _, overlaid, entries = instance.layout()  # validates the online receipt ledger too
        if any(e[0] in ('Drive', 'Picker', 'MAP') and e[2] for e in entries):
            raise RuntimeError('Unload the drives and empty the changer before deletion.')
        if stage.exists() or stage.is_symlink():
            raise RuntimeError('Deletion area already present without a journal.')
        barcodes = set(request['barcodes'])
        before = instance.contents.read_text()
        after = ''.join(re.sub(r'^(Slot\s+\d+:)\s*[A-Z0-9-]+\s*$', r'\1\n', line)
                        if any(e[2] in barcodes and e[3] == i for e in entries) else line
                        for i, line in enumerate(overlaid.splitlines(keepends=True)))
        snapshots = {}
        with instance.db() as db:
            for barcode in request['barcodes']:
                if not db.execute('SELECT 1 FROM cartridges WHERE vtl_id=? AND barcode=?',
                                  (instance.vtl_id, barcode)).fetchone():
                    raise RuntimeError('Cartridge missing from the catalogue: ' + barcode)
                if db.execute('SELECT 1 FROM cartridge_deletions WHERE vtl_id=? AND barcode=?',
                              (instance.vtl_id, barcode)).fetchone():
                    raise RuntimeError('Cartridge already deleted locally: ' + barcode)
                path = instance.media / barcode
                snapshot = files(path, safe)
                if 'mam' not in snapshot:
                    raise RuntimeError('Cartridge metadata missing: ' + barcode)
                snapshots[barcode] = {'identity': identity(path), 'files': snapshot}
        ledger_before = ledger.read_text() if ledger.exists() else None
        ledger_after = (''.join(line for i, line in enumerate(ledger_before.splitlines(keepends=True))
                               if i == 0 or line.split()[1] not in barcodes)
                        if ledger_before is not None else None)
        journal = {'request': request, 'phase': 'prepared', 'media_identity': identity(instance.media),
                   'before': before, 'after': after, 'ledger_before': ledger_before,
                   'ledger_after': ledger_after, 'tapes': snapshots}
        atomic(pending_path, journal)
    if journal['media_identity'] != identity(instance.media):
        raise RuntimeError('The local volume no longer matches the original deletion.')
    if journal['phase'] not in ('prepared', 'catalogued') or set(journal['tapes']) != set(request['barcodes']):
        raise RuntimeError('Incomplete or inconsistent deletion journal.')
    if journal['phase'] == 'catalogued':
        # Never restart into an inventory that has been edited during recovery.
        replace_text(instance.contents, journal['after'], journal['after'], safe, sync)
        if journal['ledger_after'] is not None:
            replace_text(ledger, journal['ledger_after'], journal['ledger_after'], safe, sync)
        elif ledger.exists() or ledger.is_symlink():
            raise RuntimeError('Online insertion journal appeared during deletion.')
    stage.mkdir(exist_ok=True, mode=0o700)
    safe(stage, True)
    if stage.stat().st_dev != instance.media.stat().st_dev:
        raise RuntimeError('The deletion area must stay on the same volume.')
    sync(instance.media)
    if journal['phase'] == 'prepared':
        for barcode, snapshot in journal['tapes'].items():
            source, target = instance.media / barcode, stage / barcode
            location = source if source.exists() or source.is_symlink() else target
            if files(location, safe) != snapshot['files'] or identity(location) != snapshot['identity']:
                raise RuntimeError('Files changed since the request: ' + barcode)
            if location == source:
                rename(source, target)
        replace_text(instance.contents, journal['before'], journal['after'], safe, sync)
        if journal['ledger_before'] is not None:
            replace_text(ledger, journal['ledger_before'], journal['ledger_after'], safe, sync)
        elif ledger.exists() or ledger.is_symlink():
            raise RuntimeError('An online insertion journal appeared during deletion.')
        with instance.db() as db:
            for barcode in request['barcodes']:
                db.execute('INSERT INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,?,?) '
                           'ON CONFLICT(vtl_id,barcode) DO NOTHING',
                           (instance.vtl_id, barcode, request['request_id']))
        journal['phase'] = 'catalogued'
        atomic(pending_path, journal)
    # A crash between unlinks is safe: only the originally recorded plain files
    # may be removed; added/replaced files or directories block recovery.
    for barcode, snapshot in journal['tapes'].items():
        if (instance.media / barcode).exists() or (instance.media / barcode).is_symlink():
            raise RuntimeError('A local media reappeared: ' + barcode)
        target = stage / barcode
        if not target.exists() and not target.is_symlink():
            continue
        remaining = files(target, safe)
        if identity(target) != snapshot['identity'] or any(snapshot['files'].get(k) != v for k, v in remaining.items()):
            raise RuntimeError('Deletion area modified: ' + barcode)
        for name in remaining:
            (target / name).unlink()
        sync(target)
        target.rmdir()
        sync(stage)
    stage.rmdir()
    sync(instance.media)
    result = {'request': request, 'phase': 'completed'}
    atomic(receipt, result)
    pending_path.unlink()
    sync(instance.root)
    return result
