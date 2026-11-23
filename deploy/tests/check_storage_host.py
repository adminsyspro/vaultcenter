#!/usr/bin/env python3
"""Opt-in disposable Linux appliance acceptance. Never attaches PBS or S3."""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import time
import urllib.request


def run(*args, **kw): return subprocess.check_output(args,text=True,**kw).strip()

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--run',action='store_true',required=True);parser.add_argument('--image',required=True);parser.add_argument('--root',required=True);args=parser.parse_args()
    if os.geteuid()!=0:raise SystemExit('Dedicated Linux VM root required')
    root=Path(args.root);root.mkdir(mode=0o700,parents=False,exist_ok=False);os.umask(0o077)
    host=Path(__file__).resolve().parents[1]/'host';sys.path.insert(0,str(host))
    import storage_controller
    name='vaultcenter-storage-check-'+secrets.token_hex(3)
    state=root/'state';media=root/'media';config=root/'config';bridge=root/'bridge';hoststate=root/'host'
    for path in (state,media,config,bridge,hoststate):path.mkdir()
    mount=root/'mount';mount.mkdir();loop=root/'disk.img'
    with loop.open('wb') as stream:stream.truncate(256*1024**2)
    run('mkfs.ext4','-q','-F',str(loop));run('mount','-o','loop',str(loop),str(mount))
    destination=mount/'tapes';destination.mkdir()
    url='http://127.0.0.1:18080'
    (config/'auth.env').write_text(f'COLD_VTL_LISTEN=127.0.0.1:18080\nCOLD_VTL_PUBLIC_URL={url}\nCOLD_VTL_ALLOW_INSECURE_HTTP=true\n')
    (config/'socket.conf').write_text('[Service]\nEnvironment=COLD_VTL_STORAGE_SOCKET=/run/vaultcenter-host/storage.sock\n')
    (config/'targets.conf').write_text('default-driver iscsi\n# Acceptance has no published target.\n')
    for marker in ('storage-managed','storage-bootstrap','storage-services-held'):(state/marker).write_text('{}\n')
    def bind(source,target,ro=False):return {'type':'bind','source':str(source),'target':target,'read_only':ro,'bind':{'create_host_path':False}}
    compose={'name':name,'services':{'appliance':{'image':args.image,'privileged':True,'network_mode':'host','cgroup':'host','tmpfs':['/run','/run/lock','/tmp'],'volumes':[bind('/sys/fs/cgroup','/sys/fs/cgroup'),bind('/dev','/dev'),bind(state,'/var/lib/cold-vtl'),bind(media,'/opt/mhvtl'),bind(config,'/etc/cold-vtl',True),bind(config/'targets.conf','/etc/tgt/targets.conf',True),bind(config/'socket.conf','/etc/systemd/system/cold-vtld.service.d/storage-socket.conf',True),bind(bridge,'/run/vaultcenter-host',True)],'healthcheck':{'test':['CMD','curl','--fail','--silent',url+'/health'],'interval':'2s','timeout':'3s','start_period':'5s','retries':5}}}}
    base=root/'compose.json';base.write_text(json.dumps(compose))
    controller_config={'state_dir':str(state),'media_dir':str(media),'compose_file':str(base),'project':name,'container':name+'-appliance-1','socket':str(bridge/'storage.sock'),'host_state':str(hoststate),'bootstrap':True}
    settings=root/'host.json';settings.write_text(json.dumps(controller_config))
    controller=None
    password=secrets.token_urlsafe(24)
    def dc(*extra):return run('docker','compose','-f',str(base),*extra)
    opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()));csrf=None
    def request(path,body=None):
        headers={'Origin':url,'Content-Type':'application/json'}
        if csrf:headers['X-CSRF-Token']=csrf
        with opener.open(urllib.request.Request(url+path,data=json.dumps(body).encode() if body is not None else None,headers=headers),timeout=20) as response:return json.load(response)
    def healthy():
        for _ in range(100):
            try:
                if request('/health')['status']=='ok':return
            except Exception:pass
            time.sleep(1)
        raise RuntimeError('API failed to start')
    def login():return request('/auth/login',{'username':'admin','password':password})['csrf_token']
    def hashes():return {str(p.relative_to(destination)):hashlib.sha256(p.read_bytes()).hexdigest() for p in destination.rglob('*') if p.is_file()}
    try:
        subprocess.run(['docker','run','--rm','-i','--mount',f'type=bind,src={state},dst=/var/lib/cold-vtl','--entrypoint','/usr/local/bin/cold-vtld',args.image,'create-user','admin','--role','admin'],input=password+'\n',text=True,check=True,capture_output=True)
        log=(root/'controller.log').open('w')
        controller=subprocess.Popen([sys.executable,str(host/'storage_controller.py'),'--config',str(settings)],stdout=log,stderr=log)
        for _ in range(50):
            if (bridge/'storage.sock').exists():break
            time.sleep(.1)
        dc('up','-d');healthy();csrf=login()
        status=request('/v1/settings/tape-storage');assert status['phase']=='unconfigured',status
        check=request('/v1/settings/tape-storage/check',{'path':str(destination)})
        body={'path':str(destination),'revision':0,'request_id':'linux-acceptance','check_id':check['check_id']}
        result=request('/v1/settings/tape-storage/apply',body);assert result['phase']=='applying',result
        for _ in range(120):
            try:
                status=request('/v1/settings/tape-storage')
                if status['phase']=='ready':break
                if status['phase']=='recovery_required':raise RuntimeError(status)
            except (OSError,urllib.error.URLError):pass
            time.sleep(1)
        else:raise RuntimeError('Storage never became ready')
        assert status['path']==str(destination),status
        assert request('/v1/settings/tape-storage/apply',body)['operation']['id']=='linux-acceptance'
        print('HTTP admin → checked path → journal → container recreation: passed',flush=True)
        container=name+'-appliance-1'
        # Write only a tiny disposable mhVTL cartridge, no PBS or cloud activity.
        run('docker','exec',container,'mktape','-l','10','-H','/opt/mhvtl','-m','PATH01L8','-s','64','-t','data','-d','LTO8')
        before=hashes();assert before and (destination/'PATH01L8/mam').is_file()
        override=hoststate/'mount.json'
        dc('-f',str(override),'up','-d','--force-recreate');healthy();csrf=login();assert hashes()==before
        assert request('/v1/settings/tape-storage')['phase']=='ready'
        run('docker','stop',container)
        run('umount',str(mount));destination.mkdir() # Simulate the dangerous system-disk fallback directory.
        (state/'storage-services-held').unlink() # Identity guard must work independently of the initial hold.
        dc('-f',str(override),'up','-d','--force-recreate');healthy();csrf=login()
        assert request('/v1/settings/tape-storage')['phase']=='unavailable'
        result=subprocess.run(['docker','exec',container,'python3','/usr/local/libexec/storage-check.py','--service'],capture_output=True)
        assert result.returncode!=0
        for unit in ('tgt.service','vtltape@11.service','vtllibrary@10.service'):
            assert run('docker','exec',container,'systemctl','show','--property=ActiveState','--value',unit)=='inactive'
        assert list(destination.iterdir())==[], 'Fallback directory was written'
        run('mount','-o','loop',str(loop),str(mount))
        assert request('/v1/settings/tape-storage')['phase']=='unavailable', 'A stale private bind became writable after host remount'
        (state/'storage-services-held').write_text('{}\n')
        dc('-f',str(override),'up','-d','--force-recreate');healthy();csrf=login()
        assert hashes()==before;assert request('/v1/settings/tape-storage')['phase']=='ready'
        report={'image':run('docker','inspect',container,'--format','{{.Image}}'),'http':True,'apply':True,'idempotent_retry':True,'cartridge_files':len(before),'recreation_preserves_media':True,'missing_mount_blocks_services':True,'fallback_directory_empty':True,'host_remount_alone_still_blocked':True,'remount_restores_identity':True}
        (root/'result.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report),flush=True)
    finally:
        (root/'container.log').write_text(subprocess.run(['docker','logs','--tail','200',name+'-appliance-1'],capture_output=True,text=True).stdout)
        subprocess.run(['docker','compose','-f',str(base),'down'],capture_output=True)
        if controller:controller.terminate();controller.wait(timeout=15);log.close()
        subprocess.run(['umount',str(mount)],capture_output=True)
        # Evidence and disposable loop image retained for review; no production volumes touched.

if __name__=='__main__':main()
