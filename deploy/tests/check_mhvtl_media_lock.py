#!/usr/bin/env python3
"""Real flock contention and process death against the shipped engine helper."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument('--source', type=Path, required=True)
args = p.parse_args()
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    (root/'TAPE01').mkdir()
    (root/'TAPE02').mkdir()
    (root/'lock.c').write_text(r'''
#include <assert.h>
#include <sys/wait.h>
#include <signal.h>
#include "vaultcenter_media_lock.h"
int main(int argc, char **argv) {
 assert(argc==3);
 int drive=vaultcenter_media_lock(argv[1]); assert(drive>=0);
 int reader=open(argv[1],O_RDONLY|O_DIRECTORY); assert(reader>=0);
 assert(flock(reader,LOCK_SH|LOCK_NB)<0);
 int other=vaultcenter_media_lock(argv[2]); assert(other>=0); close(other);
 close(drive); assert(flock(reader,LOCK_SH|LOCK_NB)==0);
 assert(vaultcenter_media_lock(argv[1])<0);
 int worker=open(argv[1],O_RDONLY|O_DIRECTORY); assert(worker>=0);
 assert(flock(worker,LOCK_SH|LOCK_NB)==0); close(reader);
 assert(vaultcenter_media_lock(argv[1])<0); close(worker);
 int pipefd[2]; assert(pipe(pipefd)==0);
 pid_t child=fork(); assert(child>=0);
 if(!child) { close(pipefd[0]); int fd=vaultcenter_media_lock(argv[1]); assert(fd>=0);
   assert(write(pipefd[1],"x",1)==1); pause(); _exit(1); }
 close(pipefd[1]); char signal; assert(read(pipefd[0],&signal,1)==1);
 assert(vaultcenter_media_lock(argv[1])<0);
 assert(kill(child,SIGKILL)==0); int status; assert(waitpid(child,&status,0)==child);
 drive=vaultcenter_media_lock(argv[1]); assert(drive>=0); close(drive);
 return 0;
}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror',
                    '-I'+str(args.source.resolve()/'include'),str(root/'lock.c'),'-o',str(root/'lock')],check=True)
    subprocess.run([str(root/'lock'),str(root/'TAPE01'),str(root/'TAPE02')],check=True,timeout=10)
print('PASS engine media lock: mutual exclusion, independent tapes, worker overlap, crash release')
