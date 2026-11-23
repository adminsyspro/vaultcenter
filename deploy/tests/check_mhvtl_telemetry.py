#!/usr/bin/env python3
"""Compile the actual patched producer and check framing, idle and saturation."""
from pathlib import Path
import subprocess
import sys
import tempfile

source = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory() as temp:
    root = Path(temp)
    header = (source/'include/vtllib.h').read_text()
    enum = header[header.index('enum MHVTL_STATE {'):].split('};', 1)[0]+'};\n'
    library = (source/'usr/vtllib.c').read_text()
    mapping = library[library.index('const char *vaultcenter_state_description('):].split('\n}\n', 1)[0]+'\n}\n'
    # Older table is deliberately included: the test catches its shifted indexes.
    table = library[library.index('} state_desc[] = {')+2:].split('};', 1)[0]+'};\n'
    prelude = enum+'struct {const char *state_desc;} '+table+mapping
    (root/'test.c').write_text(prelude+r'''
#include "vaultcenter_telemetry.h"
#include <assert.h>
#include <fcntl.h>
int main(void) {
    assert(!strcmp(vaultcenter_state_description(MHVTL_STATE_WRITING), "Writing"));
    assert(!strcmp(vaultcenter_state_description(MHVTL_STATE_READING), "Reading"));
    assert(!strcmp(vaultcenter_state_description(MHVTL_STATE_LOADED_IDLE), "Loaded - Idle"));
    int fds[2]; assert(pipe(fds) == 0);
    assert(fcntl(fds[1], F_SETFL, O_NONBLOCK) == 0);
    assert(fcntl(fds[0], F_SETFL, O_NONBLOCK) == 0);
    FILE *out = fdopen(fds[1], "w"); char buf[1024];
    vaultcenter_status_at(out, 11, "Writing", "CVT001L8", 1, 100, 0, 1000, 1000);
    int n = read(fds[0], buf, sizeof(buf)-1); assert(n > 0); buf[n] = 0;
    assert(strstr(buf, "\"bytes_written\":100")); assert(strstr(buf, "\"state\":\"Writing\""));
    assert(buf[n-1] == '\n'); assert(n < 512);
    vaultcenter_status_at(out, 11, "Writing", "CVT001L8", 1, 100, 0, 1100, 1100);
    assert(read(fds[0], buf, sizeof(buf)) < 0 && errno == EAGAIN);
    vaultcenter_status_at(out, 11, "Writing", "CVT001L8", 1, 100, 0, 5000, 5000);
    n = read(fds[0], buf, sizeof(buf)-1); assert(n > 0); buf[n] = 0;
    assert(strstr(buf, "Loaded - Idle"));
    vaultcenter_status_at(out, 11, "Writing", "CVT001L8", 1, 200, 0, 6000, 6000);
    n = read(fds[0], buf, sizeof(buf)-1); assert(n > 0); buf[n] = 0;
    assert(strstr(buf, "\"state\":\"Writing\""));
    for (unsigned long i = 7; i < 100000; ++i)
        vaultcenter_status_at(out, 11, "Writing", "CVT001L8", 1, i*100, 0, i*1000, i*1000);
    fclose(out); close(fds[0]); return 0;
}
''')
    subprocess.run(['cc','-Wall','-Wextra','-Werror','-I'+str(source/'include'),str(root/'test.c'),'-o',str(root/'test')],check=True)
    subprocess.run([str(root/'test')],check=True,timeout=3)
print('PASS: real producer heartbeat, successful byte counter framing, idle timeout, throttling and saturated pipe')
