#!/usr/bin/env python3
"""Opt-in Linux proof on disposable loop filesystems and a private container."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import threading
import time

A='00000000-0000-4000-8000-000000000021'
B='00000000-0000-4000-8000-000000000031'

def run(*args, check=True):
    return subprocess.run(args,check=check,capture_output=True,text=True,timeout=60)

def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--run',action='store_true',required=True)
    parser.add_argument('--image',required=True); parser.add_argument('--root',type=Path,required=True)
    args=parser.parse_args(); root=args.root.resolve()
    if root.exists(): raise SystemExit('Use a new evidence directory; never reuse existing storage.')
    sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'host'))
    from storage_mounts import Mounts
    from storage_paths import inspect_path
    root.mkdir(parents=True,mode=0o700)
    name='vaultcenter-mount-proof-'+str(int(time.time()))
    config={'mount_root':str(root/'published'),'host_state':str(root/'host'),'container':name}
    mounts=Mounts(config); created=[]; reads=[]; errors=[]; stop=threading.Event()
    try:
        destinations={}
        for label in ('disk-a','disk-b','disk-new'):
            image=root/(label+'.img');run('truncate','-s','128M',str(image));run('mkfs.ext4','-q','-F',str(image))
            target=root/label;target.mkdir();run('mount','-o','loop',str(image),str(target));created.append(target)
            data=target/'tapes';data.mkdir();destinations[label]=inspect_path(str(data),[])
        mounts.prepare()
        run('docker','run','-d','--name',name,'--network','none','--mount',f'type=bind,src={root}/published,dst=/var/lib/vaultcenter-media,bind-propagation=rslave','--entrypoint','sleep',args.image,'infinity')
        original=run('docker','inspect','--format','{{.Id}} {{.State.Pid}} {{.State.StartedAt}}',name).stdout
        # Publish after container startup, including entirely new UUID slots.
        for vtl,label in ((A,'disk-a'),(B,'disk-b')):
            c=destinations[label];mounts.publish(vtl,c['path'],c['identity'])
        payload=b'B must remain available\n'*1024
        (Path(destinations['disk-b']['path'])/'payload').write_bytes(payload)
        expected=hashlib.sha256(payload).hexdigest()
        def reader():
            while not stop.is_set():
                try:
                    result=run('docker','exec',name,'python3','-c',
                        'import hashlib,pathlib,sys;p=pathlib.Path(sys.argv[1]);print(hashlib.sha256((p/"payload").read_bytes()).hexdigest());(p/"activity").write_text("B active")',
                        '/var/lib/vaultcenter-media/'+B).stdout.strip()
                    if result!=expected: raise AssertionError('Changed payload on B')
                    reads.append(time.monotonic())
                except Exception as error: errors.append(str(error));stop.set()
        with ThreadPoolExecutor(max_workers=1) as pool:
            future=pool.submit(reader)
            while len(reads)<2 and not errors: time.sleep(.1)
            c=destinations['disk-new'];mounts.publish(A,c['path'],c['identity'])
            mounts.verify(A,c['path'],c['identity'])
            mounts.withdraw(A)
            denied=run('docker','exec',name,'touch','/var/lib/vaultcenter-media/'+A+'/must-not-exist',check=False)
            assert denied.returncode!=0, 'Fallback accepts writes'
            # B continues while A has no data mount, then A returns.
            before=len(reads);deadline=time.monotonic()+15
            while len(reads)<before+3 and not errors and time.monotonic()<deadline:time.sleep(.1)
            assert len(reads)>=before+3
            mounts.publish(A,c['path'],c['identity'])
            stop.set();future.result()
        assert not errors,errors
        assert original==run('docker','inspect','--format','{{.Id}} {{.State.Pid}} {{.State.StartedAt}}',name).stdout
        # Source withdrawal must never be confused with a still-live bind replica.
        run('umount',str(root/'disk-new'));created.remove(root/'disk-new')
        (root/'disk-new/tapes').mkdir()
        try:mounts.verify(A,c['path'],c['identity'])
        except ValueError:pass
        else:raise AssertionError('Missing original source was accepted')
        run('mount','-o','loop',str(root/'disk-new.img'),str(root/'disk-new'));created.append(root/'disk-new')
        mounts.withdraw(A)
        run('docker','restart',name)
        mounts.verify(B,destinations['disk-b']['path'],destinations['disk-b']['identity'])
        denied=run('docker','exec',name,'touch','/var/lib/vaultcenter-media/'+A+'/must-not-exist',check=False)
        assert denied.returncode!=0
        result={'a_changed_without_container_restart':True,'b_reads':len(reads),'b_errors':errors,
                'b_sha256':expected,'fallback_readonly':True,'missing_source_rejected':True,'b_ready_after_restart_without_a':True}
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    finally:
        stop.set();run('docker','rm','-f',name,check=False)
        # Unmount only this disposable tree, deepest/topmost first, never global.
        for _ in range(12):
            found=[]
            for line in Path('/proc/self/mountinfo').read_text().splitlines():
                target=line.split()[4]
                if target.startswith(str(root)+'/'):found.append(target)
            if not found:break
            for target in sorted(set(found),key=len,reverse=True):run('umount',target,check=False)
        remaining=[line for line in Path('/proc/self/mountinfo').read_text().splitlines() if line.split()[4].startswith(str(root)+'/')]
        if remaining:raise RuntimeError('Disposable mounts still present: '+str(remaining))

if __name__=='__main__':main()
