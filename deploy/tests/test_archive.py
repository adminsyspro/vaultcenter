import copy
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
import uuid
from test_maintenance import module, FakeController, FakeGuard

archive = module('archive', 'cold-vtl/archive-controller.py')


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.guard = FakeGuard(self.root)
        self.controller = FakeController(self.root, self.guard)
        self.instance = archive.Archive({}, self.root, self.controller)
        migrations = Path(__file__).parents[2] / 'crates/cold-vtl-api/migrations'
        with self.instance.db() as db:
            for name in ['0001_initial.sql', '0002_restore.sql', '0005_archive.sql', '0006_recall.sql', '0007_s3_settings.sql']:
                db.executescript((migrations / name).read_text())
            db.execute("INSERT INTO cartridges(barcode,capacity_bytes,state) VALUES('TAPE01',100,'writable')")
        self.calls = []
        self.receipt = {'digest': 'a' * 64, 'scope': {
            'bucket': 'test-bucket', 'generations': [{'barcode': 'TAPE01', 'generation': 1}],
            'inventory': {'objects': 2, 'bytes': 20, 'cartridges': [{'barcode': 'TAPE01'}]}}}
        self.observed = 'None'
        self.fail_put = False
        self.fail_inspect = False
        self.mock = patch.object(self.instance, 'run_worker', side_effect=self.worker)
        self.mock.start()

    def tearDown(self):
        self.mock.stop()
        if self.guard.lock:
            self.guard.lock.close()
        self.temp.cleanup()

    def worker(self, args):
        self.calls.append(args)
        if args[0] == 'archive-inspect':
            if self.fail_inspect:
                raise RuntimeError('Copie TAPE01 altérée : somme de contrôle différente du manifeste.')
            return copy.deepcopy(self.receipt)
        if args[0] == 'archive-bucket':
            if self.fail_put:
                raise RuntimeError('Lost response')
            return {'status': 'request_accepted', 'bucket': 'test-bucket'}
        if args[0] == 'bucket-status':
            return {'bucket': 'test-bucket', 'status': self.observed}
        raise AssertionError(args)

    def job(self, action):
        job_id = str(uuid.uuid4())
        with self.instance.db() as db:
            db.execute("INSERT INTO jobs(id,kind,subject,status) VALUES(?,?,?,'queued')", (job_id, 'archive_' + action, 'test-bucket'))
        return {'job_id': job_id, 'actor': 'admin'}

    def submit(self, **fields):
        request = self.job('submit')
        request.update(prepare_job_id=request['job_id'], confirm_bucket='test-bucket',
                       acknowledge_billing=True, exclusive_bucket=True)
        request.update(fields)
        with self.instance.db() as db:
            db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,updated_at) VALUES(?,?,'queued',?,CURRENT_TIMESTAMP)",
                       (request['job_id'], request['job_id'], json.dumps(request)))
        return request

    def phase(self, request):
        with self.instance.db() as db:
            return db.execute('SELECT phase FROM archive_requests WHERE job_id=?', (request['job_id'],)).fetchone()[0]

    def test_archive_verifies_the_bucket_then_submits_without_maintenance(self):
        request = self.submit()
        with patch.object(self.controller, 'check', side_effect=AssertionError('maintenance must not be required')):
            result = self.instance.execute('submit', request)
        self.assertEqual([c[0] for c in self.calls], ['archive-inspect', 'archive-bucket'])
        self.assertEqual(self.calls[0], ['archive-inspect'])
        self.assertNotIn('--media-root', self.calls[1])
        self.assertEqual(self.calls[1][self.calls[1].index('--expected-receipt') + 1], 'a' * 64)
        self.assertEqual(self.phase(request), 'accepted')
        self.assertEqual(result['verified'], {'cartridges': 1, 'bytes': 20, 'copies': 1})
        self.assertNotIn('maintenance', result['message'].lower())
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT state FROM cartridges').fetchone()[0], 'writable')
        self.observed = 'Archived'
        self.instance.execute('refresh', self.job('refresh'))
        self.assertEqual(self.phase(request), 'archived')

    def test_lost_put_response_never_retries_even_when_none_is_observed(self):
        request = self.submit()
        self.fail_put = True
        with self.assertRaisesRegex(RuntimeError, 'Lost response'):
            self.instance.execute('submit', request)
        self.assertEqual(self.phase(request), 'uncertain')
        self.instance.execute('refresh', self.job('refresh'))
        self.assertEqual(self.phase(request), 'uncertain')
        with self.assertRaises(RuntimeError):
            self.controller.leave('admin')
        with self.assertRaises(RuntimeError):
            self.instance.execute('submit', request)
        self.observed = 'Archiving'
        self.instance.execute('refresh', self.job('refresh'))
        self.assertEqual(self.phase(request), 'accepted')
        self.assertEqual(sum(c[0] == 'archive-bucket' for c in self.calls), 1)

    def test_damaged_copy_refuses_before_any_put(self):
        request = self.submit()
        self.fail_inspect = True
        with self.assertRaisesRegex(RuntimeError, 'altérée'):
            self.instance.execute('submit', request)
        self.assertEqual(self.phase(request), 'failed')
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))

    def test_missing_billing_exclusive_use_or_foreign_bucket_refuses(self):
        for fields in ({'acknowledge_billing': False}, {'exclusive_bucket': False}, {'confirm_bucket': 'other'}):
            request = self.submit(**fields)
            with self.assertRaisesRegex(RuntimeError, 'Confirmation'):
                self.instance.execute('submit', request)
            self.assertEqual(self.phase(request), 'failed')
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))

    def test_confirmation_is_bound_to_its_own_request(self):
        request = self.submit()
        other = str(uuid.uuid4())
        with self.instance.db() as db:
            # Completed from the start: only one active job per subject is allowed.
            db.execute("INSERT INTO jobs(id,kind,subject,status) VALUES(?,'archive_submit','test-bucket','completed')", (other,))
            db.execute('UPDATE archive_requests SET prepare_job_id=? WHERE job_id=?', (other, request['job_id']))
        with self.assertRaisesRegex(RuntimeError, 'Confirmation missing'):
            self.instance.execute('submit', request)
        self.assertEqual(self.calls, [])

    def test_interruption_before_and_after_durable_boundary(self):
        for boundary, expected in [('queued', 'failed'), ('submitting', 'uncertain')]:
            request = self.submit()
            with self.instance.db() as db:
                db.execute('UPDATE archive_requests SET phase=? WHERE job_id=?', (boundary, request['job_id']))
                db.execute("UPDATE jobs SET status='failed' WHERE id=?", (request['job_id'],))
            self.instance.execute('refresh', self.job('refresh'))
            self.assertEqual(self.phase(request), expected)
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))

    def test_global_lock_blocks_maintenance_and_duplicate_confirmation(self):
        request = self.submit()
        with self.assertRaises(RuntimeError):
            self.controller.leave('admin')
        with self.assertRaises(sqlite3.IntegrityError):
            with self.instance.db() as db:
                db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,updated_at) VALUES(?,?,'queued','{}',CURRENT_TIMESTAMP)",
                           (str(uuid.uuid4()), request['job_id']))
        with (self.root / 'maintenance-controller.lock').open('a+') as lock:
            archive.fcntl.flock(lock, archive.fcntl.LOCK_EX | archive.fcntl.LOCK_NB)
            with self.assertRaisesRegex(RuntimeError, 'maintenance lock'):
                self.instance.execute('submit', request)
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))

    def test_worker_errors_do_not_expose_sdk_credentials(self):
        self.mock.stop()
        with patch.object(archive.subprocess, 'run') as run:
            run.return_value.returncode = 1
            run.return_value.stderr = 'AWS_SECRET_ACCESS_KEY=secret'
            with self.assertRaises(RuntimeError) as result:
                self.instance.run_worker(['archive-inspect'])
            self.assertNotIn('secret', str(result.exception))


