"""Guardrails for the opt-in real iSCSI isolation probe, not simulated isolation."""
import importlib.util
from pathlib import Path
import unittest

PATH = Path(__file__).resolve().parents[1] / 'lab/multi-vtl/probe.py'

class IsolationProbeTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('isolation_probe', PATH)
        self.probe = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.probe)

    def test_does_not_accept_production_or_other_target(self):
        for identifier in (1, 10, 11, 21, 31, -1, '20; reboot'):
            with self.subTest(identifier=identifier), self.assertRaises(ValueError):
                self.probe.target_name(identifier)

    def test_target_number_alone_does_not_authorize_removal(self):
        output = 'Target 20: iqn.example:production\nTarget 30: iqn.2026-09.local.cold-vtl:isolation30\n'
        with self.assertRaises(RuntimeError):
            self.probe.require_identity(output, 20)

    def test_requires_exact_target_identity_not_prefix(self):
        output = 'Target 20: iqn.2026-09.local.cold-vtl:isolation20-other\n'
        with self.assertRaises(RuntimeError):
            self.probe.require_identity(output, 20)

    def test_accepts_test_target_beside_production(self):
        output = 'Target 1: iqn.example:production\nTarget 20: iqn.2026-09.local.cold-vtl:isolation20\n'
        self.probe.require_identity(output, 20)

    def test_failed_or_short_read_invalidates_evidence(self):
        good = {'reads': 5, 'failures': 0, 'verified_bytes': 29696}
        self.probe.require_reads(good)
        for bad in ({**good, 'failures': 1}, {**good, 'reads': 0}, {**good, 'verified_bytes': 1}):
            with self.assertRaises(RuntimeError):
                self.probe.require_reads(bad)

if __name__ == '__main__':
    unittest.main()
