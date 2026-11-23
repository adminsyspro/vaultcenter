"""Persistent rate settings must never silently become unlimited or cross VTLs."""
import importlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'cold-vtl'))
A = '00000000-0000-4000-8000-000000000021'
B = '00000000-0000-4000-8000-000000000031'


class BandwidthTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(importlib.util.find_spec('bandwidth'), 'Bandwidth configuration is not implemented')
        self.m = importlib.import_module('bandwidth')
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.m.initialize_bandwidth(self.root, A)

    def test_revision_conflict_preserves_both_limits(self):
        self.assertEqual(self.m.read_bandwidth(self.root, A)['revision'], 1)
        self.m.update_bandwidth(self.root, A, 1, 50_000_000, 100_000_000)
        with self.assertRaises(ValueError):
            self.m.update_bandwidth(self.root, A, 1, None, None)
        saved = self.m.read_bandwidth(self.root, A)
        self.assertEqual((saved['revision'], saved['tape_write_bps'], saved['s3_bps']), (2, 50_000_000, 100_000_000))
        self.assertEqual((self.root / 'vtls' / A / 'config/bandwidth-drive.conf').read_text(),
                         f'VAULTCENTER_BW1 {A} 2 50000000\n')

    def test_missing_after_migration_is_not_unlimited(self):
        (self.root / 'vtls' / A / 'bandwidth.json').unlink()
        with self.assertRaises(ValueError):
            self.m.initialize_bandwidth(self.root, A)
        with self.assertRaises(ValueError):
            self.m.read_bandwidth(self.root, A)

    def test_b_scope_unchanged(self):
        self.m.initialize_bandwidth(self.root, B)
        before = (self.root / 'vtls' / B / 'bandwidth.json').read_bytes()
        self.m.update_bandwidth(self.root, A, 1, 50_000_000, 100_000_000)
        self.assertEqual((self.root / 'vtls' / B / 'bandwidth.json').read_bytes(), before)
        self.assertIsNone(self.m.read_bandwidth(self.root, B)['s3_bps'])

    def test_projection_failure_reports_pending(self):
        with patch.object(self.m, 'atomic_text', side_effect=OSError('read only projection')):
            result = self.m.update_bandwidth(self.root, A, 1, 50_000_000, None)
        self.assertEqual(result['application'], 'pending')
        self.assertEqual(self.m.read_bandwidth(self.root, A)['tape_write_bps'], 50_000_000)
        self.m.reconcile_bandwidth(self.root, A)
        self.assertEqual((self.root / 'vtls' / A / 'config/bandwidth-drive.conf').read_text(),
                         f'VAULTCENTER_BW1 {A} 2 50000000\n')

    def test_invalid_rates_do_not_mutate(self):
        for value in (0, -1, True, 1.5, 1_000_001, 100_001_000_000, '1000000'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.m.update_bandwidth(self.root, A, 1, value, None)
        self.assertEqual(self.m.read_bandwidth(self.root, A)['revision'], 1)

    def test_corrupt_or_foreign_configuration_is_not_adopted(self):
        file = self.root / 'vtls' / A / 'bandwidth.json'
        value = json.loads(file.read_text())
        value['vtl_id'] = B
        file.write_text(json.dumps(value))
        with self.assertRaises(ValueError):
            self.m.initialize_bandwidth(self.root, A)

    def test_symlink_directory_is_rejected(self):
        (self.root / 'vtls' / B).symlink_to(self.root / 'vtls' / A, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.m.initialize_bandwidth(self.root, B)

    def test_reinitialization_preserves_limits(self):
        self.m.update_bandwidth(self.root, A, 1, None, 1_000_000)
        result = self.m.initialize_bandwidth(self.root, A)
        self.assertEqual((result['revision'], result['s3_bps']), (2, 1_000_000))

    def test_missing_read_does_not_initialize_draft(self):
        with self.assertRaises(ValueError):
            self.m.read_bandwidth(self.root, B)
        self.assertFalse((self.root / 'vtls' / B / 'bandwidth.json').exists())

    def test_confirmation_requires_live_process_and_matching_revision(self):
        value=self.m.read_bandwidth(self.root,A)
        sample={'version':1,'vtl_id':A,'generated_at':10000,'drives':[{'bandwidth_revision':1,
            'tape_write_limit_bps':0,'bandwidth_error':False,'bandwidth_observed_at':9900,
            'bandwidth_pid':123,'bandwidth_start':'456'}]}
        self.assertEqual(self.m.tape_observation(value,sample,10000,(123,'456'))['state'],'confirmed')
        self.assertEqual(self.m.tape_observation(value,sample,10000,(124,'457'))['state'],'pending')
        self.assertEqual(self.m.tape_observation(value,sample,30000,(123,'456'))['state'],'pending')
        sample['drives'][0]['bandwidth_revision']=2
        self.assertEqual(self.m.tape_observation(value,sample,10000,(123,'456'))['state'],'pending')

    def test_parallel_host_queue_required_before_enabling_tape_limit(self):
        hosts=self.root/'scsi_host';host=hosts/'host3';host.mkdir(parents=True)
        (host/'proc_name').write_text('mhvtl\n');(host/'can_queue').write_text('1\n')
        with self.assertRaises(ValueError):self.m.require_parallel_host(hosts)
        (host/'can_queue').write_text('128\n');self.m.require_parallel_host(hosts)
        (host/'can_queue').write_text('broken\n')
        with self.assertRaises(ValueError):self.m.require_parallel_host(hosts)
