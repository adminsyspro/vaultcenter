#!/usr/bin/env python3
"""Compile the real rate-limiter header using an injected monotonic clock."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--source', required=True, type=Path)
args = parser.parse_args()
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    (root / 'test.c').write_text(r'''
#include <stdint.h>
#include <stdio.h>
#include <assert.h>
#include <string.h>
static uint64_t now_ns, max_sleep;
static int sleeps, change_during_sleep;
static const char *config_path;
static const char *id = "00000000-0000-4000-8000-000000000021";
static void config(unsigned long long revision, unsigned long long rate) {
    FILE *f = fopen(config_path,"w"); assert(f);
    fprintf(f,"VAULTCENTER_BW1 %s %llu %llu\n",id,revision,rate); fclose(f);
}
static uint64_t test_now(void) { return now_ns; }
static int test_sleep(uint64_t n) {
    assert(n > 0 && n <= 100000000);
    if (n > max_sleep) max_sleep = n;
    now_ns += n; sleeps++;
    if (change_during_sleep && now_ns >= 500000000) { config(2,100000000); change_during_sleep=0; }
    return 0;
}
#define VAULTCENTER_BW_NOW test_now
#define VAULTCENTER_BW_SLEEP test_sleep
#include "vaultcenter_bandwidth.h"
int main(int argc, char **argv) {
    assert(argc == 2); config_path=argv[1];
    /* invalid_config_blocks_start */
    assert(vaultcenter_bw_init(config_path,id) != 0);
    config(1,1000000); assert(vaultcenter_bw_init(config_path,id) == 0);
    /* low_rate_large_block_waits_interruptibly */
    vaultcenter_bw_begin(); assert(vaultcenter_bw_wait(2*1024*1024) == 0);
    assert(now_ns >= 2097152000ULL && now_ns < 2097154000ULL);
    assert(max_sleep <= 100000000 && sleeps >= 20);
    /* reload_raises_limit_during_wait */
    now_ns=0; sleeps=0; config(1,1000000);
    assert(vaultcenter_bw_init(config_path,id) == 0); change_during_sleep=1;
    vaultcenter_bw_begin(); assert(vaultcenter_bw_wait(2*1024*1024) == 0);
    assert(now_ns < 1200000000 && vaultcenter_bw.revision == 2);
    /* disabled_has_no_per_write_sleep */
    config(3,0); now_ns+=1000000000;
    assert(vaultcenter_bw_reload() == 0); sleeps=0;
    vaultcenter_bw_begin(); assert(vaultcenter_bw_wait(2097152) == 0); assert(sleeps == 0);
    /* Malformed and foreign settings preserve the previous effective cap. */
    config(4,1000000); now_ns+=1000000000; assert(vaultcenter_bw_reload() == 0);
    FILE *f=fopen(config_path,"w"); assert(f); fputs("VAULTCENTER_BW1 foreign 5 0\n",f); fclose(f);
    now_ns+=1000000000; assert(vaultcenter_bw_reload() != 0);
    assert(vaultcenter_bw.rate == 1000000 && vaultcenter_bw.revision == 4 && vaultcenter_bw.error);
    /* Stale revisions do not undo a newer cap. */
    config(3,0); now_ns+=1000000000; assert(vaultcenter_bw_reload() != 0);
    assert(vaultcenter_bw.rate == 1000000);
    return 0;
}
''')
    subprocess.run(['cc', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                    '-I' + str(args.source.resolve() / 'include'), str(root / 'test.c'),
                    '-o', str(root / 'test')], check=True)
    subprocess.run([str(root / 'test'), str(root / 'bandwidth.conf')], check=True, timeout=5)
print('PASS: invalid configuration, bounded sleeps, live changes, unlimited path and last valid settings')
