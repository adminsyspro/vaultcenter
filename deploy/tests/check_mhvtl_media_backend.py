#!/usr/bin/env python3
"""Exercise actual load/write/unload functions from the built pinned backend."""
import argparse
import ctypes
import fcntl
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument('--source', type=Path, required=True)
source = p.parse_args().source.resolve()
with tempfile.TemporaryDirectory(prefix='vaultcenter-lock-backend-') as tmp:
    root = Path(tmp)
    (root/'identity.c').write_text('char mhvtl_driver_name[]="media-lock-test";')
    subprocess.run(['cc', '-shared', '-fPIC', str(root/'identity.c'), '-o', str(root/'identity.so')], check=True)
    ctypes.CDLL(str(root/'identity.so'), mode=ctypes.RTLD_GLOBAL)
    lib = ctypes.CDLL(str(source/'usr/libvtlscsi.so'))
    (ctypes.c_char * 1025).in_dll(lib, 'home_directory').value = os.fsencode(root)
    env = {**os.environ, 'LD_LIBRARY_PATH': str(source/'usr')}
    for barcode in ('TEST001L8', 'TEST002L8'):
        subprocess.run([str(source/'usr/bin/mktape'), '-l', '10', '-m', barcode,
                        '-s', '32', '-t', 'data', '-d', 'LTO8', '-H', tmp],
                       env=env, check=True, stdout=subprocess.DEVNULL)
    def fingerprint():
        return {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in (root/'TEST001L8').iterdir()}
    before = fingerprint()
    sam = ctypes.c_ubyte(0)
    fd = os.open(root/'TEST001L8', os.O_RDONLY)
    fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
    assert lib.load_tape(b'TEST001L8', ctypes.byref(sam)) == 3
    # Real backend writes another medium while the first is protected for export.
    assert lib.load_tape(b'TEST002L8', ctypes.byref(sam)) == 0
    payload = ctypes.create_string_buffer(b'concurrent PBS tape data' * 100)
    assert lib.write_tape_block(payload, len(payload.raw), 0, None, 0, 0, 0, ctypes.byref(sam)) == 0
    lib.unload_tape(ctypes.byref(sam))
    assert (root/'TEST002L8'/'data.0').stat().st_size > 0
    assert fingerprint() == before
    fcntl.flock(fd, fcntl.LOCK_UN)
    assert lib.load_tape(b'TEST001L8', ctypes.byref(sam)) == 0
    try:
        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
    except BlockingIOError:
        pass
    else:
        raise AssertionError('loaded media was not locked by the engine')
    lib.unload_tape(ctypes.byref(sam))
    fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
    os.close(fd)
print('PASS actual mhVTL backend: export/load exclusion, write to another cartridge, unchanged exported files, unlock after unload')
