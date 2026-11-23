"""Root-owned resource manifests for independently operated tape libraries."""
from contextlib import contextmanager, closing
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import tempfile
import time
import uuid

ROOT=Path('/var/lib/cold-vtl')
SETTINGS=Path('/etc/cold-vtl/vtl-runtime.json')
# mhVTL waits for its idle poll interval to exceed 0xf000 before exiting.
# With a 10 us increment, that takes about 190 s after the last command.
DRIVE_BACKOFF=10
DRIVE_STOP_SECONDS=240
DEFAULT_IQN_PREFIX='iqn.2026-10.io.vaultcenter'
DEFAULT_SCSI_PRODUCT='VAULTCENTER'
# FZ: VTL provisioned before the rename; clients are bound to these serials.
SERIAL_PREFIXES=('FZ','VC')
PBS_DRIVE_IDENTITY={'vendor':'IBM','product':'ULT3580-TD8','revision':'HB81'}
IQN=re.compile(r'(iqn\.[0-9]{4}-(?:0[1-9]|1[0-2])\.[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?)'
               r':vtl-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})')
# Same SCSI identity as the StarWind VTL previously used by Veeam. The drive
# vendor must be HPE for the Windows HPE StoreEver LTO-8 driver (mhvtl #146).
VEEAM_IDENTITY={'library':{'vendor':'HP','product':'MSL G3 Series','revision':'9.50'},
                'drive':{'vendor':'HPE','product':'Ultrium 8-SCSI','revision':'HB81'}}

# Veeam exports a whole media set to I/E ports at the end of a job and needs a
# real port per tape (two for the current full); PBS reserves ordinary slots as
# export slots on its side.
VEEAM_IE_PORTS=2


def client(m):return m.get('client','pbs')


def valid_iqn(iqn,vtl_id=None):
    match=IQN.fullmatch(iqn) if isinstance(iqn,str) else None
    return bool(match) and len(iqn.encode())<=223 and vtl_id in (None,match[2])


def client_identity(settings):
    """iSCSI prefix and SCSI product of new VTLs; existing VTLs keep theirs."""
    prefix=settings.get('iqn_prefix',DEFAULT_IQN_PREFIX)
    product=settings.get('scsi_product',DEFAULT_SCSI_PRODUCT)
    if not isinstance(prefix,str) or not valid_iqn(prefix+':vtl-'+str(uuid.UUID(int=0))):
        raise ValueError('Invalid IQN prefix in vtl-runtime.json.')
    if not isinstance(product,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,16}',product):
        raise ValueError('Invalid SCSI product in vtl-runtime.json.')
    return prefix,product


def library_contents(ports):
    return 'VERSION: 2\n\nDrive 1:\n\nPicker 1:\n\n'+''.join(f'MAP {i}:\n' for i in range(1,ports+1))+'\n'+''.join(f'Slot {i}:\n' for i in range(1,65))


def set_ie_ports(text,ports):
    """Adjust the `MAP n:` entries to `ports`; only empty trailing ports are removed. Slots are kept as they are."""
    lines=text.splitlines(keepends=True)
    maps=[i for i,line in enumerate(lines) if re.fullmatch(r'MAP\s+\d+:.*\n?',line)]
    if not maps or len(maps)==ports:return text
    numbers=[int(re.match(r'MAP\s+(\d+)',lines[i])[1]) for i in maps]
    if numbers!=list(range(1,len(numbers)+1)):raise ValueError('Library import/export ports are not contiguous.')
    if any(re.fullmatch(r'MAP\s+\d+:\s*\S.*\n?',lines[i]) for i in maps[ports:]):
        raise ValueError('An import/export port to be removed contains a cartridge.')
    if not lines[maps[-1]].endswith('\n'):lines[maps[-1]]+='\n'
    added=[f'MAP {i}:\n' for i in range(len(maps)+1,ports+1)]
    removed=set(maps[ports:])
    end=maps[-1]+1
    return ''.join([l for i,l in enumerate(lines[:end]) if i not in removed]+added+lines[end:])


def identifier(value):
    try:
        if not isinstance(value,str) or str(uuid.UUID(value))!=value: raise ValueError()
    except (ValueError,AttributeError): raise ValueError('Invalid VTL identifier.') from None
    return value


