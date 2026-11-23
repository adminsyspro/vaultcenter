import json
from pathlib import Path
import sqlite3
import unittest

MIGRATIONS = Path(__file__).resolve().parents[2] / 'crates/cold-vtl-api/migrations'

class NotificationEventsTest(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        for path in sorted(MIGRATIONS.glob('*.sql')):
            self.db.executescript(path.read_text())
        # Empty fallback exposes the missing behavior without a missing-file error.
        self.db.execute("CREATE TABLE IF NOT EXISTS notification_settings(id INTEGER PRIMARY KEY, revision INTEGER, configuration TEXT)")
        self.db.execute("CREATE TABLE IF NOT EXISTS notification_events(id TEXT PRIMARY KEY, payload TEXT, recipients TEXT)")
        self.config = dict(enabled=True, recipients=['ops@example.test'], events={'export': {'success': True, 'failure': True}, 'archive': {'success': True, 'failure': True}, 'restore': {'success': True, 'failure': True}})
        self.db.execute('INSERT OR REPLACE INTO notification_settings VALUES(1,1,?)', (json.dumps(self.config),))
        self.vtl = '00000000-0000-4000-8000-000000000001'

    def tearDown(self):
        self.db.close()

    def job(self, kind='export', job='one'):
        self.db.execute("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES(?,?,?,'running',?)", (job,kind,'CVT008L8-' + job,self.vtl))
        return job

    def events(self):
        return self.db.execute('SELECT payload,recipients FROM notification_events').fetchall()

    def test_direct_helper_update_captures_one_safe_event_and_recipient_snapshot(self):
        self.job()
        self.db.execute("UPDATE jobs SET status='completed',message='secret-token-do-not-mail' WHERE id='one'")
        rows=self.events()
        self.assertEqual(len(rows), 1)
        self.assertNotIn('secret-token', rows[0][0])
        self.assertEqual(json.loads(rows[0][1]), ['ops@example.test'])
        self.config['recipients']=['other@example.test']
        self.db.execute('UPDATE notification_settings SET configuration=?', (json.dumps(self.config),))
        self.db.execute("UPDATE jobs SET status='completed' WHERE id='one'")
        self.assertEqual(self.events(), rows)

    def test_disabled_does_not_backfill(self):
        self.config['enabled']=False
        self.db.execute('UPDATE notification_settings SET configuration=?', (json.dumps(self.config),))
        self.job()
        self.db.execute("UPDATE jobs SET status='failed' WHERE id='one'")
        self.assertEqual(self.events(), [])
        self.config['enabled']=True
        self.db.execute('UPDATE notification_settings SET configuration=?', (json.dumps(self.config),))
        self.assertEqual(self.events(), [])

    def test_archive_acceptance_is_not_completion(self):
        self.job('archive_prepare','prep');self.job('archive_submit')
        self.db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id) VALUES('one','prep','queued','{}','archive',?)",(self.vtl,))
        self.db.execute("UPDATE archive_requests SET phase='accepted'")
        self.db.execute("UPDATE jobs SET status='completed'")
        self.assertEqual(self.events(), [])
        self.db.execute("UPDATE archive_requests SET phase='archived'")
        self.assertEqual(len(self.events()),1)
        self.db.execute("UPDATE archive_requests SET phase='archived'")
        self.assertEqual(len(self.events()),1)

    def test_filter_and_nonterminal_noise(self):
        self.config['events']['export']['success']=False
        self.db.execute('UPDATE notification_settings SET configuration=?',(json.dumps(self.config),))
        self.job();self.job('archive_refresh','poll')
        self.db.execute("UPDATE jobs SET status='completed'")
        self.assertEqual(self.events(),[])
        self.job('restore','restore')
        self.db.execute("UPDATE jobs SET status='failed' WHERE id='restore'")
        self.assertEqual(len(self.events()),1)

    def test_submission_failure_without_provider_row_is_not_lost(self):
        self.job('archive_submit')
        self.db.execute("UPDATE jobs SET status='failed'")
        self.assertEqual(len(self.events()),1)

    def test_uncertain_provider_then_failed_job_sends_one_alert(self):
        self.job('archive_prepare','prep');self.job('archive_submit')
        self.db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id) VALUES('one','prep','submitting','{}','archive',?)",(self.vtl,))
        self.db.execute("UPDATE archive_requests SET phase='uncertain'")
        self.db.execute("UPDATE jobs SET status='failed' WHERE id='one'")
        self.assertEqual(len(self.events()),1)
        self.assertEqual(json.loads(self.events()[0][0])['outcome'],'uncertain')

    def test_daemon_failure_during_cloud_submission_is_uncertain_once(self):
        self.job('archive_prepare','prep');self.job('archive_submit')
        self.db.execute("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id) VALUES('one','prep','submitting','{}','archive',?)",(self.vtl,))
        self.db.execute("UPDATE jobs SET status='failed' WHERE id='one'")
        self.assertEqual(json.loads(self.events()[0][0])['outcome'],'uncertain')
        self.db.execute("UPDATE archive_requests SET phase='uncertain'")
        self.assertEqual(len(self.events()),1)

    def test_destination_bucket_is_snapshotted_from_pinned_revision(self):
        destination='00000000-0000-4000-8000-000000000002'
        self.db.execute('INSERT INTO storage_destination_revisions VALUES(?,1,?,CURRENT_TIMESTAMP)',(destination,json.dumps({'bucket':'archive-october','secret_access_key':'NEVER-SEND'})))
        self.job()
        self.db.execute("UPDATE jobs SET destination_id=?,configuration_revision=1 WHERE id='one'",(destination,))
        self.db.execute("UPDATE jobs SET status='completed' WHERE id='one'")
        payload=json.loads(self.events()[0][0])
        self.assertEqual(payload.get('bucket'),'archive-october')
        self.assertNotIn('NEVER-SEND',str(payload))
        self.db.execute('INSERT INTO storage_destination_revisions VALUES(?,2,?,CURRENT_TIMESTAMP)',(destination,json.dumps({'bucket':'archive-november'})))
        self.assertEqual(json.loads(self.events()[0][0])['bucket'],'archive-october')

if __name__=='__main__':unittest.main()
