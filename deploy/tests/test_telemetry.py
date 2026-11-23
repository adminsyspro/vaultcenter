import importlib.util
import json
import datetime
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).parents[1] / 'cold-vtl/telemetry.py'
spec = importlib.util.spec_from_file_location('telemetry', MODULE)
t = importlib.util.module_from_spec(spec)
spec.loader.exec_module(t)
UUID = '00000000-0000-4000-8000-000000000001'

def event(message, queue=11, at_ms=1000):
    stamp = datetime.datetime.fromtimestamp(at_ms / 1000).strftime('%a %b %e %H:%M:%S %Y')
    return f'{stamp} - {queue}: - {message}\n'

def collector(config, started_at_ms=0):
    with patch('time.time', return_value=started_at_ms/1000):
        return t.Collector(config)

class TelemetryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.config = dict(vtl_id=UUID, media_root=str(self.root), library_queue_id=10,
                           drives=[dict(queue_id=11, ordinal=1, label='Lecteur 1')])

    def test_parser_bounds_and_exact_protocol(self):
        self.assertEqual(t.parse_event(event('Writing')), (11, 'Writing'))
        for value in ['Writing', event('Writing')[:-1], 'x'*4097, event('Writing', -1)]:
            self.assertIsNone(t.parse_event(value))

    def test_drive_scope_expiry_restart_and_failure(self):
        c = collector(self.config, started_at_ms=0)
        c.accept(event('Writing', 99), 1000)
        self.assertEqual(c.snapshot(1000)['drives'][0]['state'], 'unknown')
        c.accept(event('Writing'), 1000)
        self.assertEqual(c.snapshot(1000)['drives'][0]['state'], 'writing')
        self.assertEqual(c.snapshot(17000)['drives'][0]['state'], 'unknown')
        c.accept(event('Initialising v2', at_ms=18000), 18000)
        self.assertIsNone(c.snapshot(18000)['drives'][0]['barcode'])
        c.accept(event('Load failed', at_ms=19000), 19000)
        self.assertEqual(c.snapshot(19000)['drives'][0]['state'], 'error')

    def test_movement_is_not_proof_of_position(self):
        c = collector(self.config, started_at_ms=0)
        c.accept(event('Moving CVT001L8 from Storage slot 1 to drive 1', 10), 1000)
        c.accept(event('Loaded'), 1001)
        self.assertIsNone(c.snapshot(2000)['drives'][0]['barcode'])
        self.assertEqual(c.snapshot(1001)['slots'], [])

    def test_rates_and_missing_metrics(self):
        self.assertEqual(t.growth_rate(100, 300, 2), 100)
        for args in [(300,100,2),(100,300,0),(100,300,-1)]:
            self.assertIsNone(t.growth_rate(*args))
        c = collector(self.config, started_at_ms=0)
        c.accept(event('Writing'), 1000)
        d = c.snapshot(1000)['drives'][0]
        self.assertIsNone(d['bytes_written'])
        self.assertIsNone(d['write_rate_bps'])

    def test_metadata_no_payload_reads_and_no_symlink_escape(self):
        tape = self.root/'CVT001L8'
        tape.mkdir()
        (tape/'data.0').write_bytes(b'a'*100)
        (tape/'mam').write_bytes(b'b'*10)
        self.assertEqual(t.sample_media(self.root, 'CVT001L8'), 110)
        self.assertIsNone(t.sample_media(self.root, '../secret'))
        self.assertIsNone(t.sample_media(self.root, 'ABSENT'))
        (tape/'escape').symlink_to('/etc/passwd')
        self.assertIsNone(t.sample_media(self.root, 'CVT001L8'))
        (self.root/'LINK').symlink_to(tape)
        self.assertIsNone(t.sample_media(self.root, 'LINK'))

    def test_atomic_private_snapshot(self):
        path = self.root/'cache.json'
        t.write_snapshot(path, collector(self.config, started_at_ms=0).snapshot(1000))
        self.assertEqual(json.loads(path.read_text())['vtl_id'], UUID)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(list(self.root.iterdir()), [path])

    def test_local_sizes_remain_available_when_drive_and_inventory_are_offline(self):
        tape = self.root/'CVT001L8'
        tape.mkdir()
        (tape/'data.0').write_bytes(b'a'*450)
        c = collector(self.config)
        c.invalidate()  # Missing FIFO while the library is stopped.
        c.sample(1000, 1.0)
        value = c.snapshot(1000)
        self.assertEqual(value['status'], 'available')
        self.assertIsNone(value['reason'])
        self.assertEqual(value['observed_at'], 1000)
        self.assertEqual(value['media'][0]['local_size_bytes'], 450)
        self.assertEqual(value['drives'][0]['state'], 'unknown')
        self.assertIsNone(value['drives'][0]['write_rate_bps'])
        self.assertEqual(value['slots'], [])
        self.assertEqual(c.snapshot(11001)['status'], 'unavailable')
        self.assertEqual(c.snapshot(-5000)['status'], 'unavailable')
        (tape/'data.0').unlink()
        tape.rmdir()
        c.sample(12000, 12.0)
        self.assertEqual(c.snapshot(12000)['media'], [])
        self.assertEqual(c.snapshot(12000)['status'], 'unavailable')

    def test_process_identity_and_open_cartridge(self):
        proc = self.root/'proc'; pid = proc/'42'; (pid/'fd').mkdir(parents=True)
        (pid/'stat').write_text('42 (vtltape) S ' + ' '.join(['0']*18 + ['123']))
        (pid/'comm').write_text('vtltape\n')
        tape = self.root/'CVT001L8'; tape.mkdir(); (tape/'data.0').write_bytes(b'a')
        (pid/'fd/3').symlink_to(tape/'data.0')
        pidfile = self.root/'drive.pid'; pidfile.write_text('42')
        self.config['proc_root'] = str(proc)
        self.config['drives'][0]['pid_file'] = str(pidfile)
        c = collector(self.config, started_at_ms=0)
        c.sample(1000, 1.0)
        c.accept(event('Writing', at_ms=2000), 2000)
        self.assertEqual(c.snapshot(2000)['drives'][0]['barcode'], 'CVT001L8')
        (pid/'stat').write_text('42 (vtltape) S ' + ' '.join(['0']*18 + ['124']))
        c.sample(2000, 2.0)
        self.assertEqual(c.snapshot(2000)['drives'][0]['state'], 'unknown')
        (pid/'fd/3').unlink()
        c.sample(3000, 3.0)
        self.assertIsNone(c.snapshot(3000)['drives'][0]['barcode'])

    def test_stale_prestart_future_and_receipt_time_do_not_revive_activity(self):
        c = collector(self.config, started_at_ms=0)
        c.accept(event('Writing', at_ms=1000), 30000)
        self.assertEqual(c.snapshot(30000)['drives'][0]['state'], 'unknown')
        c.accept(event('Writing', at_ms=40000), 30000)
        self.assertEqual(c.snapshot(30000)['drives'][0]['state'], 'unknown')
        c.accept(event('Writing', at_ms=30000), 31000)
        self.assertEqual(c.snapshot(31000)['drives'][0]['observed_at'], 30000)
        restarted = collector(self.config, started_at_ms=30500)
        restarted.accept(event('Writing', at_ms=30000), 31000)
        self.assertEqual(restarted.snapshot(31000)['drives'][0]['state'], 'unknown')

    def test_same_process_cartridge_change_invalidates_activity(self):
        from unittest.mock import patch
        c = collector(self.config, started_at_ms=0)
        with patch.object(t, 'open_media', return_value=((42, '123'), 'OLD001L8')):
            c.sample(1000, 1.0)
        c.accept(event('Writing', at_ms=2000), 2000)
        self.assertEqual(c.snapshot(2000)['drives'][0]['state'], 'writing')
        with patch.object(t, 'open_media', return_value=((42, '123'), 'NEW001L8')):
            c.sample(3000, 3.0)
        self.assertEqual(c.snapshot(3000)['drives'][0]['barcode'], 'NEW001L8')
        self.assertEqual(c.snapshot(3000)['drives'][0]['state'], 'unknown')
        # Late old-cartridge event must not bind to the new one either.
        c.accept(event('Writing', at_ms=2000), 3100)
        self.assertEqual(c.snapshot(3100)['drives'][0]['state'], 'unknown')
        c.accept(event('Reading', at_ms=4000), 4000)
        self.assertEqual(c.snapshot(4000)['drives'][0]['state'], 'reading')