class RecallTests(unittest.TestCase):
    tearDown = ArchiveTests.tearDown
    phase = ArchiveTests.phase

    def setUp(self):
        ArchiveTests.setUp(self)
        self.observed = 'Archived'
        self.receipt['scope']['raw_status'] = 'Archived'

    def worker(self, args):
        self.calls.append(args)
        if args[0] == 'recall-inspect':
            if self.observed != 'Archived':
                raise RuntimeError('Bucket must be Archived')
            return copy.deepcopy(self.receipt)
        if args[0] == 'request-bucket-restore':
            if self.fail_put:
                raise RuntimeError('Lost recall response')
            self.observed = 'Restoring'
            return {'status': 'request_accepted', 'bucket': 'test-bucket'}
        if args[0] == 'bucket-status':
            return {'bucket': 'test-bucket', 'status': self.observed}
        raise AssertionError(args)

    def job(self, action):
        request = ArchiveTests.job(self, action)
        if action != 'refresh':
            with self.instance.db() as db:
                db.execute('UPDATE jobs SET kind=? WHERE id=?', ('recall_' + action, request['job_id']))
        return request

    def submit(self, **fields):
        request = self.job('submit')
        request.update(prepare_job_id=request['job_id'], confirm_bucket='test-bucket', acknowledge_retrieval_cost=True)
        request.update(fields)
        with self.instance.db() as db:
            db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation) VALUES(?,?,'queued',?,'recall')",
                       (request['job_id'], request['job_id'], json.dumps(request)))
        return request

    def test_recall_checks_and_submits_in_one_request_then_monitor_observes_readiness(self):
        commands = self.controller.commands.copy()
        request = self.submit()
        result = self.instance.execute('recall-submit', request)
        self.assertEqual(result['status'], 'request_accepted')
        self.assertEqual([c[0] for c in self.calls], ['recall-inspect', 'request-bucket-restore'])
        self.assertEqual(self.phase(request), 'accepted')
        self.instance.observe()
        self.assertEqual(self.phase(request), 'accepted')
        self.observed = 'Restored'
        self.instance.observe()
        self.assertEqual(self.phase(request), 'restored')
        self.assertEqual(self.controller.commands, commands)
        self.assertIsNone(self.guard.lease)

    def test_lost_reply_stays_uncertain_until_restoring_or_restored_observed(self):
        request = self.submit()
        self.fail_put = True
        with self.assertRaisesRegex(RuntimeError, 'Lost recall'):
            self.instance.execute('recall-submit', request)
        self.assertEqual(self.phase(request), 'uncertain')
        self.instance.observe()
        self.assertEqual(self.phase(request), 'uncertain')
        self.observed = 'Restoring'
        self.instance.observe()
        self.assertEqual(self.phase(request), 'accepted')
        self.observed = 'Restored'
        self.instance.observe()
        self.assertEqual(self.phase(request), 'restored')
        self.assertEqual(sum(c[0] == 'request-bucket-restore' for c in self.calls), 1)

    def test_restart_recovers_submitting_without_reissuing_recall(self):
        request = self.submit()
        self.instance.phase(request['job_id'], 'submitting')
        with self.instance.db() as db:
            db.execute("UPDATE jobs SET status='failed' WHERE id=?", (request['job_id'],))
        self.observed = 'Restored'
        restarted = archive.Archive({}, self.root, self.controller)
        with patch.object(restarted, 'run_worker', side_effect=self.worker):
            restarted.observe()
        self.assertEqual(self.phase(request), 'restored')
        self.assertFalse(any(c[0] == 'request-bucket-restore' for c in self.calls))

    def test_monitor_does_not_cancel_a_queued_request_or_steal_live_flock(self):
        request = self.submit()
        self.instance.observe()
        self.assertEqual(self.phase(request), 'queued')
        before = len(self.calls)
        with (self.root / 'maintenance-controller.lock').open('a+') as lock:
            archive.fcntl.flock(lock, archive.fcntl.LOCK_EX | archive.fcntl.LOCK_NB)
            self.assertEqual(self.instance.observe()['status'], 'busy')
        self.assertEqual(len(self.calls), before)

    def test_bucket_not_archived_refuses_before_put(self):
        for state in ('None', 'Restoring', 'Restored', 'Archiving', 'unexpected'):
            self.observed = state
            request = self.submit()
            with self.assertRaises(RuntimeError):
                self.instance.execute('recall-submit', request)
            self.assertEqual(self.phase(request), 'failed')
        self.assertFalse(any(c[0] == 'request-bucket-restore' for c in self.calls))

    def test_another_recall_is_blocked_until_first_completion_is_observed(self):
        request = self.submit()
        self.instance.execute('recall-submit', request)
        second = self.job('submit')
        second.update(prepare_job_id=second['job_id'], confirm_bucket='test-bucket', acknowledge_retrieval_cost=True)
        # The catalogue refuses a second pending recall intent; the controller refuses the job too.
        with self.assertRaises(sqlite3.IntegrityError):
            with self.instance.db() as db:
                db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation) VALUES(?,?,'queued',?,'recall')",
                           (second['job_id'], second['job_id'], json.dumps(second)))
        with self.assertRaisesRegex(RuntimeError, 'already been accepted'):
            self.instance.execute('recall-submit', second)
        self.observed = 'Restored'
        self.instance.observe()
        self.observed = 'Archived'  # The read window has elapsed.
        self.instance.execute('recall-submit', self.submit())
        self.assertEqual(self.phase(request), 'restored')
        self.assertEqual(sum(c[0] == 'request-bucket-restore' for c in self.calls), 2)

    def test_monitor_network_failure_leaves_intent_unchanged(self):
        request = self.submit()
        self.instance.phase(request['job_id'], 'uncertain')
        with patch.object(self.instance, 'run_worker', side_effect=RuntimeError('network down')):
            with self.assertRaises(RuntimeError):
                self.instance.observe()
        self.assertEqual(self.phase(request), 'uncertain')

    def test_missing_cost_and_wrong_bucket_are_refused(self):
        for fields in ({'acknowledge_retrieval_cost': False}, {'confirm_bucket': 'foreign'}):
            request = self.submit(**fields)
            with self.assertRaisesRegex(RuntimeError, 'Confirm'):
                self.instance.execute('recall-submit', request)
            self.assertEqual(self.phase(request), 'failed')
        self.assertFalse(any(c[0] == 'request-bucket-restore' for c in self.calls))
