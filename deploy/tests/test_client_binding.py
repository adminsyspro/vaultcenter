"""A Veeam binding is per VTL, root-validated and frozen once prepared."""
import importlib
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'cold-vtl'))
from test_vtl_controller import VtlControllerTests, A, B

class ClientBindingTests(VtlControllerTests):
    def setUp(self):
        super().setUp()
        self.c=importlib.import_module('client_binding')

    def drop_pbs_binding(self):
        del self.settings['bindings'][B]

    def test_pbs_binding_from_settings_is_read_only(self):
        value=self.c.status(self.r,A)
        self.assertEqual((value['client'],value['initiator'],value['editable']),('pbs','192.0.2.1',False))
        self.assertNotIn('key_file',json.dumps(value))
        with self.assertRaises(ValueError):self.c.save(self.r,A,0,'veeam','192.168.1.105')

    def test_veeam_binding_round_trip_and_revision(self):
        self.drop_pbs_binding()
        self.assertEqual(self.c.status(self.r,B)['client'],None)
        saved=self.c.save(self.r,B,0,'veeam','192.168.1.105')
        self.assertEqual((saved['client'],saved['initiator'],saved['revision'],saved['editable']),('veeam','192.168.1.105',1,True))
        self.assertEqual(self.c.effective(self.r,B),{'client':'veeam','initiator':'192.168.1.105'})
        with self.assertRaises(ValueError):self.c.save(self.r,B,0,'veeam','192.168.1.106')

    def test_rejects_invalid_initiator_and_client(self):
        self.drop_pbs_binding()
        for client,ip in (('veeam','not-an-ip'),('veeam','192.168.1.105 '),('pbs','192.168.1.105'),('bacula','192.168.1.105')):
            with self.subTest(client=client,ip=ip),self.assertRaises(ValueError):self.c.save(self.r,B,0,client,ip)

    def test_client_binding_locked_after_prepare(self):
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        self.r.prepare(B,1)
        with self.assertRaises(ValueError):self.c.save(self.r,B,1,'veeam','192.168.1.106')
        self.assertFalse(self.c.status(self.r,B)['editable'])

    def test_corrupt_or_foreign_file_is_refused(self):
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        path=self.root/'vtls'/B/'client.json'
        path.write_text(json.dumps({'schema':1,'vtl_id':A,'revision':1,'client':'veeam','initiator':'192.168.1.105'}))
        with self.assertRaises(ValueError):self.c.effective(self.r,B)

    def test_veeam_prepare_freezes_client_and_identity(self):
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        prepared=self.r.prepare(B,1);self.r.apply(B,prepared['token'],'admin')
        m=self.r.current(B)
        self.assertEqual((m['client'],m['target']['initiator']),('veeam','192.168.1.105'))
        self.assertNotIn('pbs',m)
        conf=(Path(m['config_root'])/'device.conf').read_text()
        for line in (' Vendor identification: HP\n',' Product identification: MSL G3 Series\n',' Product revision level: 9.50\n',
                     ' Vendor identification: HPE\n',' Product identification: Ultrium 8-SCSI\n'):
            self.assertIn(line,conf)
        self.assertNotIn('MHVTL',conf);self.assertNotIn('ULT3580',conf)

    def test_veeam_library_has_import_export_ports_for_a_media_set(self):
        # Veeam exports every tape of a media set to its own I/E port.
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        prepared=self.r.prepare(B,1);self.r.apply(B,prepared['token'],'admin')
        m=self.r.current(B)
        text=(Path(m['config_root'])/f"library_contents.{m['library']['id']}").read_text()
        self.assertEqual([l for l in text.splitlines() if l.startswith('MAP')],[f'MAP {i}:' for i in range(1,self.m.VEEAM_IE_PORTS+1)])
        self.assertEqual(sum(l.startswith('Slot ') for l in text.splitlines()),64)

    def test_existing_veeam_library_ports_are_adjusted_without_touching_slots(self):
        old='VERSION: 2\n\nDrive 1:\n\nPicker 1:\n\nMAP 1: CVT001L8\n\nSlot 1:\nSlot 2: CVT002L8\n'
        grown=self.m.set_ie_ports(old,3)
        self.assertEqual(grown,'VERSION: 2\n\nDrive 1:\n\nPicker 1:\n\nMAP 1: CVT001L8\nMAP 2:\nMAP 3:\n\nSlot 1:\nSlot 2: CVT002L8\n')
        self.assertEqual(self.m.set_ie_ports(grown,3),grown)
        self.assertEqual(self.m.set_ie_ports(grown,1),old)
        with self.assertRaises(ValueError):self.m.set_ie_ports('MAP 1:\nMAP 2: CVT003L8\n',1)
        with self.assertRaises(ValueError):self.m.set_ie_ports('MAP 1:\nMAP 3:\n',4)

    def test_pbs_library_keeps_a_single_port(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.r.current(A)
        text=(Path(m['config_root'])/f"library_contents.{m['library']['id']}").read_text()
        self.assertEqual([l for l in text.splitlines() if l.startswith('MAP')],['MAP 1:'])

    def test_veeam_serials_fit_the_hp_msl_personality(self):
        # mhVTL's HP MSL personality reports at most 10 serial characters and
        # truncates the changer's NAA designator.
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        prepared=self.r.prepare(B,1);self.r.apply(B,prepared['token'],'admin')
        m=self.r.current(B)
        for item in (m['library'],*m['drives']):self.assertRegex(item['serial'],r'^VC[0-9A-F]{8}$')
        self.assertIs(m['library'].get('naa_unverifiable'),True)
        self.assertNotIn('naa_unverifiable',m['drives'][0])

    def test_unreadable_naa_is_accepted_only_for_a_veeam_changer(self):
        library={'serial':'FZ12345678','naa':'5012345678abcdef','channel':0,'target':4,'lun':0}
        row={'path':'/dev/sg4','serial':'FZ12345678','naa':None,'address':'3:0:4:0'}
        self.assertEqual(self.r.matching_device(library,[row]),[])
        self.assertEqual(self.r.matching_device({**library,'naa_unverifiable':True},[row]),[row])
        wrong={**row,'serial':'FZ00000000'}
        self.assertEqual(self.r.matching_device({**library,'naa_unverifiable':True},[wrong]),[])
        other={**row,'naa':'50ffffffffffffff'}
        self.assertEqual(self.r.matching_device({**library,'naa_unverifiable':True},[other]),[])

    def test_pbs_manifest_records_its_scsi_identity(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.r.current(A)
        self.assertNotIn('client',m)
        self.assertEqual(m['identity'],{'library':{'vendor':'MHVTL','product':'VAULTCENTER','revision':None},
                                        'drive':{'vendor':'IBM','product':'ULT3580-TD8','revision':'HB81'}})
        self.assertEqual(m['pbs'],self.settings['bindings'][A])
        conf=(Path(m['config_root'])/'device.conf').read_text()
        library=conf.split('\n\nDrive:')[0]
        self.assertIn(' Vendor identification: MHVTL\n Product identification: VAULTCENTER\n Unit serial number: ',library)
        self.assertNotIn('Product revision level',library)
        self.assertIn(' Vendor identification: IBM\n Product identification: ULT3580-TD8\n Product revision level: HB81\n',conf)

    def test_prepare_without_any_binding_still_refuses(self):
        self.drop_pbs_binding()
        with self.assertRaisesRegex(ValueError,'binding'):self.r.prepare(B,1)

    def test_veeam_activation_never_uses_ssh(self):
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        prepared=self.r.prepare(B,1);self.r.apply(B,prepared['token'],'admin')
        with patch.object(self.r,'qualified'),patch.object(self.r,'no_workers'),\
             patch.object(self.r,'services',return_value={'u':'active'}):
            result=self.r.activate(B,'admin')
        self.assertEqual(result['phase'],'active')
        self.assertFalse([c for c in self.commands if c[0]=='ssh'])
        state=json.loads((self.root/'vtls'/B/'client-lease.json').read_text())
        self.assertEqual(state,{'vtl_id':B,'lease':None})

    def test_veeam_lease_check_is_local(self):
        self.drop_pbs_binding()
        self.c.save(self.r,B,0,'veeam','192.168.1.105')
        prepared=self.r.prepare(B,1);self.r.apply(B,prepared['token'],'admin')
        root=self.root/'vtls'/B;m=self.r.current(B)
        import client_lease
        client_lease.LocalLease(Path(m['state_root']),B).handle('acquire','00000000-0000-4000-8000-0000000000a9')
        self.m.atomic(root/'maintenance-state.json',{'phase':'maintenance','lease':'00000000-0000-4000-8000-0000000000a9'})
        self.m.atomic(root/'maintenance-block',{'lease':'00000000-0000-4000-8000-0000000000a9'})
        self.assertEqual(self.r.lease(B),'00000000-0000-4000-8000-0000000000a9')
        self.assertFalse([c for c in self.commands if c[0]=='ssh'])

if __name__=='__main__':unittest.main()
