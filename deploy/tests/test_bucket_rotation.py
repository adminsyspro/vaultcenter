import contextlib
import copy
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import test_archive
archive = test_archive.archive

VTL = '00000000-0000-4000-8000-000000000001'
SOURCE = '00000000-0000-4000-8000-000000000002'


class RotationTests(unittest.TestCase):
    tearDown = test_archive.ArchiveTests.tearDown
    phase = test_archive.ArchiveTests.phase

    def setUp(self):
        test_archive.ArchiveTests.setUp(self)
        self.create_fails = False
        self.refuse_create = None
        self.contexts = []
        self.configuration = dict(provider='ovh_cold_archive_v1', endpoint='https://s3.rbx-archive.io.cloud.ovh.net',
                                  region='rbx-archive', bucket='test-bucket', prefix='cold-vtl',
                                  access_key_id='old-key', secret_access_key='old-secret')
        migrations = Path(__file__).parents[2] / 'crates/cold-vtl-api/migrations'
        with self.instance.db() as db:
            db.execute('INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,1,?,?)', (json.dumps(self.configuration), 'test'))
            db.executescript((migrations / '0004_maintenance.sql').read_text())
            db.executescript((migrations / '0008_multi_vtl.sql').read_text())
            db.executescript(next(migrations.glob('0009_*.sql')).read_text())
            identity = {k: v for k, v in self.configuration.items() if k not in ('access_key_id', 'secret_access_key')}
            db.execute('UPDATE storage_destinations SET identity=? WHERE id=?', (json.dumps(identity), SOURCE))

    def worker(self, args):
        self.contexts.append(copy.deepcopy(self.instance.context))
        if args[0] == 'create-bucket':
            self.calls.append(args)
            if self.create_fails:
                raise RuntimeError('lost create response')
            if self.refuse_create:
                return dict(status='not_created', reason=self.refuse_create, bucket=args[2])
            return dict(status='created', bucket=args[2])
        return test_archive.ArchiveTests.worker(self, args)

    def job(self, action, destination=SOURCE, revision=1):
        request = test_archive.ArchiveTests.job(self, action)
        request.update(vtl_id=VTL, destination_id=destination, configuration_revision=revision)
        with self.instance.db() as db:
            db.execute('UPDATE jobs SET destination_id=?,configuration_revision=? WHERE id=?', (destination, revision, request['job_id']))
        return request

    def submit(self, destination=SOURCE, revision=1, **fields):
        request = self.job('submit', destination, revision)
        request.update(prepare_job_id=request['job_id'], confirm_bucket='test-bucket', acknowledge_billing=True,
                       exclusive_bucket=True, next_bucket='vc-2026-09')
        request.update(fields)
        with self.instance.db() as db:
            db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,destination_id) VALUES(?,?,'queued',?,?)",
                       (request['job_id'], request['job_id'], json.dumps(request), destination))
            if request.get('next_bucket'):
                db.execute("INSERT INTO bucket_rotations(job_id,vtl_id,source_destination_id,next_bucket,phase) VALUES(?,?,?,?,'reserved')",
                           (request['job_id'], VTL, destination, request['next_bucket']))
        return request

    def active(self):
        with self.instance.db() as db:
            return db.execute('SELECT id FROM storage_destinations WHERE active=1').fetchone()[0]

    def rotation(self, request):
        with self.instance.db() as db:
            return dict(db.execute('SELECT * FROM bucket_rotations WHERE job_id=?', (request['job_id'],)).fetchone())

    def test_acceptance_switches_only_after_create_preserving_credentials_and_old_copies(self):
        with self.instance.db() as db:
            db.execute("INSERT INTO remote_generations(cartridge_id,barcode,generation,metadata) SELECT id,barcode,1,'{}' FROM cartridges")
        request = self.submit()
        self.instance.execute('submit', request)
        rotation = self.rotation(request)
        self.assertEqual(rotation['phase'], 'switched')
        self.assertEqual(self.active(), rotation['next_destination_id'])
        self.assertEqual([c[0] for c in self.calls], ['archive-inspect', 'create-bucket', 'archive-bucket'])
        self.assertTrue(all(c['destination_id'] == SOURCE and c['configuration_revision'] == 1 for c in self.contexts))
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT destination_id FROM remote_generations').fetchone()[0], SOURCE)
            active_config = json.loads(db.execute('SELECT configuration FROM s3_settings').fetchone()[0])
            self.assertEqual(active_config['bucket'], 'vc-2026-09')
            self.assertEqual(active_config['secret_access_key'], 'old-secret')
            self.assertEqual(db.execute('SELECT COUNT(*) FROM s3_reconcile_required').fetchone()[0], 0)
            self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_definitive_refusal_releases_the_rotation_with_its_reason(self):
        self.refuse_create = 'exists'
        request = self.submit()
        with self.assertRaisesRegex(RuntimeError, 'already taken'):
            self.instance.execute('submit', request)
        rotation = self.rotation(request)
        self.assertEqual(rotation['phase'], 'failed')
        self.assertIsNone(rotation['next_destination_id'])
        self.assertEqual(self.active(), SOURCE)
        self.assertEqual(self.phase(request), 'failed')
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM storage_destinations').fetchone()[0], 1)
            self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        # Nothing is left to settle: the next archive proceeds with a new name.
        self.refuse_create = None
        self.instance.execute('submit', self.submit(next_bucket='vc-2026-09-b'))
        self.assertNotEqual(self.active(), SOURCE)

    def test_worker_refusal_payload_is_returned_not_raised(self):
        self.mock.stop()
        with patch.object(archive.subprocess, 'run') as run:
            run.return_value.returncode = 3
            run.return_value.stdout = json.dumps({'status': 'not_created', 'reason': 'denied', 'bucket': 'vc-x'})
            self.assertEqual(self.instance.run_worker(['create-bucket', '--bucket', 'vc-x'])['reason'], 'denied')
            run.return_value.stdout = 'garbage'
            with self.assertRaises(RuntimeError):
                self.instance.run_worker(['create-bucket', '--bucket', 'vc-x'])
            run.return_value.returncode = 1
            run.return_value.stdout = json.dumps({'status': 'not_created', 'reason': 'denied', 'bucket': 'vc-x'})
            with self.assertRaises(RuntimeError):
                self.instance.run_worker(['create-bucket', '--bucket', 'vc-x'])

    def test_create_failure_leaves_source_active_and_never_archives_or_retries(self):
        request = self.submit()
        self.create_fails = True
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaisesRegex(RuntimeError, 'The creation of the next bucket is not confirmed'):
            self.instance.execute('submit', request)
        self.assertRegex(stderr.getvalue(), r'^create-bucket vc-\S+: lost create response\n$')
        self.assertEqual(self.active(), SOURCE)
        self.assertEqual(self.rotation(request)['phase'], 'uncertain')
        self.assertFalse(any(c[0] == 'archive-bucket' for c in self.calls))
        self.instance.observe()
        self.assertEqual(sum(c[0] == 'create-bucket' for c in self.calls), 1)

    def test_historical_archive_after_manual_rotation_does_not_create_or_switch(self):
        next_config = dict(self.configuration, bucket='manual-next-bucket')
        with self.instance.db() as db:
            db.execute('UPDATE storage_destinations SET active=0 WHERE id=?', (SOURCE,))
            db.execute("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('manual-next',?,?,1)",
                       (VTL, json.dumps({k: v for k, v in next_config.items() if k not in ('access_key_id', 'secret_access_key')})))
            db.execute("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES('manual-next',2,?)", (json.dumps(next_config),))
            db.execute('UPDATE s3_settings SET revision=2,configuration=?', (json.dumps(next_config),))
        request = self.submit(next_bucket=None)
        self.instance.execute('submit', request)
        self.assertEqual(self.active(), 'manual-next')
        self.assertEqual(self.phase(request), 'accepted')
        self.assertEqual([c[0] for c in self.calls], ['archive-inspect', 'archive-bucket'])
        with self.instance.db() as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM bucket_rotations').fetchone()[0], 0)
            self.assertEqual(db.execute('SELECT COUNT(*) FROM storage_destinations').fetchone()[0], 2)

    def test_lost_archive_response_recovers_by_observation_without_cloud_replay(self):
        request = self.submit()
        self.fail_put = True
        with self.assertRaisesRegex(RuntimeError, 'Lost response'):
            self.instance.execute('submit', request)
        self.assertEqual(self.active(), SOURCE)
        self.assertEqual(self.rotation(request)['phase'], 'ready')
        self.instance.observe()
        self.assertEqual(self.active(), SOURCE)
        self.observed = 'Archiving'
        self.instance.observe()
        self.assertEqual(self.active(), self.rotation(request)['next_destination_id'])
        self.assertEqual(sum(c[0] == 'archive-bucket' for c in self.calls), 1)
        self.assertEqual(sum(c[0] == 'create-bucket' for c in self.calls), 1)
        self.assertEqual(self.contexts[-1]['destination_id'], SOURCE)

    def test_restart_after_archive_acceptance_finishes_switch(self):
        request = self.submit()
        with patch.object(self.instance, 'switch_bucket', side_effect=RuntimeError('crash')):
            with self.assertRaisesRegex(RuntimeError, 'crash'):
                self.instance.execute('submit', request)
        self.assertEqual(self.phase(request), 'accepted')
        self.assertEqual(self.active(), SOURCE)
        self.observed = 'Archived'
        self.instance.observe()
        self.assertEqual(self.rotation(request)['phase'], 'switched')
        self.assertNotEqual(self.active(), SOURCE)
        self.assertEqual(sum(c[0] == 'archive-bucket' for c in self.calls), 1)

    def test_foreign_context_and_duplicate_bucket_refuse_before_cloud_mutation(self):
        request = self.job('submit')
        request['destination_id'] = 'foreign'
        with self.assertRaisesRegex(RuntimeError, 'S3 scope'):
            self.instance.execute('submit', request)
        with self.assertRaisesRegex(RuntimeError, 'vc- prefix'):
            self.instance.execute('submit', self.submit(next_bucket='test-bucket'))
        self.assertFalse(any(c[0] in ('create-bucket', 'archive-bucket') for c in self.calls))

    def test_only_attached_buckets_need_the_exclusive_confirmation(self):
        with self.assertRaisesRegex(RuntimeError, 'Confirmation'):
            self.instance.execute('submit', self.submit(exclusive_bucket=False))
        self.instance.execute('submit', self.submit())
        managed = self.active()
        self.assertNotEqual(managed, SOURCE)
        request = self.submit(destination=managed, revision=2, exclusive_bucket=False, next_bucket='vc-2026-10')
        self.instance.execute('submit', request)
        self.assertEqual(self.phase(request), 'accepted')
        self.assertEqual(self.rotation(request)['phase'], 'switched')

    def test_worker_receives_exact_revision_even_after_active_switch(self):
        self.instance.execute('submit', self.submit())
        self.mock.stop()
        with patch.object(archive.subprocess, 'run') as run:
            run.return_value.returncode = 0
            run.return_value.stdout = '{}'
            self.instance.run_worker(['bucket-status'])
            env = run.call_args.kwargs['env']
            self.assertEqual(env['COLD_VTL_DESTINATION_ID'], SOURCE)
            self.assertEqual(env['COLD_VTL_CONFIGURATION_REVISION'], '1')
            self.assertNotIn('old-secret', str(run.call_args))

    def test_rejected_confirmation_releases_reserved_rotation(self):
        request = self.submit(acknowledge_billing=False)
        with self.assertRaisesRegex(RuntimeError, 'Confirmation'):
            self.instance.execute('submit', request)
        self.assertEqual(self.rotation(request)['phase'], 'failed')
        self.assertFalse(any(c[0] == 'create-bucket' for c in self.calls))
        self.instance.execute('submit', self.submit())
        self.assertNotEqual(self.active(), SOURCE)

    def test_explicit_refresh_recovers_abandoned_reserved_job_without_creation(self):
        request = self.submit()
        with self.instance.db() as db:
            db.execute("UPDATE jobs SET status='failed' WHERE id=?", (request['job_id'],))
        self.instance.execute('refresh', self.job('refresh'))
        self.assertEqual(self.phase(request), 'failed')
        self.assertEqual(self.rotation(request)['phase'], 'failed')
        self.assertFalse(any(c[0] == 'create-bucket' for c in self.calls))

    def test_modern_job_without_revision_refuses_even_when_new_credentials_exist(self):
        request = self.job('submit')
        request.pop('configuration_revision')
        with self.instance.db() as db:
            db.execute('UPDATE jobs SET configuration_revision=NULL WHERE id=?', (request['job_id'],))
            db.execute('INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,2,?)', (SOURCE, json.dumps(self.configuration)))
        with self.assertRaisesRegex(RuntimeError, 'The S3 revision of the job is missing'):
            self.instance.execute('submit', request)
        self.assertEqual(self.calls, [])

    def test_historical_recall_uses_source_bucket_and_old_credentials_after_rotation(self):
        self.instance.execute('submit', self.submit())
        active = self.active()
        self.observed = 'Archived'
        self.instance.observe()
        self.controller.leave('admin')
        self.receipt['scope']['raw_status'] = 'Archived'
        prior_calls = len(self.calls)

        def worker(args):
            self.calls.append(args)
            self.assertEqual(self.instance.context['destination_id'], SOURCE)
            self.assertEqual(self.instance.context['configuration_revision'], 1)
            if args[0] == 'recall-inspect':
                return copy.deepcopy(self.receipt)
            if args[0] == 'request-bucket-restore':
                return dict(status='request_accepted', bucket='test-bucket')
            raise AssertionError(args)
        with patch.object(self.instance, 'run_worker', side_effect=worker):
            request = self.job('submit')
            request.update(prepare_job_id=request['job_id'], confirm_bucket='test-bucket', acknowledge_retrieval_cost=True)
            with self.instance.db() as db:
                db.execute("UPDATE jobs SET kind='recall_submit' WHERE id=?", (request['job_id'],))
                db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,destination_id,phase,request,operation) VALUES(?,?,?,'queued',?,'recall')",
                           (request['job_id'], request['job_id'], SOURCE, json.dumps(request)))
            self.instance.execute('recall-submit', request)
        self.assertEqual(self.active(), active)
        self.assertEqual([c[0] for c in self.calls[prior_calls:]], ['recall-inspect', 'request-bucket-restore'])

    def test_invalid_submit_context_releases_reservation_in_failure_transaction(self):
        request = self.submit()
        request.pop('configuration_revision')
        with self.instance.db() as db:
            db.execute('UPDATE jobs SET configuration_revision=NULL WHERE id=?', (request['job_id'],))
            db.execute('UPDATE archive_requests SET request=? WHERE job_id=?', (json.dumps(request), request['job_id']))
        with self.assertRaisesRegex(RuntimeError, 'The S3 revision of the job is missing'):
            self.instance.execute('submit', request)
        self.assertEqual(self.phase(request), 'failed')
        self.assertEqual(self.rotation(request)['phase'], 'failed')
        self.assertFalse(any(c[0] == 'create-bucket' for c in self.calls))