class HeartbeatTests(TelemetryTests):
    def ready(self):
        c = collector(self.config)
        with patch.object(t, 'open_media', return_value=((42, '123'), 'CVT001L8')):
            c.sample(1000, 1)
        return c

    def heartbeat(self, at=2000, total=100, **overrides):
        record = dict(vaultcenter=1, queue=11, pid=42, at_ms=at,
            mono_ms=at, generation=1, bytes_written=total,
            barcode='CVT001L8', state='Writing')
        record.update(overrides)
        return json.dumps(record)+'\n'

    def test_successful_bytes_rate_and_sustained_heartbeat(self):
        c = self.ready()
        c.accept(self.heartbeat(), 2000)
        self.assertEqual(c.snapshot(2000)['drives'][0]['bytes_written'], 100)

        self.assertIsNone(c.snapshot(2000)['drives'][0]['write_rate_bps'])
        for second in range(3, 33):
            c.accept(self.heartbeat(second*1000, 100+(second-2)*200), second*1000)
        drive = c.snapshot(32000)['drives'][0]
        self.assertEqual(drive['state'], 'writing')
        self.assertEqual(drive['write_rate_bps'], 200)
        c.accept(self.heartbeat(33000, 6100, state='Loaded - Idle'), 33000)
        self.assertEqual(c.snapshot(33000)['drives'][0]['state'], 'idle')
        self.assertEqual(c.snapshot(33000)['drives'][0]['write_rate_bps'], 0)

    def test_bandwidth_confirmation_expires_with_heartbeat(self):
        c = self.ready()
        c.accept(self.heartbeat(bandwidth_revision=2, tape_write_limit_bps=50_000_000, bandwidth_error=False), 2000)
        observed = c.snapshot(2000)['drives'][0]
        self.assertEqual(observed.get('bandwidth_revision'), 2)
        self.assertEqual(observed.get('tape_write_limit_bps'), 50_000_000)
        self.assertIsNone(c.snapshot(20000)['drives'][0].get('bandwidth_revision'))

    def test_stale_missing_reordered_and_generation_boundaries(self):
        c = self.ready()
        c.accept(self.heartbeat(), 2000)
        c.accept(self.heartbeat(3000, 300), 3000)
        c.accept(self.heartbeat(2500, 200), 3100)
        self.assertEqual(c.snapshot(3100)['drives'][0]['observed_at'], 3000)
        stale = c.snapshot(19000)['drives'][0]
        self.assertEqual(stale['state'], 'unknown')
        self.assertIsNone(stale['write_rate_bps'])
        c.accept(self.heartbeat(20000, 400), 20000)
        self.assertIsNone(c.snapshot(20000)['drives'][0]['write_rate_bps'])
        c.accept(self.heartbeat(21000, 500, generation=2), 21000)
        self.assertIsNone(c.snapshot(21000)['drives'][0]['write_rate_bps'])
        c.invalidate()
        self.assertIsNone(c.snapshot(22000)['drives'][0]['bytes_written'])

    def test_reject_wrong_process_cartridge_malformed_and_counter_reset(self):
        for values in [dict(pid=43), dict(barcode='OTHER'), dict(bytes_written=-1),
                       dict(bytes_written=True), dict(mono_ms='bad'), dict(state='invented'),
                       dict(at_ms=99000), dict(bytes_written=2**64)]:
            c = self.ready()
            c.accept(self.heartbeat(**values), 2000)
            self.assertIsNone(c.snapshot(2000)['drives'][0]['observed_at'], values)
        c = self.ready()
        c.accept(self.heartbeat(), 2000)
        c.accept(self.heartbeat(3000, 90), 3000)
        self.assertIsNone(c.snapshot(3000)['drives'][0]['write_rate_bps'])
        with patch.object(t, 'open_media', return_value=((43, '124'), 'CVT001L8')):
            c.sample(4000, 4)
        self.assertIsNone(c.snapshot(4000)['drives'][0]['bytes_written'])

    def test_required_heartbeat_does_not_trust_legacy_shifted_labels(self):
        self.config['require_heartbeat'] = True
        c = self.ready()
        c.accept(event('Reading', at_ms=2000), 2000)
        self.assertIsNone(c.snapshot(2000)['drives'][0]['observed_at'])
        c.accept(self.heartbeat(), 2000)
        self.assertEqual(c.snapshot(2000)['drives'][0]['state'], 'writing')


