#!/usr/bin/env python3
"""Check generated service arguments with the pinned mhVTL option parsers.

Usage: python3 deploy/tests/check_mhvtl_launch.py PATH_TO_MHVTL_SOURCE
Run on Linux: mhVTL uses GNU getopt optional short-option arguments.
No tape daemons or SCSI commands are executed.
"""
from pathlib import Path
import json
import shlex
import re
import subprocess
import sys

from test_vtl_controller import A, B, VtlControllerTests


def check(source):
    fixture = VtlControllerTests()
    fixture.setUp()
    try:
        manifests = []
        for vtl_id in (A, B):
            prepared = fixture.r.prepare(vtl_id, 1)
            fixture.r.apply(vtl_id, prepared['token'], 'test')
            manifests.append(fixture.r.manifest(vtl_id))
        for binary in ('vtllibrary', 'vtltape'):
            text = (source / 'usr/cmd' / (binary + '.c')).read_text()
            start = text.index('\twhile ((opt = getopt(')
            end = text.index('\n\tif (my_id < 0)', start)
            code = '''#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#define MAXPRIOR 1024
static void usage(char *name) { (void)name; }
int main(int argc, char **argv) {
    int opt, debug=0, verbose=0, foreground=0, my_id=-1;
    char *fifoname=NULL, *progname=argv[0];
'''
            code += text[start:end]
            code += '\nputs(fifoname ? fifoname : "MISSING"); return 0; }\n'
            path = fixture.root / (binary + '.c')
            path.write_text(code)
            executable = fixture.root / binary
            subprocess.run(['cc', '-o', str(executable), str(path)], check=True)
            for manifest in manifests:
                unit = next(p for p in (fixture.root / 'units').glob('*.service')
                            if f'ExecStart=/usr/bin/{binary} ' in p.read_text()
                            and manifest['vtl_id'] in p.name)
                command = next(line.split('=', 1)[1] for line in unit.read_text().splitlines()
                               if line.startswith('ExecStart='))
                arguments = shlex.split(command)[1:]
                actual = subprocess.check_output([str(executable), *arguments], text=True).strip()
                config = json.loads((Path(manifest['state_root']) / 'telemetry.json').read_text())
                expected = config['vtls'][0]['fifo']
                assert actual == expected, f'{unit.name}: producer FIFO {actual!r}, collector FIFO {expected!r}'
        # Use the pinned daemon's own idle/exit thresholds to bound its stop time.
        # A low Backoff with the old 30 s unit deadline causes SIGKILL and stale locks.
        tape=(source/'usr/cmd/vtltape.c').read_text()
        header=(source/'include/vtllib.h').read_text()
        minimum=int(re.search(r'#define\s+MIN_SLEEP_TIME\s+(\d+)',header)[1])
        threshold=int(re.search(r'if \(sleep_time > (0x[0-9a-f]+)\)',tape)[1],16)
        for manifest in manifests:
            config=(Path(manifest['config_root'])/'device.conf').read_text().split('Drive: ',1)[1]
            backoff=int(re.search(r'Backoff: (\d+)',config)[1])
            assert backoff==10
            interval=minimum;elapsed=0
            while interval<=threshold:
                elapsed+=interval;interval+=backoff
            unit=(fixture.root/'units'/fixture.r.units(manifest)[1]).read_text()
            deadline=int(re.search(r'TimeoutStopSec=(\d+)',unit)[1])
            assert deadline>elapsed/1e6*1.2, (backoff,elapsed/1e6,deadline)
    finally:
        fixture.doCleanups()


if __name__ == '__main__':
    check(Path(sys.argv[1]))
    print('PASS: both mhVTL parsers receive the matching per-VTL collector FIFO')
