"""Resource ownership and journals must prevent cross-VTL hardware changes."""
import importlib
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'cold-vtl'))
A='00000000-0000-4000-8000-000000000021'
B='00000000-0000-4000-8000-000000000031'

class VtlControllerTests(unittest.TestCase):
    def setUp(self):
        self.m=importlib.import_module('vtl_runtime')
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve();self.root.mkdir(exist_ok=True)
        self.db=sqlite3.connect(self.root/'catalog.db');self.addCleanup(self.db.close)
        self.db.executescript('''CREATE TABLE vtls(id TEXT PRIMARY KEY,name TEXT,lifecycle TEXT,provisioning_revision INTEGER,media_root TEXT,restore_root TEXT,device_configuration TEXT,updated_at TEXT);
        CREATE TABLE vtl_provisioning(vtl_id TEXT PRIMARY KEY,revision INTEGER,manifest TEXT,manifest_sha256 TEXT,phase TEXT,journal TEXT,updated_at TEXT);
        CREATE TABLE jobs(vtl_id TEXT,status TEXT);''')
        self.db.executemany("INSERT INTO vtls(id,name,lifecycle,provisioning_revision) VALUES(?,?,'draft',1)",[(A,'A'),(B,'B')]);self.db.commit()
        self.settings={'resources':[
            {'library_id':20,'drive_id':21,'channel':0,'library_target':2,'drive_target':3,'target_id':20},
            {'library_id':30,'drive_id':31,'channel':0,'library_target':4,'drive_target':5,'target_id':30}],
            'bindings':{v:{'pbs':'root@192.0.2.1','key_file':'/etc/cold-vtl/pbs_key','known_hosts':'/etc/cold-vtl/known_hosts','changer':'test-'+n,'drive':'drive-'+n,'initiator':'192.0.2.1'} for v,n in ((A,'a'),(B,'b'))},
            'unit_dir':str(self.root/'units')}
        self.commands=[];self.targets='';self.device_list=[]
        def runner(args):
            self.commands.append(args)
            if args[0]=='tgtadm' and '--op' in args and args[args.index('--op')+1]=='show':return self.targets
            return ''
        self.r=self.m.Registry(self.root,self.settings,runner=runner)
        self.storage=patch.object(self.r,'storage',side_effect=lambda v:{'vtl_id':v,'phase':'ready'})
        self.storage.start();self.addCleanup(self.storage.stop)
        self.devices=patch.object(self.r,'devices',side_effect=lambda **kwargs:self.device_list)
        self.devices.start();self.addCleanup(self.devices.stop)

    def test_generated_telemetry_uses_its_own_pbs_binding(self):
        for v in (A,B):
            prepared=self.r.prepare(v,1);self.r.apply(v,prepared['token'],'admin')
            import importlib.util
            spec=importlib.util.spec_from_file_location('telemetry_config',Path(__file__).parents[1]/'cold-vtl'/'telemetry.py')
            telemetry=importlib.util.module_from_spec(spec);spec.loader.exec_module(telemetry)
            config=telemetry.load_config(Path(self.r.manifest(v)['state_root'])/'telemetry.json')
            self.assertEqual(config['vtls'][0]['pbs'],self.settings['bindings'][v])
            self.assertEqual(config['vtls'][0]['vtl_id'],v)

    def test_two_preparations_reserve_distinct_resources(self):
        a=self.r.prepare(A,1);b=self.r.prepare(B,1)
        self.assertNotEqual(a['token'],b['token'])
        ma=self.r.manifest(A);mb=self.r.manifest(B)
        self.assertEqual(ma['library']['id'],20);self.assertEqual(mb['library']['id'],30)
        self.assertTrue(all(d['naa'].startswith('5') for d in [ma['library'],*ma['drives']]))
        self.assertNotEqual(ma['target']['iqn'],mb['target']['iqn'])
        self.assertEqual(ma['media_root'],'/var/lib/vaultcenter-media/'+A)
        with self.assertRaises(ValueError):self.r.apply(B,a['token'],'admin')
        self.assertEqual(list((self.root/'units').glob('*')) if (self.root/'units').exists() else [],[])

    def test_apply_is_idempotent_and_does_not_start_unqualified_services(self):
        checked=self.r.prepare(A,1);result=self.r.apply(A,checked['token'],'admin')
        self.assertEqual(result['phase'],'configured')
        again=self.r.apply(A,checked['token'],'admin');self.assertEqual(result,again)
        self.assertTrue((self.root/'vtls'/A/'config/device.conf').is_file())
        self.assertFalse(any('start' in c or 'restart' in c for c in self.commands))
        self.assertEqual(self.db.execute('SELECT lifecycle FROM vtls WHERE id=?',(A,)).fetchone()[0],'draft')

    def test_external_target_or_scsi_allocation_is_not_overwritten(self):
        self.targets='Target 20: iqn.2000-01.other:foreign\n'
        a=self.r.prepare(A,1);self.assertEqual(self.r.manifest(A)['target']['id'],30)
        self.targets+='Target 30: iqn.2000-01.other:another\n'
        with self.assertRaises(ValueError):self.r.apply(A,a['token'],'admin')
        self.assertFalse(any('--force' in c for c in self.commands))

    def test_pool_collision_and_stale_revision_are_rejected(self):
        self.settings['resources'][1]['drive_id']=21
        with self.assertRaises(ValueError):self.r.prepare(A,1)
        self.settings['resources'][1]['drive_id']=31
        with self.assertRaises(ValueError):self.r.prepare(A,2)

    def test_scsi_identity_must_match_not_just_device_number(self):
        self.r.prepare(A,1);manifest=self.r.manifest(A)
        self.device_list=[{'path':'/dev/sg7','serial':'WRONG','naa':manifest['drives'][0]['naa'],'address':'9:0:3:0'}]
        with self.assertRaises(ValueError):self.r.resolve_device(manifest['drives'][0])
        drive=manifest['drives'][0]
        self.device_list=[{'path':'/dev/sg8','serial':drive['serial'],'naa':drive['naa'],'address':'9:0:3:0'}]
        self.assertEqual(self.r.resolve_device(drive),'/dev/sg8')

    def test_changed_manifest_is_rejected_on_resume(self):
        checked=self.r.prepare(A,1)
        self.db.execute("UPDATE vtl_provisioning SET manifest='{}' WHERE vtl_id=?",(A,));self.db.commit()
        with self.assertRaises(ValueError):self.r.apply(A,checked['token'],'admin')

    def test_unqualified_runtime_cannot_publish(self):
        a=self.r.prepare(A,1);self.r.apply(A,a['token'],'admin')
        with self.assertRaises(ValueError):self.r.publish(A)
        self.assertFalse(any('--targetname' in c for c in self.commands))

    def test_new_target_accepts_full_first_burst_before_initiator_binding(self):
        for v in (A,B):
            p=self.r.prepare(v,1);self.r.apply(v,p['token'],'admin')
        with patch.object(self.r,'qualified'),patch.object(self.r,'wait_devices',return_value=['/dev/sg7','/dev/sg8']),patch.object(self.r,'verify_target'):
            self.r.publish(A)
        updates=[c for c in self.commands if c[:7]==['tgtadm','--lld','iscsi','--mode','target','--op','update']]
        parameters={'MaxRecvDataSegmentLength':'262144','MaxBurstLength':'262144',
                    'FirstBurstLength':'262144','ImmediateData':'Yes','InitialR2T':'No'}
        self.assertEqual(len(updates),len(parameters))
        self.assertEqual({c[c.index('--name')+1]:c[c.index('--value')+1] for c in updates},parameters)
        self.assertTrue(all(c[c.index('--tid')+1]=='20' for c in updates))
        bind=next(c for c in self.commands if c[:7]==['tgtadm','--lld','iscsi','--mode','target','--op','bind'])
        self.assertTrue(all(self.commands.index(c)<self.commands.index(bind) for c in updates))

    def test_withdraw_never_deletes_another_target(self):
        a=self.r.prepare(A,1);self.r.apply(A,a['token'],'admin')
        self.targets='Target 20: iqn.2000-01.other:replacement\nTarget 30: iqn.2000-01.other:b\n'
        with self.assertRaises(ValueError):self.r.withdraw(A)
        self.assertFalse(any('--force' in c for c in self.commands))

    def test_resource_appearing_after_prepare_blocks_apply(self):
        prepared=self.r.prepare(A,1)
        self.device_list=[{'path':'/dev/sg42','serial':'FOREIGN','naa':'1234567890123456','address':'9:0:3:0'}]
        with self.assertRaises(ValueError):self.r.apply(A,prepared['token'],'admin')
        self.assertFalse((self.root/'vtls'/A/'runtime.json').exists())

    def test_publication_verifies_luns_and_acl_not_only_iqn(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin');m=self.r.manifest(A)
        self.targets=f"Target 20: {m['target']['iqn']}\n    LUN information:\n        LUN: 1\n            Backing store type: sg\n            Backing store path: /dev/sg99\n    ACL information:\n        192.0.2.1\n"
        with self.assertRaises(ValueError):self.r.verify_target(m,['/dev/sg1','/dev/sg2'])

    def test_activation_failure_stops_only_a_and_keeps_lease_journal(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.r.manifest(A)
        def remote(manifest,action,lease):
            if action=='verify':raise ValueError('PBS unreachable')
            return {'ok':True,'vtl_id':A,'lease':lease,'locked':True}
        with patch.object(self.r,'qualified'),patch.object(self.r,'no_workers'),patch.object(self.r,'remote',side_effect=remote),patch.object(self.r,'services',return_value=dict.fromkeys(self.r.units(m),'active')):
            with self.assertRaises(ValueError):self.r.activate(A,'admin')
        stopped=[c[2] for c in self.commands if c[:2]==['systemctl','stop']]
        self.assertEqual(stopped,list(reversed(self.r.units(m))))
        self.assertTrue((self.root/'vtls'/A/'maintenance-block').exists())
        self.assertFalse((self.root/'vtls'/B/'maintenance-block').exists())

    def test_scoped_remote_command_cannot_be_parsed_as_ssh_options(self):
        self.r.prepare(A,1);m=self.r.manifest(A)
        def runner(args):
            self.assertEqual(args[args.index(m['pbs']['pbs'])-1],'--')
            return json.dumps({'ok':True,'vtl_id':A,'lease':'lease','locked':True})
        self.r.run=runner
        self.r.remote(m,'status','lease')

    def test_publish_waits_for_its_own_scsi_devices_to_register(self):
        self.r.prepare(A,1);m=self.r.manifest(A)
        devices=[{'path':'/dev/sg'+str(i),'serial':d['serial'],'naa':d['naa'],'address':f"9:{d['channel']}:{d['target']}:0"} for i,d in enumerate([*m['drives'],m['library']],7)]
        with patch.object(self.r,'devices',side_effect=[[],devices]),patch.object(self.m.time,'sleep'):
            self.assertEqual(self.r.wait_devices(m),['/dev/sg7','/dev/sg8'])

    def test_partial_publication_recovers_only_with_matching_journal_and_lease(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin');m=self.r.manifest(A)
        root=Path(m['state_root']);devices=['/dev/sg7','/dev/sg8']
        self.m.atomic(root/'publication.json',{'phase':'creating','devices':devices,'target':m['target']})
        self.m.atomic(root/'maintenance-state.json',{'phase':'leaving','lease':'owned'})
        self.targets=f"Target 20: {m['target']['iqn']}\n    LUN information:\n        LUN: 0\n            Backing store type: null\n    ACL information:\n"
        with patch.object(self.r,'qualified'),patch.object(self.r,'wait_devices',return_value=devices),patch.object(self.r,'remote',return_value={'locked':True,'lease':'owned'}),patch.object(self.r,'verify_target'):
            self.r.publish(A)
        self.assertTrue(any('--op' in c and 'delete' in c and c[c.index('--tid')+1]=='20' for c in self.commands))
        self.assertFalse(any('--tid' in c and c[c.index('--tid')+1]=='30' for c in self.commands))

    def test_partial_publication_rejects_foreign_lun_before_cleanup(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin');m=self.r.manifest(A)
        root=Path(m['state_root']);devices=['/dev/sg7','/dev/sg8']
        self.m.atomic(root/'publication.json',{'phase':'creating','devices':devices,'target':m['target']})
        self.m.atomic(root/'maintenance-state.json',{'phase':'leaving','lease':'owned'})
        self.targets=f"Target 20: {m['target']['iqn']}\n    LUN information:\n        LUN: 0\n            Backing store type: null\n        LUN: 1\n            Backing store type: sg\n            Backing store path: /dev/sg99\n    ACL information:\n"
        with patch.object(self.r,'qualified'),patch.object(self.r,'wait_devices',return_value=devices),patch.object(self.r,'remote',return_value={'locked':True,'lease':'owned'}):
            with self.assertRaises(ValueError):self.r.publish(A)
        self.assertFalse(any('delete' in c for c in self.commands))

    def test_applying_intent_remains_visible_without_resolved_runtime(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin')
        self.db.execute("UPDATE vtl_provisioning SET phase='applying' WHERE vtl_id=?",(A,));self.db.commit()
        self.assertEqual(self.r.status(A)['phase'],'applying')

    def test_catalogue_active_with_unfinished_journal_exposes_activation_resume(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin');m=self.r.manifest(A)
        self.db.execute("UPDATE vtl_provisioning SET phase='active' WHERE vtl_id=?",(A,));self.db.commit()
        self.m.atomic(Path(m['state_root'])/'activation.json',{'vtl_id':A,'revision':2,'phase':'verified','lease':'owned'})
        with patch.object(self.r,'services',return_value={}):
            self.assertEqual(self.r.status(A)['phase'],'activation_incomplete')

    def test_catalogue_becomes_active_only_after_lease_finalization(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin');m=self.r.manifest(A)
        def remote(manifest,action,lease):
            if action=='release':raise ValueError('release unavailable')
            return {'ok':True,'vtl_id':A,'locked':True,'lease':lease}
        with patch.object(self.r,'qualified'),patch.object(self.r,'no_workers'),patch.object(self.r,'remote',side_effect=remote),patch.object(self.r,'services',return_value=dict.fromkeys(self.r.units(m),'active')):
            with self.assertRaises(ValueError):self.r.activate(A,'admin')
        self.assertEqual(self.db.execute('SELECT phase FROM vtl_provisioning WHERE vtl_id=?',(A,)).fetchone()[0],'configured')
        self.assertEqual(self.db.execute('SELECT lifecycle FROM vtls WHERE id=?',(A,)).fetchone()[0],'draft')

    def test_verified_activation_resumes_stopped_services_after_restart(self):
        p=self.r.prepare(A,1);self.r.apply(A,p['token'],'admin');m=self.r.manifest(A);root=Path(m['state_root'])
        self.m.atomic(root/'activation.json',{'vtl_id':A,'revision':2,'phase':'verified','lease':'owned'})
        self.m.atomic(root/'ever-published',{'vtl_id':A})
        states=dict.fromkeys(self.r.units(m),'inactive')
        def runner(args):
            if args[:2]==['systemctl','start'] and args[2] in states:states[args[2]]='active'
            return ''
        self.r.run=runner
        with patch.object(self.r,'qualified'),patch.object(self.r,'no_workers'),patch.object(self.r,'services',side_effect=lambda v:states.copy()),patch.object(self.r,'remote',side_effect=lambda m,a,l:{'locked':True,'lease':l}):
            self.assertEqual(self.r.activate(A,'admin')['phase'],'active')
        self.assertEqual(set(states.values()),{'active'})

if __name__=='__main__':unittest.main()
