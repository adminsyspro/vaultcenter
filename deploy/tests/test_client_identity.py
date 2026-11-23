"""PBS and Veeam see a VTL through its iSCSI name and SCSI identity: new VTLs
take them from the installation settings, existing VTLs keep theirs."""
import json
from pathlib import Path
import re
import sys
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parent))
import test_vtl_controller as base

A,B=base.A,base.B

LEGACY_IQN='iqn.2005-03.com.example:vtl-'


class ClientIdentityTests(base.VtlControllerTests):
    def legacy(self,vtl_id,product='LEGACYVTL'):
        """Rewrite a configured VTL as if provisioned before the settings existed."""
        m=self.r.manifest(vtl_id)
        m['target']['iqn']=LEGACY_IQN+vtl_id
        m['identity']['library']['product']=product
        for device in [m['library'],*m['drives']]:device['serial']='FZ'+device['serial'][2:]
        self.db.execute('UPDATE vtl_provisioning SET manifest=?,manifest_sha256=? WHERE vtl_id=?',(json.dumps(m),self.m.digest(m),vtl_id))
        self.db.commit()
        self.m.atomic(Path(m['state_root'])/'runtime.json',m)
        self.device_list=[{'path':f'/dev/sg{n}','serial':d['serial'],'naa':d['naa'],'address':f"9:{d['channel']}:{d['target']}:{d['lun']}"}
                          for n,d in enumerate([m['library'],*m['drives']])]
        return m

    def device_conf(self,vtl_id):
        return (self.root/'vtls'/vtl_id/'config/device.conf').read_text()

    def test_new_pbs_vtl_gets_the_default_identity(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.r.manifest(A)
        self.assertEqual(m['target']['iqn'],'iqn.2026-10.io.vaultcenter:vtl-'+A)
        self.assertEqual(m['identity']['library'],{'vendor':'MHVTL','product':'VAULTCENTER','revision':None})
        self.assertEqual(m['identity']['drive'],{'vendor':'IBM','product':'ULT3580-TD8','revision':'HB81'})
        for device in [m['library'],*m['drives']]:self.assertRegex(device['serial'],r'^VC[0-9A-F]{12}$')
        self.assertIn(' Product identification: VAULTCENTER\n',self.device_conf(A))

    def test_settings_choose_the_identity_of_new_vtls(self):
        self.settings.update(iqn_prefix='iqn.2005-03.com.example',scsi_product='ACMEVTL')
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.r.manifest(A)
        self.assertEqual(m['target']['iqn'],'iqn.2005-03.com.example:vtl-'+A)
        self.assertEqual(m['identity']['library']['product'],'ACMEVTL')
        self.assertIn(' Product identification: ACMEVTL\n',self.device_conf(A))

    def test_invalid_settings_refuse_preparation(self):
        cases=[('iqn_prefix','iqn.2026-10.com.Example'),('iqn_prefix',"iqn.2026-10.com.ex'ample"),
               ('iqn_prefix','eui.0123'),('iqn_prefix','iqn.2026-13.com.example'),('iqn_prefix','iqn.2026-10.'+'a'*200),
               ('scsi_product','A'*17),('scsi_product','ACME VTL'),('scsi_product',''),('scsi_product',42)]
        for key,value in cases:
            with self.subTest(key=key,value=value):
                self.settings.pop('iqn_prefix',None);self.settings.pop('scsi_product',None)
                self.settings[key]=value
                with self.assertRaises(ValueError):self.r.prepare(A,1)
                self.assertEqual(self.db.execute('SELECT COUNT(*) FROM vtl_provisioning').fetchone()[0],0)

    def test_manifest_without_identity_is_refused(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        before=self.device_conf(A)
        m=self.r.manifest(A);del m['identity']
        with self.assertRaisesRegex(ValueError,'migration'):self.r.write_configuration(m)
        self.assertEqual(self.device_conf(A),before)

    def test_existing_identity_is_kept(self):
        prepared=self.r.prepare(A,1);self.r.apply(A,prepared['token'],'admin')
        m=self.legacy(A)
        self.settings.update(iqn_prefix='iqn.2026-10.io.other',scsi_product='OTHER')
        self.r.write_configuration(self.r.manifest(A))
        text=self.device_conf(A)
        self.assertIn(' Product identification: LEGACYVTL\n',text)
        self.assertNotIn('OTHER',text)
        for device in [m['library'],*m['drives']]:self.assertIn(f" Unit serial number: {device['serial']}\n",text)
        self.assertEqual(self.r.manifest(A)['target']['iqn'],LEGACY_IQN+A)

    def test_storage_idle_sees_a_target_of_any_prefix(self):
        self.targets=f'Target 30: {LEGACY_IQN}{B}\n'
        with self.assertRaises(ValueError):self.r.storage_idle(B)
        self.targets=f'Target 30: {LEGACY_IQN}{A}\n'
        self.r.storage_idle(B)


def load_tests(loader,tests,pattern):
    # Only this class's tests: the inherited ones already run in test_vtl_controller.
    return unittest.TestSuite(ClientIdentityTests(name) for name in vars(ClientIdentityTests) if name.startswith('test_'))


if __name__=='__main__':
    unittest.main()
