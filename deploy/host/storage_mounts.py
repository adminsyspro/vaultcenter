"""Publish one verified host directory without recreating the shared container."""
import json
import os
from pathlib import Path
import subprocess
import sys
from storage_paths import directory, verify_identity
from vtl_state import atomic, identifier, file_lock

# Obtain the mount of the opened directory, including overmounted same-device binds.
VIEW = '''import json,os,sys
p=sys.argv[1]
f=os.open(p,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
s=os.fstat(f);v=os.fstatvfs(f)
mid=next(x.split()[1] for x in open('/proc/self/fdinfo/'+str(f)) if x.startswith('mnt_id:'))
row=next(x.split() for x in open('/proc/self/mountinfo') if x.split()[0]==mid)
i=row.index('-')
decode=lambda s:s.replace('\\\\040',' ').replace('\\\\011','\\t').replace('\\\\134','\\\\')
print(json.dumps(dict(inode=s.st_ino,device=s.st_dev,mount_id=mid,mountpoint=decode(row[4]),root=decode(row[3]),filesystem=row[i+1],source=decode(row[i+2]),readonly=bool(v.f_flag&os.ST_RDONLY),shared=any(x.startswith('shared:') for x in row[6:i]))))
os.close(f)
'''

class Mounts:
    def __init__(self, config):
        self.config = config
        self.root = Path(config['mount_root'])
        self.receipts = Path(config['host_state'])/'mounts'
        self.receipts.mkdir(parents=True,exist_ok=True,mode=0o700)

    def run(self, args):
        result = subprocess.run(args,capture_output=True,text=True,timeout=30)
        if result.returncode:
            raise ValueError('Mount cannot be verified: '+result.stderr.strip()[-500:])
        return result.stdout

    def view(self, path, container=False):
        command=['python3','-c',VIEW,str(path)]
        if container: command=['docker','exec',self.config['container'],*command]
        return json.loads(self.run(command))

    def prepare(self):
        with file_lock(self.receipts/'registry.lock'):
            self.root.mkdir(parents=True,exist_ok=True,mode=0o700)
            with directory(str(self.root)): pass
            view=self.view(self.root)
            if view['mountpoint'] != str(self.root):
                self.run(['mount','--bind',str(self.root),str(self.root)])
            if not self.view(self.root)['shared']:
                self.run(['mount','--make-rshared',str(self.root)])

    def target(self, vtl_id): return self.root/identifier(vtl_id)
    def internal(self, vtl_id): return '/var/lib/vaultcenter-media/'+identifier(vtl_id)
    def receipt(self, vtl_id): return self.receipts/(identifier(vtl_id)+'.json')

    def empty(self, vtl_id, view):
        return (view['mountpoint']==str(self.target(vtl_id)) and
                view['source']=='vaultcenter-empty:'+vtl_id and view['filesystem']=='tmpfs' and view['readonly'])

    def slot(self, vtl_id):
        target=self.target(vtl_id);target.mkdir(mode=0o700,exist_ok=True)
        view=self.view(target)
        if view['mountpoint'] != str(target):
            with directory(str(target)) as fd:
                if os.listdir(fd): raise ValueError('The publication point contains unmanaged data.')
            self.run(['mount','-t','tmpfs','-o','ro,size=4k,nosuid,nodev,noexec','vaultcenter-empty:'+vtl_id,str(target)])
        elif not self.empty(vtl_id,view) and not self.receipt(vtl_id).exists():
            raise ValueError('Existing mount without a publication journal.')

    def verify(self, vtl_id, path, identity, replica=True):
        capacity=verify_identity(path,identity)
        with directory(path) as fd: source=os.fstat(fd)
        target=self.view(self.target(vtl_id))
        views=[(target,str(self.target(vtl_id)))]
        if replica: views.append((self.view(self.internal(vtl_id),container=True),self.internal(vtl_id)))
        for view,mountpoint in views:
            if (view['mountpoint']!=mountpoint or view['inode']!=source.st_ino or
                view['device']!=source.st_dev or view['readonly'] or view['filesystem']!=identity['filesystem']):
                raise ValueError('The VTL uses a mount that is missing or differs from the verified storage.')
        return capacity

    def known(self, vtl_id, view, receipt):
        for candidate in (receipt,receipt.get('previous')):
            if not candidate: continue
            observed=candidate.get('observed')
            if observed and all(view[k]==observed[k] for k in ('inode','device','root','filesystem')):
                return True
            # Recovery after bind succeeded but before recording its observed mount.
            try:
                verify_identity(candidate['path'],candidate['identity'])
                with directory(candidate['path']) as fd: st=os.fstat(fd)
                if view['inode']==st.st_ino and view['device']==st.st_dev: return True
            except (ValueError,OSError,KeyError): pass
        return False

    def withdraw(self, vtl_id, replica=True):
        target=self.target(vtl_id);view=self.view(target)
        if self.empty(vtl_id,view): return
        file=self.receipt(vtl_id)
        if not file.exists() or view['mountpoint']!=str(target) or not self.known(vtl_id,view,json.loads(file.read_text())):
            raise ValueError('The mount to remove does not match the journal of this VTL.')
        self.run(['umount',str(target)])
        if not self.empty(vtl_id,self.view(target)):
            raise ValueError('The read-only fallback mount is not confirmed.')
        if replica:
            observed=self.view(self.internal(vtl_id),container=True)
            if observed['source']!='vaultcenter-empty:'+vtl_id or not observed['readonly']:
                raise ValueError('The removal did not propagate to this VTL.')

    def publish(self, vtl_id, path, identity, replica=True):
        verify_identity(path,identity);self.prepare();self.slot(vtl_id)
        file=self.receipt(vtl_id)
        previous=json.loads(file.read_text()) if file.exists() else None
        if not self.empty(vtl_id,self.view(self.target(vtl_id))):
            try:
                result=self.verify(vtl_id,path,identity,replica=replica)
            except ValueError:
                self.withdraw(vtl_id,replica=replica)
            else:
                atomic(file,{'path':path,'identity':identity,'observed':self.view(self.target(vtl_id))})
                return result
        # An intent makes a bind recoverable even if recording completion fails.
        atomic(file,{'path':path,'identity':identity,'previous':previous})
        verify_identity(path,identity)
        self.run(['mount','--bind',path,str(self.target(vtl_id))])
        result=self.verify(vtl_id,path,identity,replica=replica)
        atomic(file,{'path':path,'identity':identity,'observed':self.view(self.target(vtl_id))})
        return result

    def restore(self, vtl_id, path, identity):
        # Host boot lost the previous tmpfs mounts; install the safe floor first.
        self.prepare();self.slot(vtl_id)
        return self.publish(vtl_id,path,identity,replica=False)

    def restore_states(self, values):
        self.prepare()
        for value in values:
            vtl_id=identifier(value['vtl_id'])
            try:
                self.slot(vtl_id)
                if value['phase']=='ready':self.restore(vtl_id,value['path'],value['identity'])
            except (ValueError,OSError) as error:
                print('Storage not restored for VTL '+vtl_id+': '+str(error),file=sys.stderr)
