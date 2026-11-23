"""Local stand-in for the PBS maintenance guard, for clients without a guard.

It records which lease owns the VTL so every existing lease check keeps its
meaning. It cannot see the backup client: callers obtain an explicit operator
confirmation before acquiring it for maintenance."""
from contextlib import contextmanager
import fcntl
import json
import re
from pathlib import Path
from vtl_runtime import atomic

ACTIONS = ('status', 'acquire', 'bootstrap', 'verify', 'release')


class LocalLease:
    def __init__(self, root, vtl_id):
        self.root, self.vtl_id = Path(root), vtl_id
        self.path = self.root/'client-lease.json'

    @contextmanager
    def lock(self):
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        with (self.root/'client-lease.lock').open('a+') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            yield

    def read(self):
        if not self.path.exists():
            return None
        try:
            value = json.loads(self.path.read_text())
        except ValueError:
            raise ValueError('Local lease unreadable for this VTL.') from None
        if set(value) != {'vtl_id', 'lease'} or value['vtl_id'] != self.vtl_id or not (value['lease'] is None or isinstance(value['lease'], str)):
            raise ValueError('Local lease is invalid or belongs to another VTL.')
        return value['lease']

    def response(self, lease):
        return {'ok': True, 'vtl_id': self.vtl_id, 'registered': True, 'bootstrap': False, 'lease': lease, 'locked': lease is not None}

    def handle(self, action, lease=None, barcode=None):
        if action not in ACTIONS:
            raise ValueError('Unsupported lease action.')
        with self.lock():
            current = self.read()
            if action == 'status':
                return self.response(current)
            if not isinstance(lease, str) or not re.fullmatch(r'[0-9a-f-]{36}', lease):
                raise ValueError('Lease required.')
            if action in ('acquire', 'bootstrap'):
                if current not in (None, lease):
                    raise ValueError('Another lease holds this VTL.')
                atomic(self.path, {'vtl_id': self.vtl_id, 'lease': lease})
                return self.response(lease)
            if action == 'release':
                # Idempotent like the guard: only a foreign lease is refused.
                if current not in (None, lease):
                    raise ValueError('Local lease not confirmed for this VTL.')
                atomic(self.path, {'vtl_id': self.vtl_id, 'lease': None})
                return self.response(None)
            if current != lease:
                raise ValueError('Local lease not confirmed for this VTL.')
            return self.response(current)
