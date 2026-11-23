#!/usr/bin/env python3
"""Journalled mode switch for an unused appliance; used media need dedicated migration."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
from vtl_state import atomic,file_lock

LEGACY='00000000-0000-4000-8000-000000000001'

def fingerprint(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def catalogue(path):
    with closing(sqlite3.connect(path)) as db:
        return hashlib.sha256('\n'.join(db.iterdump()).encode()).hexdigest()

class Migration:
    def __init__(self,config,stopped=None):
        self.config_path=Path(config);self.config=json.loads(self.config_path.read_text())
        self.state=Path(self.config['state_dir']);self.host=Path(self.config['host_state'])
        self.stopped=stopped or self.container_stopped

    def container_stopped(self):
        value=subprocess.check_output(['docker','inspect','--format','{{.State.Running}}',self.config['container']],text=True,timeout=10)
        return value.strip()=='false'

    def unused(self):
        if not self.stopped():raise ValueError('Stop this appliance before preparing or applying its migration.')
        media=Path(self.config['media_dir'])
        if media.is_symlink() or not media.is_dir() or any(media.iterdir()):raise ValueError('Existing tapes: dedicated migration required, no file moved.')
        if list(self.state.glob('vtls/*/data-locked')):raise ValueError('Existing data lock: automatic migration not allowed.')
        if any((self.state/name).exists() for name in ('maintenance-block','cartridge-creation-pending.json','cartridge-deletion-pending.json','reintegration-pending.json')):
            raise ValueError('Finish the previous operations before migration.')
        with closing(sqlite3.connect(self.state/'catalog.db')) as db:
            if db.execute('SELECT 1 FROM cartridges LIMIT 1').fetchone():raise ValueError('Registered cartridges: dedicated migration required.')
            if db.execute("SELECT 1 FROM jobs WHERE status IN ('queued','running') LIMIT 1").fetchone():raise ValueError('Active jobs: migration refused.')
        old=self.host/'storage.json'
        if old.exists() and json.loads(old.read_text()).get('path'):raise ValueError('Storage already configured: dedicated migration required.')

    def prepare(self,backup,image):
        if not re.fullmatch(r'sha256:[0-9a-f]{64}',image):raise ValueError('Provide the immutable digest of the verified image.')
        if self.config.get('isolated'):raise ValueError('Isolated mode already configured.')
        self.unused();backup=Path(backup).resolve();backup.mkdir(mode=0o700,parents=True,exist_ok=False)
        database=self.state/'catalog.db'
        with closing(sqlite3.connect(database)) as source,closing(sqlite3.connect(backup/'catalog.db')) as dest:source.backup(dest)
        config_backup=backup/'storage.json';config_backup.write_bytes(self.config_path.read_bytes())
        compose=Path(self.config['compose_file']);(backup/'compose.json').write_bytes(compose.read_bytes())
        with closing(sqlite3.connect(backup/'catalog.db')) as source,closing(sqlite3.connect(backup/'expected-catalog.db')) as dest:
            source.backup(dest)
            with dest:dest.execute("UPDATE vtls SET lifecycle='draft',media_root=NULL,restore_root=NULL WHERE id=?",(LEGACY,))
        manifest={'expected_database_hash':catalogue(backup/'expected-catalog.db'),'version':1,'phase':'prepared','config':str(self.config_path),'backup':str(backup),'image':image,
                  'database_hash':catalogue(database),'config_hash':fingerprint(self.config_path),'compose_hash':fingerprint(compose)}
        path=backup/'manifest.json';atomic(path,manifest);return path

    def apply(self,path):
        path=Path(path);manifest=json.loads(path.read_text());backup=Path(manifest['backup'])
        if manifest['config']!=str(self.config_path):raise ValueError('Manifest belongs to another appliance.')
        original=json.loads((backup/'storage.json').read_text())
        if fingerprint(backup/'storage.json')!=manifest['config_hash'] or fingerprint(backup/'compose.json')!=manifest['compose_hash'] or catalogue(backup/'catalog.db')!=manifest['database_hash']:
            raise ValueError('Backup modified: resume refused.')
        self.config=original
        with file_lock(self.host/'migration.lock'):
            self.unused()
            if manifest['phase']=='complete':
                if not (self.state/'storage-isolated').exists():raise ValueError('Migration finished but marker missing.')
                return manifest
            compose=Path(original['compose_file'])
            if manifest['phase']=='prepared':
                if fingerprint(self.config_path)!=manifest['config_hash'] or fingerprint(compose)!=manifest['compose_hash'] or catalogue(self.state/'catalog.db')!=manifest['database_hash']:
                    raise ValueError('Installation modified since preparation.')
                manifest['phase']='applying';atomic(path,manifest)
            if catalogue(self.state/'catalog.db') not in (manifest['database_hash'],manifest['expected_database_hash']):raise ValueError('Catalogue modified during the migration.')
            # Every following write is deterministic and recoverable from this journal.
            with closing(sqlite3.connect(self.state/'catalog.db')) as db:
                with db:db.execute("UPDATE vtls SET lifecycle='draft',media_root=NULL,restore_root=NULL WHERE id=?",(LEGACY,))
            target=json.loads((backup/'compose.json').read_text())
            mount_root=original.get('mount_root','/var/lib/vaultcenter-mounts')
            target['services']['appliance']['image']=manifest['image']
            volumes=target['services']['appliance']['volumes']
            volumes[:]=[v for v in volumes if not isinstance(v,dict) or v.get('target')!='/var/lib/vaultcenter-media']
            volumes.append({'type':'bind','source':mount_root,'target':'/var/lib/vaultcenter-media','bind':{'create_host_path':False,'propagation':'rslave'}})
            atomic(compose,target)
            config={**original,'isolated':True,'mount_root':mount_root,'initial_media':{}}
            atomic(self.config_path,config)
            atomic(self.config_path.parent/'isolated-mode',{'version':1})
            atomic(self.state/'storage-isolated',{'version':1})
            manifest['phase']='complete';atomic(path,manifest)
            return manifest


def main():
    parser=argparse.ArgumentParser();parser.add_argument('--config',default='/etc/vaultcenter-host/storage.json')
    mode=parser.add_mutually_exclusive_group(required=True);mode.add_argument('--prepare',type=Path);mode.add_argument('--apply',type=Path);mode.add_argument('--recover',type=Path)
    parser.add_argument('--image');args=parser.parse_args();os.umask(0o077)
    if os.geteuid()!=0:raise ValueError('Run on the host VM as root.')
    migration=Migration(args.config)
    if args.prepare:print(migration.prepare(args.prepare,args.image or ''))
    else:print(json.dumps(migration.apply(args.apply or args.recover)))

if __name__=='__main__':main()
