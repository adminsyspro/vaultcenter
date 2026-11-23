#!/usr/bin/env python3
"""Run a worker in its own VTL cgroup, with a fresh material identity check."""
import os
import re
import sys
from pathlib import Path
from vtl_runtime import ROOT, worker_runtime, worker_launch, storage_request


def main():
    args=sys.argv[1:]
    vtl=os.environ.get('COLD_VTL_VTL_ID')
    revision=int(os.environ['COLD_VTL_RUNTIME_REVISION'])
    if args[:1]!=['--execute']:
        command=worker_launch(vtl,revision,args)
        for variable,operation in [('COLD_VTL_EXPORT_JOB_ID','export-directory'),('COLD_VTL_RESTORE_JOB_ID','restore')]:
            job=os.environ.get(variable)
            if job is not None and args[:1]==[operation]:
                if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,119}',job):
                    raise ValueError('Invalid transfer identifier.')
                executable=command.index('/usr/local/libexec/cold-vtl-worker-run')
                if command[executable+1:executable+2]!=['--execute']:
                    raise ValueError('Invalid worker command.')
                command.insert(executable,'--setenv='+variable+'='+job)
        os.execvp(command[0],command)
    args=args[1:]
    manifest=worker_runtime(vtl,revision)
    if not args:raise ValueError('Worker command missing.')
    # All local paths supplied by the API must remain within this manifest.
    paths={'--media-root':manifest['media_root'],'--source':manifest['media_root'],
           '--destination':manifest['restore_root']}
    for flag,root in paths.items():
        for i,arg in enumerate(args):
            if arg==flag:
                if i+1==len(args):raise ValueError('Worker path missing.')
                path=Path(args[i+1]);base=Path(root)
                if not path.is_absolute() or '..' in path.parts or not path.is_relative_to(base):
                    raise ValueError('Worker path outside this VTL.')
                if path.exists() and not path.resolve().is_relative_to(base.resolve()):
                    raise ValueError('Worker path belongs to another root.')
    if any(flag in args for flag in ('--media-root','--source')):
        if storage_request(vtl).get('phase')!='ready':raise ValueError('Storage unavailable.')
    os.environ['COLD_VTL_DATABASE']=str(ROOT/'catalog.db')
    os.execv('/usr/local/bin/cold-vtl-worker',['cold-vtl-worker',*args])

if __name__=='__main__':
    try:main()
    except Exception as error:
        print(str(error),file=sys.stderr);sys.exit(1)
