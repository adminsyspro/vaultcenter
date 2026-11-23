#!/usr/bin/env python3
"""Compile and exercise the shipped wait helper and kernel readiness functions."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--source', required=True, type=Path)
args = parser.parse_args()
source = args.source.resolve()
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    test = root / 'wait.c'
    test.write_text(r'''
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include "vtl_common.h"
static int capability, sleeps, poll_calls, injected_errno, injected_flags;
static useconds_t slept;
static int cap_ioctl(int fd, unsigned long request, ...) {
    assert(fd >= 0 && request == VTL_GET_CAPABILITIES);
    if (capability < 0) errno = ENOTTY;
    return capability;
}
static int fallback_sleep(useconds_t us) { sleeps++; slept=us; return 0; }
static int observed_poll(struct pollfd *p, nfds_t n, const struct timespec *t, const sigset_t *sig) {
    poll_calls++;
    if (injected_errno) { errno=injected_errno; return -1; }
    if (injected_flags) { p->revents=injected_flags; return 1; }
    return ppoll(p,n,t,sig);
}
#define ioctl cap_ioctl
#define usleep fallback_sleep
#define ppoll observed_poll
#include "vaultcenter_command_wait.h"
#undef ioctl
#undef usleep
#undef ppoll
static double now(void) { struct timespec t; assert(!clock_gettime(CLOCK_MONOTONIC,&t)); return t.tv_sec+t.tv_nsec/1e9; }
static void signal_received(int sig) { (void)sig; }
int main(void) {
    int p[2]; assert(!pipe(p)); struct vaultcenter_command_wait wait;
    /* An old driver must not be probed with poll: its default is always ready. */
    capability=-1; vaultcenter_command_wait_init(&wait,p[0]);
    vaultcenter_command_wait_for(&wait,p[0],400); assert(sleeps==1 && slept==400 && poll_calls==0);
    capability=0; vaultcenter_command_wait_init(&wait,p[0]);
    vaultcenter_command_wait_for(&wait,p[0],10); assert(sleeps==2 && slept==10 && poll_calls==0);
    capability=VTL_CAP_COMMAND_POLL; vaultcenter_command_wait_init(&wait,p[0]);
    double t=now(); vaultcenter_command_wait_for(&wait,p[0],20000); assert(now()-t>=.015 && sleeps==2);
    /* Already queued: immediate, level-triggered, nonconsuming. */
    assert(write(p[1],"x",1)==1); t=now();
    vaultcenter_command_wait_for(&wait,p[0],1000000); assert(now()-t<.2);
    vaultcenter_command_wait_for(&wait,p[0],1000000); char c; assert(read(p[0],&c,1)==1 && c=='x');
    /* Arrival after sleeping: wake before the timeout, leave command queued. */
    pid_t child=fork(); assert(child>=0);
    if (!child) { usleep(20000); assert(write(p[1],"y",1)==1); _exit(0); }
    t=now(); vaultcenter_command_wait_for(&wait,p[0],1000000); assert(now()-t<.5);
    int status; assert(waitpid(child,&status,0)==child && WIFEXITED(status) && !WEXITSTATUS(status));
    assert(read(p[0],&c,1)==1 && c=='y');
    /* Real EINTR returns promptly to message/signal handling, no extra sleep. */
    struct sigaction sa={0}; sa.sa_handler=signal_received; sigemptyset(&sa.sa_mask); assert(!sigaction(SIGALRM,&sa,NULL));
    ualarm(20000,0); t=now(); vaultcenter_command_wait_for(&wait,p[0],1000000); assert(now()-t<.5 && sleeps==2);
    /* Broken wait support downgrades permanently, avoiding spin/error loops. */
    injected_errno=ENOSYS; vaultcenter_command_wait_for(&wait,p[0],400); assert(sleeps==3 && slept==400);
    int polls=poll_calls; injected_errno=0; vaultcenter_command_wait_for(&wait,p[0],400); assert(poll_calls==polls && sleeps==4);
    int flags[]={POLLERR,POLLHUP,POLLNVAL};
    for (unsigned i=0;i<sizeof(flags)/sizeof(flags[0]);i++) {
        vaultcenter_command_wait_init(&wait,p[0]); injected_flags=flags[i];
        int before=sleeps; vaultcenter_command_wait_for(&wait,p[0],50); assert(sleeps==before+1 && slept==50);
        injected_flags=0; polls=poll_calls; vaultcenter_command_wait_for(&wait,p[0],50); assert(poll_calls==polls);
    }
    close(p[0]); close(p[1]);
    puts("PASS wait: old module, capability, timeout, prequeued/arriving data, EINTR, error fallback");
}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror',
                    '-I'+str(source/'include'),'-I'+str(source/'include/common'),
                    str(test),'-o',str(root/'wait')],check=True)
    subprocess.run([str(root/'wait')],check=True,timeout=5)

# Run the actual kernel functions with deterministic LU/list/lock adapters.
# This checks readiness logic and ownership; Linux lab tests cover real waitqueues.
import re
kernel = (source/'kernel/mhvtl.c').read_text()
def function(name):
    match = re.search(r'^static [^;{}]*\b'+name+r'\([^;{}]*\)\s*\{', kernel, re.M)
    if not match:
        raise AssertionError('Missing kernel implementation: '+name)
    end=match.end(); depth=1
    while depth:
        depth += (kernel[end]=='{')-(kernel[end]=='}'); end+=1
    return kernel[match.start():end]
with tempfile.TemporaryDirectory() as tmp:
    root=Path(tmp)
    (root/'kernel.c').write_text(r'''
#include <assert.h>
#include <errno.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#define DEF_MAX_MINOR_NO 4
#define CMD_STATE_QUEUED 1
#define CMD_STATE_IN_USE 2
#define EPOLLERR 8
#define EPOLLIN 1
#define EPOLLRDNORM 64
typedef unsigned int __poll_t;
typedef int poll_table;
struct inode { unsigned minor; };
struct file { struct inode *inode; };
#define file_inode(f) ((f)->inode)
#define iminor(i) ((i)->minor)
#define pr_debug(...) ((void)0)
struct list_head { struct list_head *next,*prev; };
struct mhvtl_queued_cmd { int state; struct list_head queued_sibling; };
struct scsi_device { int unused; };
struct mhvtl_lu_info { int cmd_list_lock,sdev_lock; struct list_head cmd_list; struct scsi_device *sdev; };
static struct mhvtl_lu_info *devp[DEF_MAX_MINOR_NO];
static int command_wait[DEF_MAX_MINOR_NO],mhvtl_hba_list_lock,poll_registered,arrival;
static unsigned long mhvtl_open_minors[1];
static int removed,put,failed;
static struct mhvtl_queued_cmd queued;
static void lock(int *l) { assert(!*l); *l=1; }
static void unlock(int *l) { assert(*l); *l=0; }
#define spin_lock_irqsave(l,f) do { f=0;lock(l); } while(0)
#define spin_unlock_irqrestore(l,f) do { (void)f;unlock(l); } while(0)
#define spin_lock(l) lock(l)
#define spin_unlock(l) unlock(l)
#define entry(p,t,m) ((t*)((char*)(p)-offsetof(t,m)))
#define list_for_each_entry(p,h,m) for (p=entry((h)->next,__typeof__(*p),m); &p->m!=(h); p=entry(p->m.next,__typeof__(*p),m))
static void queue_cmd(struct mhvtl_lu_info *lu) {
    queued.state=CMD_STATE_QUEUED;
    queued.queued_sibling.next=queued.queued_sibling.prev=&lu->cmd_list;
    lu->cmd_list.next=lu->cmd_list.prev=&queued.queued_sibling;
}
static void poll_wait(struct file *f,int *q,poll_table *t) {
    (void)t; assert(!mhvtl_hba_list_lock && q==&command_wait[f->inode->minor]);
    poll_registered++;
    if (arrival) {queue_cmd(devp[f->inode->minor]);arrival=0;}
}
static int test_and_set_bit(unsigned n,unsigned long *bits) { int old=(*bits>>n)&1;*bits|=1UL<<n;return old; }
static void clear_bit(unsigned n,unsigned long *bits) { *bits&=~(1UL<<n); }
static void mhvtl_fail_queued_cmnds(struct mhvtl_lu_info *lu) {(void)lu;failed++;}
static void scsi_remove_device(struct scsi_device *d) {(void)d;removed++;devp[0]=NULL;}
static void scsi_device_put(struct scsi_device *d) {(void)d;put++;}
''' + '\n'.join(function(n) for n in ('mhvtl_poll','mhvtl_open','mhvtl_release')) + r'''
int main(void) {
    struct inode i={0},j={1},bad={DEF_MAX_MINOR_NO};struct file f={&i},g={&j};
    poll_table pt=0; struct mhvtl_lu_info a={0},b={0};struct scsi_device sd={0};
    a.cmd_list.next=a.cmd_list.prev=&a.cmd_list;b.cmd_list.next=b.cmd_list.prev=&b.cmd_list;
    assert(!mhvtl_poll(&f,&pt)); /* absent LU: bounded idle, not permanent HUP */
    devp[0]=&a;devp[1]=&b;
    assert(!mhvtl_poll(&f,&pt));assert(poll_registered==2);
    queue_cmd(&a);assert(mhvtl_poll(&f,&pt)==(EPOLLIN|EPOLLRDNORM));
    assert(queued.state==CMD_STATE_QUEUED);assert(!mhvtl_poll(&g,&pt));
    queued.state=CMD_STATE_IN_USE;assert(!mhvtl_poll(&f,&pt));
    a.cmd_list.next=a.cmd_list.prev=&a.cmd_list;arrival=1;
    assert(mhvtl_poll(&f,&pt)==(EPOLLIN|EPOLLRDNORM)); /* arrival during registration */
    f.inode=&bad;assert(mhvtl_poll(&f,&pt)==EPOLLERR);f.inode=&i;
    assert(mhvtl_open(&bad,&f)==-ENODEV);
    assert(!mhvtl_open(&i,&f));assert(mhvtl_open(&i,&f)==-EBUSY);assert(!mhvtl_open(&j,&g));
    assert(!removed && !put && !failed);
    a.sdev=&sd;assert(!mhvtl_release(&i,&f));assert(removed==1 && put==1 && failed==1);
    assert(!mhvtl_open(&i,&f));assert(!mhvtl_release(&i,&f));
    assert(removed==1 && put==1);assert(!mhvtl_release(&j,&g));
    assert(!mhvtl_open_minors[0]);
    puts("PASS kernel: idle, queued/in-use, per-LU readiness, arrival ordering, exclusive ownership and reopen");
}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror','-Wno-unused-parameter',str(root/'kernel.c'),'-o',str(root/'kernel')],check=True)
    subprocess.run([str(root/'kernel')],check=True,timeout=5)
