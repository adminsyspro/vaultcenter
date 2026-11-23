#!/usr/bin/env python3
"""Install the host bridge for a NEW, unexposed appliance. Never adopts data."""
import argparse
import json
import os
from pathlib import Path
import shutil
import re
import subprocess

IMAGE_PATTERN=re.compile(r'^[a-z0-9./-]+@sha256:[0-9a-f]{64}$')

def under(path):
    """Prefix a host path with VAULTCENTER_INSTALL_ROOT (empty in real use)."""
    return Path(os.environ.get('VAULTCENTER_INSTALL_ROOT','')+path)


def run(*args): return subprocess.check_output(args,text=True).strip()

def configure_kernel_logging():
    """Persist and apply logging settings without restarting the tape stack."""
    here=Path(__file__).resolve().parent
    for name,directory in [('vaultcenter-mhvtl.conf','/etc/modprobe.d'),('99-vaultcenter-console.conf','/etc/sysctl.d')]:
        destination=under(directory)/name
        destination.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(here/name,destination)
        destination.chmod(0o644)
    opts=under('/sys/bus/mhvtl/drivers/mhvtl/opts')
    if opts.exists():opts.write_text('0\n')
    run('sysctl','--load',str(under('/etc/sysctl.d/99-vaultcenter-console.conf')))

def main():
    p=argparse.ArgumentParser();p.add_argument('--compose',required=True);p.add_argument('--new-appliance',action='store_true',required=True);p.add_argument('--isolated',action='store_true');p.add_argument('--image');args=p.parse_args()
    if args.image is not None and not IMAGE_PATTERN.match(args.image): raise SystemExit('The image must be pinned by digest (name@sha256:<64 hex digits>).')
    if args.image: os.environ['VAULTCENTER_IMAGE']=args.image
    if not os.environ.get('VAULTCENTER_INSTALL_ROOT') and os.geteuid()!=0: raise SystemExit('Run as root on the dedicated Docker host.')
    os.umask(0o077)
    source=Path(args.compose).resolve()
    config=json.loads(run('docker','compose','-f',str(source),'config','--format','json'))
    project=config['name'];container=f'{project}-appliance-1'
    if subprocess.run(['docker','inspect',container],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:raise SystemExit('An appliance already exists. Automatic conversion is not supported.')
    logical_root='/etc/vaultcenter-host';root=under(logical_root)
    if root.exists(): raise SystemExit('Host configuration already exists; preserve it and use the documented upgrade procedure.')
    volumes={}
    for key in ('state','media'):
        name=config['volumes'][key]['name'];run('docker','volume','create',name)
        volumes[key]=Path(json.loads(run('docker','volume','inspect',name))[0]['Mountpoint'])
        if any(volumes[key].iterdir()):raise SystemExit(f'The {key} volume is not empty. Refusing adoption.')
        config['volumes'][key]={'name':name,'external':True}
    configure_kernel_logging()
    root.mkdir(mode=0o700,parents=True);under('/run/vaultcenter-host').mkdir(mode=0o700,parents=True,exist_ok=True)
    binary=under('/usr/local/libexec/vaultcenter-host');binary.mkdir(parents=True,exist_ok=True)
    here=Path(__file__).resolve().parent
    for name in ('storage_controller.py','storage_paths.py','vtl_state.py','vtl_storage.py','storage_mounts.py'):shutil.copy2(here/name,binary/name)
    units=under('/etc/systemd/system');units.mkdir(parents=True,exist_ok=True)
    for unit in ('vaultcenter-storage-host.service','vaultcenter-storage-mounts.service'):shutil.copy2(here/unit,units/unit)
    # Keep a resolved immutable base; the controller only writes a media override.
    dropin=root/'storage-socket.conf'
    dropin.write_text('[Service]\nEnvironment=COLD_VTL_STORAGE_SOCKET=/run/vaultcenter-host/storage.sock\n')
    config['services']['appliance']['volumes'] += [
        {'type':'bind','source':'/run/vaultcenter-host','target':'/run/vaultcenter-host','read_only':True,'bind':{'create_host_path':False}},
        {'type':'bind','source':logical_root+'/storage-socket.conf','target':'/etc/systemd/system/cold-vtld.service.d/storage-socket.conf','read_only':True,'bind':{'create_host_path':False}}]
    base=root/'compose.json';base.write_text(json.dumps(config,indent=2)+'\n')
    host={'compose_file':logical_root+'/compose.json','project':project,'container':container,'state_dir':str(volumes['state']),'media_dir':str(volumes['media']),'bootstrap':True,'host_state':'/var/lib/vaultcenter-host','socket':'/run/vaultcenter-host/storage.sock','forbidden':[str(source.parent),logical_root]}
    if args.isolated:
        host.update(isolated=True,mount_root='/var/lib/vaultcenter-mounts',initial_media={})
        (root/'isolated-mode').write_text('{}\n')
        (volumes['state']/'storage-isolated').write_text('{}\n')
        config['services']['appliance']['volumes'] += [{'type':'bind','source':host['mount_root'],'target':'/var/lib/vaultcenter-media','bind':{'create_host_path':False,'propagation':'rslave'}}]
        base.write_text(json.dumps(config,indent=2)+'\n')
    (root/'storage.json').write_text(json.dumps(host,indent=2)+'\n')
    for marker in ('storage-managed','storage-bootstrap','storage-services-held'):(volumes['state']/marker).write_text('{}\n')
    run('systemctl','daemon-reload')
    if args.isolated:run('systemctl','enable','--now','vaultcenter-storage-mounts.service')
    run('systemctl','enable','--now','vaultcenter-storage-host.service')
    print(f'Host bridge ready. Bootstrap the administrator, then start with: docker compose -f {logical_root}/compose.json up -d')

if __name__=='__main__':main()