class SlotMapTests(TelemetryTests):
    def setUp(self):
        super().setUp()
        run = tempfile.TemporaryDirectory()
        self.addCleanup(run.cleanup)
        pid_file = Path(run.name) / 'pid-10'
        pid_file.write_text('77\n')
        self.config['library_pid_file'] = str(pid_file)

    def record(self, slots, at=2000, sequence=1, part=0, parts=1, **overrides):
        data = dict(vaultcenter=1, queue=10, pid=77, at_ms=at, slot_map=sequence, part=part, parts=parts, slots=slots)
        data.update(overrides)
        return json.dumps(data) + '\n'

    def test_library_map_places_storage_then_import_export(self):
        c = collector(self.config)
        c.accept(self.record([[2, 1025, None], [3, 16, 'CVT009L8'], [2, 1024, 'CVT001L8']]), 2000)
        self.assertEqual([(s['number'], s['kind'], s['barcode'], s['position_known']) for s in c.snapshot(2000)['slots']],
                         [(1, 'storage', 'CVT001L8', True), (2, 'storage', None, True), (3, 'import_export', 'CVT009L8', True)])
        self.assertEqual(c.snapshot(2000)['status'], 'available')
        # Positions expire like PBS inventory when the library stops publishing.
        self.assertEqual({s['position_known'] for s in c.snapshot(18000)['slots']}, {False})

    def test_split_map_is_applied_only_when_complete(self):
        c = collector(self.config)
        c.accept(self.record([[2, 1024, 'CVT001L8']], sequence=4, part=0, parts=2), 2000)
        self.assertEqual(c.snapshot(2000)['slots'], [])
        c.accept(self.record([[2, 1025, 'CVT002L8']], sequence=4, part=1, parts=2), 2000)
        self.assertEqual([s['barcode'] for s in c.snapshot(2000)['slots']], ['CVT001L8', 'CVT002L8'])
        # A newer map supersedes an unfinished older one.
        c.accept(self.record([[2, 1024, None]], sequence=5, part=0, parts=2), 2100)
        c.accept(self.record([[2, 1024, 'OLD001L8']], sequence=4, part=1, parts=2), 2100)
        self.assertEqual([s['barcode'] for s in c.snapshot(2100)['slots']], ['CVT001L8', 'CVT002L8'])

    def test_rejects_foreign_stale_or_malformed_maps(self):
        c = collector(self.config)
        for line in [self.record([[2, 1024, 'CVT001L8']], pid=78),
                     self.record([[2, 1024, 'CVT001L8']], queue=11),
                     self.record([[2, 1024, 'CVT001L8']], at=-20000),
                     self.record([[4, 1024, 'CVT001L8']]),
                     self.record([[2, 1024, 'bad barcode']]),
                     self.record([[2, 1024, 'CVT001L8'], [2, 1024, None]]),
                     self.record([[2, 1024, 'CVT001L8'], [2, 1025, 'CVT001L8']]),
                     self.record([[2, 1024, 'CVT001L8']], part=1, parts=1)]:
            c.accept(line, 2000)
        self.assertEqual(c.snapshot(2000)['slots'], [])

    def test_backup_client_inventory_keeps_priority(self):
        self.config['pbs'] = dict(pbs='pbs', key_file='/dev/null', known_hosts='/dev/null')
        with patch.dict('sys.modules', pbs_inventory=type('M', (), {'InventoryPoller': lambda *a: object()})):
            c = collector(self.config)
        c.accept(self.record([[2, 1024, 'CVT001L8']]), 2000)
        self.assertEqual(c.slots, [])

    def test_pid_file_follows_the_vtl_fifo(self):
        config = dict(self.config, fifo='/run/vaultcenter-vtl-x/status.fifo', library_queue_id=30)
        del config['library_pid_file']
        self.assertEqual(collector(config).library_pid_file, '/run/vaultcenter-vtl-x/pid-30')
