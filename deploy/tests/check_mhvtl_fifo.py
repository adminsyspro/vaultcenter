#!/usr/bin/env python3
"""Regression harness using the actual pinned upstream FIFO functions.
Usage: python3 deploy/tests/check_mhvtl_fifo.py PATH_TO_MHVTL_SOURCE
Requires cc and git. Runs on Linux (including a local build container).
"""
from pathlib import Path
import subprocess
import sys
import tempfile

source = Path(sys.argv[1])/'usr/vtllib.c'
patch = Path(__file__).parents[1]/'mhvtl/nonblocking-status-fifo.patch'
with tempfile.TemporaryDirectory() as temp:
    root = Path(temp); (root/'usr').mkdir()
    original = source.read_text(); (root/'usr/vtllib.c').write_text(original)
    def check(text, should_block):
        code = '''#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <string.h>
#include <time.h>
#define MHVTL_LOG(...) ((void)0)
#define MHVTL_DBG(...) ((void)0)
static struct { char *state_desc; } state_desc[] = {{"Writing"}};
'''
        code += text[text.index('int open_fifo('):text.index('/*\n * Pinched straight', text.index('int open_fifo('))]
        code += '''int main(int argc, char **argv) {
    FILE *stream = NULL;
    if (open_fifo(&stream, argv[1])) return 2;
    for (int i=0; i<100000; i++) { char *message=NULL; status_change(stream, 0, 11, &message); }
    fclose(stream); return 0;
}
'''
        (root/'check.c').write_text(code)
        subprocess.run(['cc','-o',str(root/'check'),str(root/'check.c')],check=True)
        fifo=root/'events'
        if fifo.exists(): fifo.unlink()
        try:
            subprocess.run([str(root/'check'),str(fifo)],check=True,timeout=2)
        except subprocess.TimeoutExpired:
            assert should_block, 'patched producer blocked'
        else:
            assert not should_block, 'original did not reproduce blocking regression'
    check(original, True)
    subprocess.run(['git','apply',str(patch.resolve())],cwd=root,check=True)
    check((root/'usr/vtllib.c').read_text(), False)
print('PASS: original producer blocks, patched producer tolerates absent reader and saturated FIFO')
