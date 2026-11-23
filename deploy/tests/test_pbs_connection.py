"""Connection instructions use verified per-library identities and explicit portals."""
import importlib
import json
from pathlib import Path
import unittest
import os
import subprocess
import sys

from test_vtl_controller import VtlControllerTests, A, B


class PbsConnectionTests(VtlControllerTests):
    def connection(self):
        self.assertIsNotNone(importlib.util.find_spec('pbs_connection'), 'PBS connection instructions are not implemented')
        self.pbs = importlib.import_module('pbs_connection')
        prepared = self.r.prepare(A, 1)
        self.r.apply(A, prepared['token'], 'admin')
        return self.pbs

    def test_ids_come_from_manifest(self):
        m = self.connection()
        result = m.save_portal(self.r, A, 0, '192.0.2.99', 13260)
        manifest = self.r.current(A)
        self.assertEqual(result['iqn'], 'iqn.2026-10.io.vaultcenter:vtl-' + A)
        self.assertEqual(result['drive']['serial'], manifest['drives'][0]['serial'])
        self.assertEqual(result['drive']['name'], 'drive-a')
        self.assertTrue(result['drive']['path'].endswith('-sg'))
        self.assertEqual(result['phase'], 'ready')
        self.assertEqual(len(result['steps']), 3)
        self.assertNotIn('key_file', json.dumps(result))
        self.assertNotIn('/etc/cold-vtl/pbs_key', json.dumps(result))

    def test_ipv6_and_shell_metacharacters(self):
        m = self.connection()
        for host in ('$(touch /tmp/unwanted)', 'host\nreboot', '-evil', 'a;true', 'x:3260', ''):
            with self.subTest(host=host), self.assertRaises(ValueError):
                m.save_portal(self.r, A, 0, host, 3260)
        result = m.save_portal(self.r, A, 0, '2001:db8::1', 3260)
        self.assertIn('[2001:db8::1]:3260', result['steps'][0]['command'])
        for port in (0, 65536, True):
            with self.assertRaises(ValueError):
                m.save_portal(self.r, A, 1, 'pbs.example.test', port)

    def test_portal_revision_conflict(self):
        m = self.connection()
        m.save_portal(self.r, A, 0, '192.0.2.99', 13260)
        with self.assertRaises(ValueError):
            m.save_portal(self.r, A, 0, '192.0.2.100', 3260)
        self.assertEqual(m.connection_info(self.r, A)['host'], '192.0.2.99')
        self.assertFalse((self.root/'vtls'/B/'pbs-connection.json').exists())

    def test_missing_binding_exposes_no_secrets(self):
        m = self.connection()
        result = m.connection_info(self.r, B)
        self.assertEqual(result['phase'], 'missing_prerequisite')
        self.assertEqual(result['steps'], [])
        self.assertNotIn('known_hosts', json.dumps(result))

    def test_modified_manifest_has_no_commands(self):
        m = self.connection()
        path = self.root/'vtls'/A/'runtime.json'
        value = json.loads(path.read_text())
        value['target']['iqn'] = 'iqn.foreign'
        path.write_text(json.dumps(value))
        result = m.connection_info(self.r, A)
        self.assertEqual(result['steps'], [])
        self.assertEqual(result['phase'], 'missing_prerequisite')

    def script_tools(self, result, entries=None):
        tools = self.root/'tools'
        tools.mkdir()
        state = self.root/'commands.json'
        state.write_text(json.dumps(dict(iqn=result['iqn'], portal='192.0.2.99:13260', calls=[],
                                        connected=False, node=False, changer=entries or [], drive=[])))
        source = '''import json,os,sys
from pathlib import Path
file=Path(os.environ['VAULTCENTER_TEST_STATE']);s=json.loads(file.read_text())
tool=Path(sys.argv[0]).name;a=sys.argv[1:];s['calls'].append([tool,*a]);status=0;out=''
if tool=='iscsiadm':
 if a[:2]==['-m','session']:
  if s['connected']:out='tcp: [1] '+s['portal']+',1 '+s['iqn']+' (non-flash)'
  else:status=21
 elif a[-2:]==['-o','show']:status=0 if s['node'] else 21
 elif a[-2:]==['-o','new']:s['node']=True
 elif a[-1:] == ['--login']:s['connected']=True
 elif '-o' in a and a[a.index('-o')+1]=='update':pass
 else:status=91
elif tool=='sg_inq':
 out=('SCSI_IDENT_SERIAL='+Path(a[-1]).name.removeprefix('scsi-').removesuffix('-sg')) if '--export' in a else 'VPD INQUIRY, page code=0x80: 08 80 00 0e 46 5a'
elif tool=='proxmox-tape':
 if a[1]=='list':out=json.dumps(s[a[0]])
 elif a[1]=='create':
  item={'name':a[2],'path':a[a.index('--path')+1]}
  if a[0]=='drive':item.update(changer=a[a.index('--changer')+1],**{'changer-drivenum':0})
  s[a[0]].append(item)
 else:status=91
file.write_text(json.dumps(s));print(out);sys.exit(status)
'''
        for name in ('iscsiadm','proxmox-tape','sg_inq'):
            file=tools/name;file.write_text('#!'+sys.executable+'\n'+source);file.chmod(0o700)
        sudo=tools/'sudo';sudo.write_text('#!/bin/sh\nexec "$@"\n');sudo.chmod(0o700)
        env=dict(os.environ,PATH=str(tools)+os.pathsep+os.environ['PATH'],VAULTCENTER_TEST_STATE=str(state))
        return state,env

    def test_active_session_has_no_logout_and_registration_repeats(self):
        m=self.connection();result=m.save_portal(self.r,A,0,'192.0.2.99',13260)
        state,env=self.script_tools(result)
        for _ in range(2):
            for step in result['steps'][:2]:
                done=subprocess.run(['sh','-c',step['command']],env=env,capture_output=True,text=True,timeout=5)
                self.assertEqual(done.returncode,0,done.stderr)
        calls=json.loads(state.read_text())['calls']
        self.assertEqual(sum('--login' in c for c in calls),1)
        self.assertEqual(sum('create' in c for c in calls),2)
        self.assertFalse(any('--logout' in c or '--loginall' in c or 'delete' in c for c in calls))
        nodes=[c for c in calls if c[:3]==['iscsiadm','-m','node']]
        self.assertTrue(all(result['iqn'] in c and '192.0.2.99:13260' in c for c in nodes))

    def test_foreign_pbs_entry_is_not_overwritten(self):
        m=self.connection();result=m.save_portal(self.r,A,0,'192.0.2.99',13260)
        state,env=self.script_tools(result,[{'name':'test-a','path':'/dev/foreign'}])
        done=subprocess.run(['sh','-c',result['steps'][1]['command']],env=env,capture_output=True,text=True,timeout=5)
        self.assertNotEqual(done.returncode,0)
        self.assertIn('another device',done.stderr)
        self.assertFalse(any('create' in c for c in json.loads(state.read_text())['calls']))

    def test_veeam_steps_are_windows_powershell(self):
        import client_binding
        pbs = importlib.import_module('pbs_connection')
        del self.settings['bindings'][B]
        client_binding.save(self.r, B, 0, 'veeam', '192.168.1.105')
        prepared = self.r.prepare(B, 1)
        self.r.apply(B, prepared['token'], 'admin')
        info = pbs.save_portal(self.r, B, 0, '192.168.1.110', 3260)
        self.assertEqual((info['client'], info['pbs'], info['phase']), ('veeam', None, 'ready'))
        self.assertEqual([s['id'] for s in info['steps']], ['iscsi', 'devices'])
        command = info['steps'][0]['command']
        self.assertIn("$portal = '192.168.1.110'; $port = 3260; $iqn = 'iqn.2026-10.io.vaultcenter:vtl-" + B + "'", command)
        self.assertIn("New-IscsiTargetPortal -TargetPortalAddress $portal -TargetPortalPortNumber $port", command)
        self.assertIn("Connect-IscsiTarget -NodeAddress $iqn", command)
        self.assertIn('Update-IscsiTargetPortal -TargetPortalAddress $portal -TargetPortalPortNumber $port', info['steps'][0]['command'])
        self.assertNotIn('Update-IscsiTarget |', info['steps'][0]['command'])
        self.assertNotIn('proxmox-tape', json.dumps(info))
        self.assertNotIn('key_file', json.dumps(info))
        self.assertIn('Inventory', info['message'])

    def test_draft_veeam_vtl_reports_veeam_before_manifest(self):
        import client_binding
        m = self.connection()
        del self.settings['bindings'][B]
        self.assertEqual(m.connection_info(self.r, B)['client'], 'pbs')
        client_binding.save(self.r, B, 0, 'veeam', '192.168.1.105')
        info = m.connection_info(self.r, B)
        self.assertEqual((info['client'], info['phase']), ('veeam', 'missing_prerequisite'))
        (self.root/'vtls'/B/'client.json').write_text('{')
        self.assertEqual(m.connection_info(self.r, B)['client'], 'pbs')

    def test_portal_rejects_scope_ids_and_unicode_quotes(self):
        pbs = importlib.import_module('pbs_connection')
        for host in ("fe80::1%x';calc;$(a)", 'fe80::1%eth0', 'host\u2019name'):
            with self.subTest(host=host), self.assertRaises(ValueError):
                pbs.portal(host, 3260)

    def test_windows_steps_reject_unexpected_values(self):
        pbs = importlib.import_module('pbs_connection')
        with self.assertRaises(ValueError):
            pbs.render_windows_steps(dict(iqn="iqn.2026-10.io.vaultcenter:vtl-'" + B[1:]), '10.0.0.1', 3260)
        with self.assertRaises(ValueError):
            pbs.render_windows_steps(dict(iqn='iqn.2026-10.io.vaultcenter:vtl-' + B), '10.0.0.1\u2019', 3260)

    def legacy(self, vtl_id):
        """A VTL provisioned before the rename: another IQN prefix and FZ serials."""
        m = self.r.manifest(vtl_id)
        m['target']['iqn'] = 'iqn.2005-03.com.example:vtl-' + vtl_id
        for device in [m['library'], *m['drives']]:
            device['serial'] = 'FZ' + device['serial'][2:]
        self.db.execute('UPDATE vtl_provisioning SET manifest=?,manifest_sha256=? WHERE vtl_id=?', (json.dumps(m), self.r_module().digest(m), vtl_id))
        self.db.commit()
        self.r_module().atomic(Path(m['state_root'])/'runtime.json', m)
        self.device_list = [{'path': f'/dev/sg{n}', 'serial': d['serial'], 'naa': d['naa'], 'address': f"9:{d['channel']}:{d['target']}:{d['lun']}"}
                            for n, d in enumerate([m['library'], *m['drives']])]
        return m

    def r_module(self):
        return importlib.import_module('vtl_runtime')

    def test_existing_vtl_keeps_its_iqn_and_serials(self):
        import client_binding
        m = self.connection()
        self.legacy(A)
        result = m.save_portal(self.r, A, 0, '192.0.2.99', 13260)
        self.assertEqual((result['iqn'], result['phase']), ('iqn.2005-03.com.example:vtl-' + A, 'ready'))
        self.assertIn('iqn.2005-03.com.example:vtl-' + A, json.dumps(result['steps']))
        self.assertTrue(result['drive']['serial'].startswith('FZ'))
        del self.settings['bindings'][B]
        client_binding.save(self.r, B, 0, 'veeam', '192.168.1.105')
        prepared = self.r.prepare(B, 1)
        self.r.apply(B, prepared['token'], 'admin')
        self.legacy(B)
        info = m.save_portal(self.r, B, 0, '192.168.1.110', 3260)
        self.assertIn("$iqn = 'iqn.2005-03.com.example:vtl-" + B + "'", info['steps'][0]['command'])

    def test_foreign_or_unsafe_iqn_is_refused(self):
        m = self.connection()
        manifest = self.legacy(A)
        manifest['target']['iqn'] = 'iqn.2005-03.com.example:vtl-' + B
        self.db.execute('UPDATE vtl_provisioning SET manifest=?,manifest_sha256=? WHERE vtl_id=?', (json.dumps(manifest), self.r_module().digest(manifest), A))
        self.db.commit()
        self.r_module().atomic(Path(manifest['state_root'])/'runtime.json', manifest)
        with self.assertRaises(ValueError):
            m.save_portal(self.r, A, 0, '192.0.2.99', 13260)
        for iqn in ("iqn.2005-03.com.ex'ample:vtl-" + B, 'iqn.2005-03.com.ex$ample:vtl-' + B,
                    'iqn.2005-03.com.ex`ample:vtl-' + B, 'iqn.2005-03.com.Example:vtl-' + B):
            with self.subTest(iqn=iqn), self.assertRaises(ValueError):
                m.render_windows_steps(dict(iqn=iqn), '10.0.0.1', 3260)