def atomic(path,value):
    path=Path(path);path.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    fd,name=tempfile.mkstemp(prefix='.'+path.name,dir=path.parent)
    try:
        with os.fdopen(fd,'w') as stream:json.dump(value,stream);stream.flush();os.fsync(stream.fileno())
        os.replace(name,path)
        fd=os.open(path.parent,os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    finally:
        if os.path.exists(name):os.unlink(name)


def atomic_text(path,value):
    path=Path(path);path.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    fd,name=tempfile.mkstemp(prefix='.'+path.name,dir=path.parent)
    try:
        with os.fdopen(fd,'w') as stream:stream.write(value);stream.flush();os.fsync(stream.fileno())
        os.replace(name,path)
        fd=os.open(path.parent,os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    finally:
        if os.path.exists(name):os.unlink(name)


@contextmanager
def locked(path):
    path=Path(path);path.parent.mkdir(parents=True,mode=0o700,exist_ok=True)
    with path.open('a+') as stream:
        try:fcntl.flock(stream,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:raise ValueError('An operation is already in progress for this resource.') from None
        try:yield
        finally:fcntl.flock(stream,fcntl.LOCK_UN)


def command_timeout(args):
    if (len(args)==3 and args[:2]==['systemctl','stop']
        and re.fullmatch(r'vaultcenter-drive-[0-9a-f-]{36}-[0-9]+\.service',args[2])):
        return DRIVE_STOP_SECONDS+15
    return 40


def run(args):
    result=subprocess.run(args,capture_output=True,text=True,timeout=command_timeout(args))
    if result.returncode:
        try:detail=json.loads(result.stdout).get('error')
        except (ValueError,AttributeError):detail=None
        raise ValueError('VTL command refused: '+(detail if isinstance(detail,str) else result.stderr.strip())[-400:])
    return result.stdout


def digest(value):return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()


def storage_request(vtl_id,action='status'):
    payload={'action':action,'vtl_id':identifier(vtl_id)}
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10);connection.connect(os.environ.get('COLD_VTL_STORAGE_SOCKET','/run/vaultcenter-host/storage.sock'))
        connection.sendall((json.dumps(payload)+'\n').encode())
        with connection.makefile('rb') as stream:raw=stream.readline(65537)
    if len(raw)>65536 or not raw.endswith(b'\n'):raise ValueError('Invalid host response.')
    value=json.loads(raw)
    if value.get('ok') is not True:raise ValueError(value.get('error',{}).get('message','Storage unreachable.'))
    result=value['result']
    if result.get('vtl_id')!=vtl_id:raise ValueError('Response belongs to another VTL.')
    return result


def resolve_runtime(vtl_id,expected_revision=None,root=ROOT):
    vtl_id=identifier(vtl_id);path=Path(root)/'vtls'/vtl_id/'runtime.json'
    info=path.lstat()
    if path.is_symlink() or info.st_uid!=os.geteuid() or info.st_mode&0o022:
        raise ValueError('Hardware manifest is not protected.')
    value=json.loads(path.read_text())
    if (value.get('vtl_id')!=vtl_id or value.get('state_root')!=str(Path(root)/'vtls'/vtl_id)
        or value.get('media_root')!='/var/lib/vaultcenter-media/'+vtl_id
        or value.get('config_root')!=str(Path(root)/'vtls'/vtl_id/'config')
        or value.get('restore_root')!=str(Path(root)/'restores'/vtl_id)):
        raise ValueError('Invalid VTL manifest roots.')
    if expected_revision is not None and value['revision']!=expected_revision:
        raise ValueError('The hardware configuration has changed.')
    with closing(sqlite3.connect(Path(root)/'catalog.db')) as db:
        row=db.execute('SELECT manifest_sha256,phase FROM vtl_provisioning WHERE vtl_id=?',(vtl_id,)).fetchone()
        if not row or row[0]!=digest(value) or row[1] not in ('configured','active'):
            raise ValueError('Hardware manifest not confirmed by the catalogue.')
    return value


class Registry:
    def __init__(self,root=ROOT,settings=None,runner=run):
        self.root=Path(root);self.settings=settings if settings is not None else json.loads(SETTINGS.read_text());self.run=runner

    @contextmanager
    def db(self):
        with closing(sqlite3.connect(self.root/'catalog.db',timeout=5)) as db:
            db.row_factory=sqlite3.Row
            with db:yield db

    def storage(self,vtl_id):return storage_request(vtl_id)

    def devices(self, addresses=None, identify=True):
        devices=[]
        for sg in Path('/sys/class/scsi_generic').glob('sg*'):
            device='/dev/'+sg.name
            address=(sg/'device').resolve().name
            if addresses is not None and tuple(address.split(':')[1:]) not in addresses:continue
            if not identify:
                devices.append({'path':device,'address':address});continue
            try:
                serial=self.run(['sg_inq','--page=0x80',device])
                naa=self.run(['sg_inq','--page=0x83',device])
                serial_match=re.search(r'Unit serial number:\s*(\S+)',serial)
                naa_match=re.search(r'(?:0x|\[)([0-9a-fA-F]{16})\]?',naa)
                # A missing NAA is kept as None; only a manifest that declares it
                # unverifiable (Veeam HP MSL changer) can match such a device.
                if serial_match:
                    devices.append({'path':device,'serial':serial_match[1],'naa':naa_match[1].lower() if naa_match else None,
                                    'address':(sg/'device').resolve().name})
            except (ValueError,OSError):continue
        return devices

    def targets(self):
        text=self.run(['tgtadm','--lld','iscsi','--mode','target','--op','show'])
        return {int(n):iqn for n,iqn in re.findall(r'^Target (\d+): (\S+)$',text,re.M)}

    def manifest(self,vtl_id):
        identifier(vtl_id)
        with self.db() as db:row=db.execute('SELECT manifest,manifest_sha256 FROM vtl_provisioning WHERE vtl_id=?',(vtl_id,)).fetchone()
        if not row:raise ValueError('This VTL has no prepared manifest.')
        value=json.loads(row['manifest'])
        if digest(value)!=row['manifest_sha256'] or value.get('vtl_id')!=vtl_id:raise ValueError('Provisioning manifest modified.')
        return value

    def validate_pool(self):
        used=set()
        required={'library_id','drive_id','channel','library_target','drive_target','target_id'}
        for item in self.settings['resources']:
            if set(item)!=required or any(type(x) is not int or x<0 or x>65535 for x in item.values()):
                raise ValueError('Invalid hardware pool.')
            keys=[('queue',item['library_id']),('queue',item['drive_id']),('target',item['target_id']),
                  ('scsi',item['channel'],item['library_target']),('scsi',item['channel'],item['drive_target'])]
            if len(set(keys))!=len(keys) or any(k in used for k in keys):raise ValueError('Collision in the hardware pool.')
            used.update(keys)

    def prepare(self,vtl_id,expected_revision):
        identifier(vtl_id);self.validate_pool()
        iqn_prefix,scsi_product=client_identity(self.settings)
        if type(expected_revision) is not int:raise ValueError('Invalid revision.')
        if self.storage(vtl_id).get('phase')!='ready':raise ValueError('Configure storage before provisioning.')
        from client_binding import effective
        binding=effective(self,vtl_id)
        if not binding:raise ValueError('Backup client binding missing for this VTL.')
        ipaddress.ip_address(binding['initiator'])
        with locked(self.root/'vtl-registry.lock'),self.db() as db:
            row=db.execute('SELECT * FROM vtls WHERE id=?',(vtl_id,)).fetchone()
            if not row or row['provisioning_revision']!=expected_revision:raise ValueError('Unknown VTL or outdated revision.')
            if row['lifecycle']=='ready':raise ValueError('The active hardware configuration requires a dedicated migration.')
            records=db.execute('SELECT vtl_id,manifest,phase FROM vtl_provisioning').fetchall()
            if any(r['vtl_id']==vtl_id and r['phase'] not in ('prepared','failed') for r in records):
                raise ValueError('Finish the existing provisioning before changing it.')
            used={json.loads(r['manifest'])['library']['id'] for r in records if r['vtl_id']!=vtl_id}
            targets=self.targets();devices=self.devices(identify=False)
            def free(item):
                addresses={(str(item['channel']),str(item[k])) for k in ('library_target','drive_target')}
                return item['library_id'] not in used and item['target_id'] not in targets and not any(tuple(d['address'].split(':')[1:3]) in addresses for d in devices)
            resource=next((r for r in self.settings['resources'] if free(r)),None)
            if resource is None:raise ValueError('No free, verified hardware resource.')
            root=self.root/'vtls'/vtl_id
            veeam=binding.get('client')=='veeam'
            def device(kind,number,target):
                hash_=hashlib.sha256((vtl_id+kind).encode()).hexdigest()
                # mhVTL's HP MSL personality reports at most 10 serial characters and
                # truncates the changer's NAA designator (VPD 0x83).
                value={'id':number,'serial':'VC'+hash_[:8 if veeam else 12].upper(),'naa':'50'+hash_[:14],
                       'channel':resource['channel'],'target':target,'lun':0}
                if veeam and kind=='library':value['naa_unverifiable']=True
                return value
            value={'vtl_id':vtl_id,'revision':expected_revision+1,'state_root':str(root),
                'media_root':'/var/lib/vaultcenter-media/'+vtl_id,'restore_root':str(self.root/'restores'/vtl_id),
                'config_root':str(root/'config'),'library':device('library',resource['library_id'],resource['library_target']),
                'drives':[device('drive',resource['drive_id'],resource['drive_target'])],
                'target':{'id':resource['target_id'],'iqn':iqn_prefix+':vtl-'+vtl_id,'initiator':binding['initiator']}}
            if binding.get('client')=='veeam':value.update(client='veeam',identity=VEEAM_IDENTITY)
            else:value.update(pbs=binding,identity={'library':{'vendor':'MHVTL','product':scsi_product,'revision':None},'drive':dict(PBS_DRIVE_IDENTITY)})
            identities={d[k] for d in [value['library'],*value['drives']] for k in ('serial','naa')}
            for old in records:
                if old['vtl_id']==vtl_id:continue
                previous=json.loads(old['manifest'])
                if identities & {d[k] for d in [previous['library'],*previous['drives']] for k in ('serial','naa')}:
                    raise ValueError('SCSI identity collision.')
            token=uuid.uuid4().hex;sha=digest(value)
            journal={'token':token,'expires':time.time()+300,'expected_revision':expected_revision}
            db.execute("INSERT INTO vtl_provisioning(vtl_id,revision,manifest,manifest_sha256,phase,journal) VALUES(?,?,?,?,'prepared',?) ON CONFLICT(vtl_id) DO UPDATE SET revision=excluded.revision,manifest=excluded.manifest,manifest_sha256=excluded.manifest_sha256,phase=excluded.phase,journal=excluded.journal",
                       (vtl_id,value['revision'],json.dumps(value),sha,json.dumps(journal)))
            return {'vtl_id':vtl_id,'revision':value['revision'],'token':token,'manifest_sha256':sha}

    def matching_device(self,device,observed):
        def naa(d):
            if d['naa'] is None:return device.get('naa_unverifiable') is True
            return d['naa'].replace(':','').lower()==device['naa'].replace(':','').lower()
        return [d for d in observed if d['serial']==device['serial'] and naa(d)
                and d['address'].split(':')[1:]==[str(device['channel']),str(device['target']),str(device['lun'])]]

    def resolve_device(self,device):
        addresses={(str(device['channel']),str(device['target']),str(device['lun']))}
        matches=self.matching_device(device,self.devices(addresses=addresses))
        if len(matches)!=1:raise ValueError('SCSI identity missing, duplicated or replaced.')
        return matches[0]['path']

    def wait_devices(self,m):
        expected=[*m['drives'],m['library']]
        addresses={(str(d['channel']),str(d['target']),str(d['lun'])) for d in expected}
        deadline=time.monotonic()+10
        while True:
            observed=self.devices(addresses=addresses)
            matches=[self.matching_device(d,observed) for d in expected]
            if all(len(found)==1 for found in matches):return [found[0]['path'] for found in matches]
            if any(len(found)>1 for found in matches) or any(not any(self.matching_device(d,[row]) for d in expected) for row in observed):
                raise ValueError('An expected SCSI identity was replaced or duplicated.')
            if time.monotonic()>=deadline:raise ValueError('The SCSI devices of this VTL did not appear.')
            time.sleep(.2)

    def telemetry_unit(self,manifest):return "vaultcenter-telemetry-"+manifest["vtl_id"]+".service"

    def units(self,manifest):
        vtl=manifest['vtl_id']
        return [f'vaultcenter-library-{vtl}.service',*[f'vaultcenter-drive-{vtl}-{d["id"]}.service' for d in manifest['drives']],f'vaultcenter-target-{vtl}.service']

    def write_configuration(self,m):
        ident=m.get('identity')
        if not isinstance(ident,dict) or not {'library','drive'}<=set(ident):
            raise ValueError('SCSI identity missing from the manifest: migration required.')
        from bandwidth import initialize_bandwidth, require_parallel_host
        limits=initialize_bandwidth(self.root,m['vtl_id'])
        if limits['tape_write_bps'] is not None:require_parallel_host()
        if limits['tape_write_bps'] is not None and len(m['drives'])!=1:
            raise ValueError('The PBS limit requires a single drive per VTL.')
        root=Path(m['config_root']);root.mkdir(mode=0o700,parents=True,exist_ok=True)
        lib=m['library'];drive=m['drives'][0]
        naa=lambda d:':'.join(d['naa'][i:i+2] for i in range(0,16,2))
        lib_revision=f" Product revision level: {ident['library']['revision']}\n" if ident['library']['revision'] else ''
        text=f'''VERSION: 5

Library: {lib['id']} CHANNEL: {lib['channel']:02} TARGET: {lib['target']:02} LUN: 00
 Vendor identification: {ident['library']['vendor']}
 Product identification: {ident['library']['product']}
{lib_revision} Unit serial number: {lib['serial']}
 NAA: {naa(lib)}
 Home directory: {m['media_root']}
 PERSIST: False
 Backoff: 400

Drive: {drive['id']} CHANNEL: {drive['channel']:02} TARGET: {drive['target']:02} LUN: 00
 Library ID: {lib['id']} Slot: 01
 Vendor identification: {ident['drive']['vendor']}
 Product identification: {ident['drive']['product']}
 Product revision level: {ident['drive']['revision']}
 Unit serial number: {drive['serial']}
 NAA: {naa(drive)}
 Compression: factor 1 enabled 0
 Compression type: lzo
 Backoff: {DRIVE_BACKOFF}
'''
        atomic_text(root/'device.conf',text)
        atomic_text(root/'mhvtl.conf',f"MHVTL_HOME_PATH={m['media_root']}\nCAPACITY=16384\nVERBOSE=1\nVTL_DEBUG=0\n")
        contents=root/f"library_contents.{lib['id']}"
        ports=VEEAM_IE_PORTS if client(m)=='veeam' else 1
        if not contents.exists():atomic_text(contents,library_contents(ports))
        elif client(m)=='veeam':
            # Read only when vtllibrary starts (PERSIST is off), so this takes effect at its next start.
            text=contents.read_text();updated=set_ie_ports(text,ports)
            if updated!=text:atomic_text(contents,updated)
        Path(m['restore_root']).mkdir(mode=0o700,parents=True,exist_ok=True)
        atomic(Path(m['state_root'])/'runtime.json',m)
        units=Path(self.settings.get('unit_dir',str(self.root/'units')));units.mkdir(mode=0o700,parents=True,exist_ok=True)
        for unit,binary,device in zip(self.units(m)[:-1],['vtllibrary','vtltape'],[lib,drive]):
            content=f'''[Unit]
Description=VaultCenter VTL {m['vtl_id']} {binary}
After=network.target
ConditionPathExists=!{m['state_root']}/maintenance-block
ConditionPathExists=!{m['state_root']}/storage-block
ConditionPathExists=!{m['state_root']}/storage-services-held
[Service]
Type=simple
Environment=COLD_VTL_VTL_ID={m['vtl_id']}
Environment=VAULTCENTER_VTL_ID={m['vtl_id']}
Environment=VAULTCENTER_BANDWIDTH_CONFIG=/etc/mhvtl/bandwidth-drive.conf
ExecCondition=/usr/bin/python3 /usr/local/libexec/storage-check.py --service
RuntimeDirectory=vaultcenter-vtl-{m['vtl_id']}
RuntimeDirectoryMode=0700
RuntimeDirectoryPreserve=yes
ExecStart=/usr/bin/{binary} -F -q {device['id']} -f/run/vaultcenter-vtl-{m['vtl_id']}/status.fifo
ExecStartPost=/bin/sh -c 'umask 077; printf "%%s\\n" "$MAINPID" > /run/vaultcenter-vtl-{m['vtl_id']}/pid-{device['id']}'
ExecStopPost=-/bin/rm -f /run/vaultcenter-vtl-{m['vtl_id']}/pid-{device['id']}
ExecStop=/usr/bin/vtlcmd {device['id']} exit
BindReadOnlyPaths={m['config_root']}:/etc/mhvtl
KillMode=mixed
TimeoutStopSec={DRIVE_STOP_SECONDS if binary=='vtltape' else 30}
Restart=no
[Install]
WantedBy=multi-user.target
'''
            atomic_text(units/unit,content)
        target=self.units(m)[-1]
        atomic_text(units/target,f'''[Unit]
Description=VaultCenter iSCSI target {m['vtl_id']}
After=tgt.service {' '.join(self.units(m)[:-1])}
Requires=tgt.service
ConditionPathExists=!{m['state_root']}/maintenance-block
ConditionPathExists=!{m['state_root']}/storage-block
ConditionPathExists=!{m['state_root']}/storage-services-held
[Service]
Type=oneshot
RemainAfterExit=yes
Environment=COLD_VTL_VTL_ID={m['vtl_id']}
ExecCondition=/usr/bin/python3 /usr/local/libexec/storage-check.py --service
ExecStart=/usr/local/libexec/cold-vtl-vtl publish --vtl {m['vtl_id']}
ExecStop=/usr/local/libexec/cold-vtl-vtl withdraw --vtl {m['vtl_id']}
[Install]
WantedBy=multi-user.target
''')
        telemetry=self.telemetry_unit(m)
        atomic(Path(m['state_root'])/'telemetry.json',{'output_dir':'/run/vaultcenter/telemetry','vtls':[{
            'vtl_id':m['vtl_id'],'fifo':f"/run/vaultcenter-vtl-{m['vtl_id']}/status.fifo",'media_root':m['media_root'],
            'library_queue_id':lib['id'],'require_heartbeat':True,'drives':[{'queue_id':drive['id'],'ordinal':1,'label':'Drive 1','pid_file':f"/run/vaultcenter-vtl-{m['vtl_id']}/pid-{drive['id']}"}]}]})
        atomic_text(units/telemetry,f"""[Unit]
Description=VaultCenter telemetry {m['vtl_id']}
After=local-fs.target
[Service]
ExecStart=/usr/bin/python3 /usr/local/libexec/vaultcenter-telemetry --config {m['state_root']}/telemetry.json
RuntimeDirectory=vaultcenter/telemetry
RuntimeDirectoryMode=0700
RuntimeDirectoryPreserve=yes
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/run/vaultcenter/telemetry
Restart=on-failure
[Install]
WantedBy=multi-user.target
""")
        self.run(['systemctl','link',*[str(units/u) for u in [*self.units(m),telemetry]]])
        self.run(['systemctl','daemon-reload'])

    def apply(self,vtl_id,token,actor):
        identifier(vtl_id)
        with locked(self.root/'vtls'/vtl_id/'operation.lock'):
            m=self.manifest(vtl_id)
            with self.db() as db:
                record=db.execute('SELECT * FROM vtl_provisioning WHERE vtl_id=?',(vtl_id,)).fetchone()
                journal=json.loads(record['journal'])
                if journal.get('token')!=token:raise ValueError('Preparation belongs to another request.')
                if record['phase'] in ('configured','active'):return {'vtl_id':vtl_id,'revision':m['revision'],'phase':record['phase']}
                if record['phase']=='prepared' and journal['expires']<time.time():raise ValueError('Preparation expired.')
                revision=db.execute('SELECT provisioning_revision FROM vtls WHERE id=?',(vtl_id,)).fetchone()[0]
                if revision!=journal['expected_revision']:raise ValueError('Catalogue revision changed.')
                targets=self.targets()
                if m['target']['id'] in targets:raise ValueError('Target already published; automatic adoption is not allowed.')
                reserved={(str(d['channel']),str(d['target']),str(d['lun'])) for d in [m['library'],*m['drives']]}
                if any(tuple(d['address'].split(':')[1:]) in reserved for d in self.devices(identify=False)):raise ValueError('A SCSI resource has been used since preparation.')
                if self.storage(vtl_id).get('phase')!='ready':raise ValueError('Storage not available.')
                db.execute("UPDATE vtl_provisioning SET phase='applying' WHERE vtl_id=?",(vtl_id,))
            self.write_configuration(m)
            with self.db() as db:
                db.execute("UPDATE vtls SET provisioning_revision=?,media_root=?,restore_root=?,device_configuration=? WHERE id=?",(m['revision'],m['media_root'],m['restore_root'],json.dumps(m),vtl_id))
                db.execute("UPDATE vtl_provisioning SET phase='configured' WHERE vtl_id=?",(vtl_id,))
            return {'vtl_id':vtl_id,'revision':m['revision'],'phase':'configured'}

    def remote(self,m,action,lease):
        if client(m)=='veeam':
            from client_lease import LocalLease
            value=LocalLease(m['state_root'],m['vtl_id']).handle(action,lease)
        else:
            pbs=m['pbs']
            command=['ssh','-F','/dev/null','-i',pbs['key_file'],'-o','BatchMode=yes','-o','ConnectTimeout=5',
                     '-o','StrictHostKeyChecking=yes','-o','IdentitiesOnly=yes','-o','UserKnownHostsFile='+pbs['known_hosts'],
                     '--',pbs['pbs'],'--vtl',m['vtl_id'],action,lease]
            value=json.loads(self.run(command))
        if value.get('ok') is not True or value.get('vtl_id')!=m['vtl_id']:raise ValueError('PBS guard not confirmed for this VTL.' if client(m)=='pbs' else 'Maintenance lease not confirmed for this VTL.')
        if action not in ('release','status') and (value.get('lease')!=lease or not value.get('locked')):raise ValueError('PBS lease not confirmed.' if client(m)=='pbs' else 'Maintenance lease not confirmed for this VTL.')
        return value

    def status(self,vtl_id):
        identifier(vtl_id)
        with self.db() as db:row=db.execute('SELECT phase FROM vtl_provisioning WHERE vtl_id=?',(vtl_id,)).fetchone()
        if not row:return {'vtl_id':vtl_id,'phase':'unconfigured'}
        phase=row['phase'];m=self.manifest(vtl_id)
        try:self.qualified();qualified=True
        except (ValueError,OSError):qualified=False
        if phase in ('prepared','applying','failed'):
            return {'vtl_id':vtl_id,'revision':m['revision'],'phase':phase,'qualified':qualified,
                    'message':'Resume applying the saved preparation.' if phase=='applying' else None}
        m=self.current(vtl_id);activation=Path(m['state_root'])/'activation.json';message=None
        if activation.exists():
            journal=json.loads(activation.read_text())
            if journal.get('vtl_id')!=vtl_id or journal.get('revision')!=m['revision']:raise ValueError('Activation journal belongs to another configuration.')
            if journal.get('phase')!='active':
                phase='awaiting_pbs' if journal.get('phase')=='awaiting_pbs' else 'activation_incomplete'
                message=('Associate changer '+m['pbs']['changer']+' and drive '+m['pbs']['drive']+' in PBS, then resume activation. The lease remains reserved.' if phase=='awaiting_pbs' else 'Activation interrupted: resume to check the services and complete the PBS lease.' if client(m)=='pbs' else 'Activation interrupted: resume to check the services.')
        return {'vtl_id':vtl_id,'revision':m['revision'],'phase':phase,'qualified':qualified,'message':message,'services':self.services(vtl_id)}

    def activate(self,vtl_id,actor):
        self.qualified()
        with locked(self.root/'vtls'/identifier(vtl_id)/'operation.lock'):
            m=self.current(vtl_id);root=Path(m['state_root'])
            if self.storage(vtl_id).get('phase')!='ready':raise ValueError('Storage unavailable.')
            self.no_workers(vtl_id)
            journal_path=root/'activation.json'
            if journal_path.exists():journal=json.loads(journal_path.read_text())
            else:
                if (root/'ever-published').exists():raise ValueError('VTL already published: normal maintenance required.')
                journal={'vtl_id':vtl_id,'revision':m['revision'],'lease':str(uuid.uuid4()),'phase':'prepared','actor':actor}
                atomic(journal_path,journal)
            if journal['vtl_id']!=vtl_id or journal['revision']!=m['revision']:raise ValueError('Activation belongs to another configuration.')
            if journal['phase']=='active':return {'vtl_id':vtl_id,'phase':'active','revision':m['revision']}
            lease=journal['lease']
            if journal['phase']=='verified':
                if set(self.services(vtl_id).values())=={'active'}:return self.finalize_activation(m,journal)
                remote=self.remote(m,'status',lease)
                if not remote.get('locked') and remote.get('lease') is None:self.remote(m,'acquire',lease)
            remote=self.remote(m,'status' if (root/'ever-published').exists() else 'bootstrap',lease)
            if remote.get('lease')!=lease or not remote.get('locked'):raise ValueError('PBS activation lease lost.' if client(m)=='pbs' else 'Activation lease lost.')
            atomic(root/'maintenance-state.json',{'phase':'leaving','lease':lease,'original':dict.fromkeys(self.units(m),'active')})
            journal['phase']='starting';atomic(journal_path,journal)
            for marker in ('storage-services-held','storage-bootstrap','maintenance-block'):(root/marker).unlink(missing_ok=True)
            try:
                self.run(['systemctl','start',self.telemetry_unit(m)])
                for unit in self.units(m):self.run(['systemctl','start',unit])
                if set(self.services(vtl_id).values())!={'active'}:raise ValueError('Services not confirmed.')
                observed=self.remote(m,'status',lease)
                if observed.get('bootstrap') and not observed.get('registered'):
                    journal['phase']='awaiting_pbs';atomic(journal_path,journal)
                    return {'vtl_id':vtl_id,'phase':'awaiting_pbs','revision':m['revision'],'message':'Associate the published drives in PBS, then resume activation.'}
                self.remote(m,'verify',lease)
                journal['phase']='verified';atomic(journal_path,journal)
            except Exception:
                atomic(root/'maintenance-block',{'lease':lease})
                atomic(root/'storage-services-held',{'reason':'activation_interrupted'})
                # Stop only this leased VTL; retain the journal and PBS lease.
                for unit in reversed(self.units(m)):
                    try:self.run(['systemctl','stop',unit])
                    except Exception:pass
                raise
            return self.finalize_activation(m,journal)

    def finalize_activation(self,m,journal):
        vtl_id=m['vtl_id'];root=Path(m['state_root'])
        if set(self.services(vtl_id).values())!={'active'}:raise ValueError('Check the services before completing activation.')
        remote=self.remote(m,'status',journal['lease'])
        if remote.get('locked'):
            if remote.get('lease')!=journal['lease']:raise ValueError('Another PBS lease holds this VTL.' if client(m)=='pbs' else 'Another maintenance lease holds this VTL.')
            self.remote(m,'release',journal['lease'])
        elif remote.get('lease') is not None:raise ValueError('Inconsistent PBS lease state.' if client(m)=='pbs' else 'Inconsistent maintenance lease state.')
        atomic(root/'maintenance-state.json',{'phase':'online','lease':None,'original':{},'message':None})
        self.run(['systemctl','enable',*self.units(m),self.telemetry_unit(m)])
        with self.db() as db:
            db.execute("UPDATE vtl_provisioning SET phase='active' WHERE vtl_id=?",(vtl_id,))
            db.execute("UPDATE vtls SET lifecycle='ready' WHERE id=?",(vtl_id,))
        journal['phase']='active';atomic(root/'activation.json',journal)
        return {'vtl_id':vtl_id,'phase':'active','revision':m['revision']}

    def qualified(self):
        # Explicit, ephemeral lab mode is never a production qualification receipt.
        if self.settings.get('acceptance_only') is True and Path('/run/vaultcenter-acceptance-only').is_file():return
        file=self.root/'isolation-qualified.json'
        if not file.exists():raise ValueError('The isolation acceptance test must be validated before activation.')
        value=json.loads(file.read_text())
        if value.get('runtime_sha256')!=hashlib.sha256(Path(__file__).read_bytes()).hexdigest() or value.get('isolation_verified') is not True:
            raise ValueError('The acceptance test does not match this controller version.')

    def current(self,vtl_id):return resolve_runtime(vtl_id,root=self.root)

    def no_workers(self,vtl_id):
        identifier(vtl_id)
        text=self.run(['systemctl','list-units','--all','--no-legend','--plain','--type=service','vaultcenter-worker-'+vtl_id+'-*.service'])
        if any(line.strip() and len(line.split())>=4 and line.split()[2] not in ('inactive','failed') for line in text.splitlines()):
            raise ValueError('A worker of this VTL is still active.')

    def services(self,vtl_id):
        m=self.current(vtl_id);states={}
        for unit in self.units(m):
            values=dict(line.split('=',1) for line in self.run(['systemctl','show','--property=LoadState,ActiveState,ControlGroup',unit]).splitlines() if '=' in line)
            if values.get('LoadState')!='loaded':raise ValueError('A service of this VTL is missing.')
            states[unit]=values['ActiveState']
            if values['ActiveState'] in ('inactive','failed') and values.get('ControlGroup'):
                procs=Path('/sys/fs/cgroup')/values['ControlGroup'].lstrip('/')/'cgroup.procs'
                if procs.exists() and procs.read_text().strip():raise ValueError('A process of this VTL outlives its service.')
        targets=self.targets();expected=m['target']
        if expected['id'] in targets and targets[expected['id']]!=expected['iqn']:
            raise ValueError('The iSCSI target was replaced.')
        if states[self.units(m)[-1]]=='inactive' and expected['id'] in targets:
            raise ValueError('The target of this VTL is still published although its service is stopped.')
        return states

    def lease(self,vtl_id):
        m=self.current(vtl_id);root=Path(m['state_root'])
        state=json.loads((root/'maintenance-state.json').read_text())
        block=json.loads((root/'maintenance-block').read_text())
        if not state.get('lease') or block.get('lease')!=state['lease']:raise ValueError('Maintenance lease missing.')
        if client(m)=='veeam':
            from client_lease import LocalLease
            remote=LocalLease(m['state_root'],vtl_id).handle('status')
        else:
            pbs=m['pbs']
            command=['ssh','-F','/dev/null','-i',pbs['key_file'],'-o','BatchMode=yes','-o','ConnectTimeout=5',
                     '-o','StrictHostKeyChecking=yes','-o','IdentitiesOnly=yes','-o','UserKnownHostsFile='+pbs['known_hosts'],
                     '--',pbs['pbs'],'--vtl',vtl_id,'status']
            remote=json.loads(self.run(command))
        if remote.get('vtl_id')!=vtl_id or not remote.get('ok') or not remote.get('locked') or remote.get('lease')!=state['lease']:
            raise ValueError('The PBS lease is no longer verified for this VTL.' if client(m)=='pbs' else 'The maintenance lease is no longer verified for this VTL.')
        return state['lease']

    def target_detail(self,target_id):
        text=self.run(['tgtadm','--lld','iscsi','--mode','target','--op','show'])
        blocks=re.split(r'(?=^Target )',text,flags=re.M)
        found=[block for block in blocks if block.startswith(f'Target {target_id}: ')]
        if len(found)!=1:raise ValueError('Target missing or ambiguous.')
        return found[0]

    def verify_target(self,m,devices,partial=False):
        text=self.target_detail(m['target']['id'])
        if not text.startswith(f"Target {m['target']['id']}: {m['target']['iqn']}\n"):
            raise ValueError('Target identity differs.')
        luns={int(n):body for n,body in re.findall(r'LUN: (\d+)\n(.*?)(?=        LUN: |    Account information:|    ACL information:|\Z)',text,re.S)}
        expected={0,*range(1,len(devices)+1)}
        if (not partial and set(luns)!=expected) or (partial and (0 not in luns or not set(luns)<=expected)):
            raise ValueError('LUN list differs from the manifest.')
        for lun,path in enumerate(devices,1):
            if partial and lun not in luns:continue
            if not re.search(r'Backing store type: sg\s*$',luns[lun],re.M) or not re.search(r'Backing store path: '+re.escape(path)+r'\s*$',luns[lun],re.M):
                raise ValueError('A LUN points to another device.')
        acl=text.split('    ACL information:\n')[-1].splitlines()
        observed=[line.strip() for line in acl if line.strip()]
        if observed!=[m['target']['initiator']] and not (partial and not observed):raise ValueError('Initiator binding differs.')
        return True

    def publish(self,vtl_id):
        self.qualified();m=self.current(vtl_id)
        if self.storage(vtl_id).get('phase')!='ready':raise ValueError('Storage unavailable.')
        root=Path(m['state_root'])
        if any((root/name).exists() for name in ('maintenance-block','storage-block','storage-services-held')):
            raise ValueError('This VTL is kept stopped.')
        expected=m['target'];targets=self.targets()
        devices=self.wait_devices(m)
        if expected['id'] in targets:
            journal=json.loads((root/'publication.json').read_text())
            if journal.get('target')!=expected or journal.get('devices')!=devices:raise ValueError('Target published without a matching journal.')
            if journal.get('phase')=='creating':
                # A previous process may have died between target/LUN/ACL writes.
                # Rebuild only its verified subset, under the original PBS lease.
                self.verify_target(m,devices,partial=True)
                state=json.loads((root/'maintenance-state.json').read_text())
                if state.get('phase')!='leaving' or not state.get('lease'):raise ValueError('Recovery lease missing.')
                remote=self.remote(m,'status',state['lease'])
                if not remote.get('locked') or remote.get('lease')!=state['lease']:raise ValueError('Recovery lease lost.')
                self.no_workers(vtl_id)
                self.run(['tgtadm','--lld','iscsi','--mode','target','--op','delete','--tid',str(expected['id']),'--force'])
            else:
                self.verify_target(m,devices)
                atomic(root/'publication.json',{'phase':'published','devices':devices,'target':expected})
                return {'vtl_id':vtl_id,'published':True}
        atomic(root/'ever-published',{'vtl_id':vtl_id})
        atomic(root/'publication.json',{'phase':'creating','devices':devices,'target':expected})
        self.run(['tgtadm','--lld','iscsi','--mode','target','--op','new','--tid',str(expected['id']),'--targetname',expected['iqn']])
        # Allow a complete 256 KiB write without R2T round trips. The initiator
        # must opt in too; publish these values before allowing connections.
        for name,value in (('MaxRecvDataSegmentLength','262144'),('MaxBurstLength','262144'),
                           ('FirstBurstLength','262144'),('ImmediateData','Yes'),('InitialR2T','No')):
            self.run(['tgtadm','--lld','iscsi','--mode','target','--op','update','--tid',str(expected['id']),
                      '--name',name,'--value',value])
        for lun,device in enumerate(devices,1):
            self.run(['tgtadm','--lld','iscsi','--mode','logicalunit','--op','new','--tid',str(expected['id']),'--lun',str(lun),
                      '--device-type','pt','--bstype','sg','--backing-store',device])
        self.run(['tgtadm','--lld','iscsi','--mode','target','--op','bind','--tid',str(expected['id']),'--initiator-address',expected['initiator']])
        self.verify_target(m,devices)
        atomic(root/'publication.json',{'phase':'published','devices':devices,'target':expected})
        return {'vtl_id':vtl_id,'published':True}

    def withdraw(self,vtl_id):
        m=self.current(vtl_id);expected=m['target'];targets=self.targets()
        if expected['id'] not in targets:return {'vtl_id':vtl_id,'published':False}
        if targets[expected['id']]!=expected['iqn']:raise ValueError('Target identity differs; withdrawal refused.')
        self.lease(vtl_id);self.no_workers(vtl_id)
        journal=json.loads((Path(m['state_root'])/'publication.json').read_text())
        if journal.get('target')!=expected or not isinstance(journal.get('devices'),list):raise ValueError('Publication journal missing or different.')
        self.verify_target(m,journal['devices'],partial=journal.get('phase')=='creating')
        # Force disconnect only this leased target, as proven by the SCSI probe.
        self.run(['tgtadm','--lld','iscsi','--mode','target','--op','delete','--tid',str(expected['id']),'--force'])
        if expected['id'] in self.targets():raise ValueError('The target is still published.')
        atomic(Path(m['state_root'])/'publication.json',{'phase':'withdrawn','target':expected})
        return {'vtl_id':vtl_id,'published':False}

    def storage_idle(self,vtl_id):
        identifier(vtl_id)
        with self.db() as db:
            row=db.execute('SELECT lifecycle FROM vtls WHERE id=?',(vtl_id,)).fetchone()
            if not row:raise ValueError('Unknown VTL.')
            if db.execute("SELECT 1 FROM jobs WHERE vtl_id=? AND status IN ('queued','running') LIMIT 1",(vtl_id,)).fetchone():
                raise ValueError('A job of this VTL is active.')
        root=self.root/'vtls'/vtl_id
        if not (root/'runtime.json').exists():
            if row['lifecycle']!='draft' or (root/'ever-published').exists():raise ValueError('The hardware state of the VTL cannot be verified.')
            if any(iqn.endswith(':vtl-'+vtl_id) for iqn in self.targets().values()):raise ValueError('Unexpected target for a VTL being prepared.')
        else:
            self.no_workers(vtl_id)
            if any(state!='inactive' for state in self.services(vtl_id).values()):raise ValueError('Stop the services of this VTL before the change.')
            if (root/'ever-published').exists():self.lease(vtl_id)
        return {'vtl_id':vtl_id,'stopped':True,'lease_verified':True}


def worker_runtime(vtl_id,revision,root=ROOT):
    m=resolve_runtime(vtl_id,revision,root)
    with closing(sqlite3.connect(Path(root)/'catalog.db')) as db:
        row=db.execute('SELECT p.phase,v.lifecycle,v.provisioning_revision FROM vtl_provisioning p JOIN vtls v ON v.id=p.vtl_id WHERE p.vtl_id=?',(vtl_id,)).fetchone()
    if row!=('active','ready',revision):raise ValueError('Library not activated for this worker.')
    Registry(root).qualified()
    return m


def worker_launch(vtl_id,revision,args,root=ROOT):
    """Build a tracked process; the service checks the manifest again before exec."""
    m=worker_runtime(vtl_id,revision,root)
    unit='vaultcenter-worker-'+vtl_id+'-'+uuid.uuid4().hex
    command=['systemd-run','--quiet','--pipe','--wait','--collect','--service-type=exec',
             '--property=KillMode=control-group','--unit='+unit]
    for name in ('COLD_VTL_DATABASE','COLD_VTL_DESTINATION_ID','COLD_VTL_CONFIGURATION_REVISION','COLD_VTL_STORAGE_SOCKET'):
        if name in os.environ:command.append('--setenv='+name+'='+os.environ[name])
    command+=['--setenv=COLD_VTL_VTL_ID='+vtl_id,'--setenv=COLD_VTL_RUNTIME_REVISION='+str(m['revision']),
              '/usr/local/libexec/cold-vtl-worker-run','--execute',*args]
    return command
