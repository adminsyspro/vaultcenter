"""Optional export history identity survives systemd without changing qualification."""
import os
import sys
import unittest
from unittest.mock import patch
from test_maintenance import module

worker = module('worker_history', 'cold-vtl/worker-run.py')
class Executed(Exception): pass

class WorkerHistoryTests(unittest.TestCase):
    def invoke(self, job, operation="export-directory", variable="COLD_VTL_EXPORT_JOB_ID"):
        args=['worker-run',operation,'--barcode','TAPE01']
        command=['systemd-run','--quiet','--setenv=COLD_VTL_VTL_ID=vtl-a','/usr/local/libexec/cold-vtl-worker-run','--execute',*args[1:]]
        with patch.dict(os.environ,{'COLD_VTL_VTL_ID':'vtl-a','COLD_VTL_RUNTIME_REVISION':'2',variable:job,'AWS_SECRET_ACCESS_KEY':'must-not-forward'}), patch.object(sys,'argv',args), patch.object(worker,'worker_launch',return_value=command), patch.object(worker.os,'execvp',side_effect=Executed) as execute:
            try: worker.main()
            except Executed: pass
            return execute.call_args.args[1]

    def test_export_job_crosses_boundary_as_one_environment_argument(self):
        command=self.invoke('export-123')
        self.assertIn('--setenv=COLD_VTL_EXPORT_JOB_ID=export-123',command)
        self.assertLess(command.index('--setenv=COLD_VTL_EXPORT_JOB_ID=export-123'),command.index('/usr/local/libexec/cold-vtl-worker-run'))
        self.assertFalse(any('must-not-forward' in arg for arg in command))

    def test_invalid_identity_never_launches_service(self):
        for value in ['--property=User=root','x\ny','x y','x'*121]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.invoke(value)

    def test_restore_job_crosses_boundary_without_export_identity(self):
        command=self.invoke('recovery-123','restore','COLD_VTL_RESTORE_JOB_ID')
        self.assertIn('--setenv=COLD_VTL_RESTORE_JOB_ID=recovery-123',command)
        self.assertFalse(any('COLD_VTL_EXPORT_JOB_ID' in arg for arg in command))
        for value in ['x y','x\ny','--unsafe']:
            with self.assertRaises(ValueError):self.invoke(value,'restore','COLD_VTL_RESTORE_JOB_ID')
