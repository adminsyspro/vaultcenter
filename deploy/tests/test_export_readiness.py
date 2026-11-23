import tempfile
import os
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch
from test_maintenance import module

adapter = module('export_readiness', 'cold-vtl/export_readiness.py')

class ExportReadinessTests(unittest.TestCase):
    def test_running_binary_is_checked_even_if_installed_one_is_new(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            installed = root/'libvtlscsi.so'; installed.write_bytes(b'ELF VAULTCENTER_MEDIA_LOCK_V1')
            running = root/'proc'/'123'; running.mkdir(parents=True)
            (running/'maps').write_text('100-200 r-xp 0 00:00 999 /usr/lib/libvtlscsi.so\n')
            instance = SimpleNamespace(scoped=True, vtl_id='vtl-a', root=root,
                controller=SimpleNamespace(config={'runtime':{'drives':[{'id':11}]}},
                    block=root/'maintenance-block', read=lambda:{'phase':'online'},
                    services=lambda:{'library':'active','drive':'active'},
                    run=Mock(return_value='123\n')),
                installed=Mock())
            with patch.object(adapter, 'ENGINE', installed), patch.object(adapter, 'PROC', root/'proc'):
                with self.assertRaisesRegex(RuntimeError, 'engine'):
                    adapter.check(instance, {'vtl_id':'vtl-a'})
                info=installed.stat()
                (running/'maps').write_text(f'100-200 r-xp 0 {os.major(info.st_dev):x}:{os.minor(info.st_dev):x} {info.st_ino} /usr/lib/libvtlscsi.so\n')
                self.assertEqual(adapter.check(instance, {'vtl_id':'vtl-a'}),
                                 {'vtl_id':'vtl-a','media_lock_version':1})
                (root/'maintenance-block').touch()
                with self.assertRaises(RuntimeError): adapter.check(instance, {'vtl_id':'vtl-a'})
                (root/'maintenance-block').unlink()
                replacement=root/'new'; replacement.write_bytes(installed.read_bytes()); replacement.replace(installed)
                with self.assertRaisesRegex(RuntimeError, 'engine'):
                    adapter.check(instance, {'vtl_id':'vtl-a'})

    def test_absent_or_stopped_engine_never_claims_online_protection(self):
        with tempfile.TemporaryDirectory() as tmp:
            instance=SimpleNamespace(scoped=True,vtl_id='a',root=Path(tmp),installed=Mock(),
                controller=SimpleNamespace(read=lambda:{'phase':'maintenance'}))
            with self.assertRaises(RuntimeError): adapter.check(instance, {'vtl_id':'a'})
