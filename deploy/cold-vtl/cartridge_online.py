"""Bounded, correlated mhVTL insertion commands; never commands a tape drive."""
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import uuid

OP = re.compile(r'[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}\Z')
BARCODE = re.compile(r'[A-Z0-9-]{6,16}\Z')


def command(instance, action, operation_id, barcode=None):
    if not OP.fullmatch(operation_id) or action not in ('status','insert'):
        raise RuntimeError('Invalid creation command.')
    args = ['vtlcmd',str(instance.library_id),'vaultcenter-media',action,operation_id]
    if barcode is not None:
        if not BARCODE.fullmatch(barcode):
            raise RuntimeError('Invalid barcode.')
        args.append(barcode)
    env = dict(os.environ, MHVTL_CONFIG_PATH=str(instance.config_dir))
    # Runtime IDs are globally allocated. Do not inherit a test queue override.
    env.pop('MHVTL_QKEY',None)
    try:
        result = subprocess.run(args,env=env,capture_output=True,text=True,timeout=7,check=False,
                                pass_fds=(instance.lock_fd,) if instance.lock_fd is not None else ())
        if result.returncode or len(result.stdout)>4096:
            raise ValueError('unconfirmed')
        value = json.loads(result.stdout)
        if (not isinstance(value,dict) or type(value.get('version')) is not int or value['version']!=1
                or type(value.get('library_id')) is not int or value['library_id']!=instance.library_id
                or value.get('operation_id')!=operation_id):
            raise ValueError('foreign response')
    except (OSError,subprocess.TimeoutExpired,ValueError) as error:
        raise RuntimeError('Online insertion not confirmed by the engine. Check its version and status; retry with the same settings after an error.') from error
    if value.get('error'):
        raise RuntimeError('Online insertion refused by the library: '+str(value['error']))
    if action=='status':
        if type(value.get('free_slots')) is not int or not 0<=value['free_slots']<=256:
            raise RuntimeError('Invalid number of online slots.')
    elif value.get('barcode')!=barcode or value.get('result') not in ('inserted','already_inserted'):
        raise RuntimeError('Insertion not confirmed for this cartridge.')
    return value


def status(instance):
    return command(instance,'status',str(uuid.uuid4()))


def insert(instance, operation_id, barcode):
    return command(instance,'insert',operation_id,barcode)


def receipts(instance):
    """Offline inventory overlay, identical to daemon replay; never edits config."""
    path=instance.media/f'.vaultcenter-media-{instance.library_id}'
    try:
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    except FileNotFoundError:
        return []
    try:
        info=os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink!=1 or info.st_size>4096*56+64:
            raise RuntimeError('Invalid online insertion journal.')
        with os.fdopen(fd,'r',encoding='ascii',closefd=False) as stream:
            lines=stream.readlines()
    finally:
        os.close(fd)
    if not lines or lines[0]!=f'VAULTCENTER_MEDIA1 {instance.library_id}\n':
        raise RuntimeError('Insertion journal of another library or incomplete.')
    operations=set();barcodes=[]
    for line in lines[1:]:
        fields=line.split()
        if (not line.endswith('\n') or len(fields)!=2 or not OP.fullmatch(fields[0])
                or not BARCODE.fullmatch(fields[1]) or fields[0] in operations or fields[1] in barcodes
                or len(barcodes)>=4096):
            raise RuntimeError('Invalid online insertion journal.')
        operations.add(fields[0]);barcodes.append(fields[1])
    return barcodes


def overlay(instance, content, entries):
    lines=content.splitlines(keepends=True)
    entries=list(entries)
    used={e[2] for e in entries if e[2]}
    for barcode in receipts(instance):
        if barcode in used:
            continue
        index=next((i for i,e in enumerate(entries) if e[0]=='Slot' and not e[2]),None)
        if index is None:
            raise RuntimeError('Incomplete inventory: no slot for a confirmed insertion.')
        kind,number,_,line=entries[index]
        entries[index]=(kind,number,barcode,line)
        lines[line]=f'Slot {number}: {barcode}\n'
        used.add(barcode)
    return ''.join(lines),entries
