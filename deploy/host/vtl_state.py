"""Durable per-library state and process locks shared with media helpers."""
from contextlib import closing, contextmanager
import fcntl
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import uuid


def identifier(value):
    if not isinstance(value, str): raise ValueError('Invalid VTL identifier.')
    try: parsed = str(uuid.UUID(value))
    except (ValueError, AttributeError): raise ValueError('Invalid VTL identifier.') from None
    if value != parsed: raise ValueError('Invalid VTL identifier.')
    return parsed


def atomic(path, value):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(prefix='.'+path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(value, stream); stream.flush(); os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary): os.unlink(temporary)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try: os.fsync(fd)
    finally: os.close(fd)


@contextmanager
def file_lock(path):
    with Path(path).open('a+') as stream:
        try: fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: raise ValueError('An operation is in progress on this resource.') from None
        try: yield
        finally: fcntl.flock(stream, fcntl.LOCK_UN)


class VtlState:
    def __init__(self, root, state_dir):
        self.root, self.state_dir = Path(root), Path(state_dir)
        (self.root/'vtls').mkdir(parents=True, exist_ok=True, mode=0o700)

    def validate(self, vtl_id):
        vtl_id = identifier(vtl_id)
        with closing(sqlite3.connect((self.state_dir/'catalog.db').as_uri()+'?mode=ro', uri=True)) as db:
            if not db.execute('SELECT 1 FROM vtls WHERE id=?',(vtl_id,)).fetchone():
                raise ValueError('Unknown VTL library.')
        return vtl_id

    def path(self, vtl_id):
        path = self.root/'vtls'/self.validate(vtl_id)
        path.mkdir(mode=0o700, exist_ok=True)
        return path

    def operation_root(self, vtl_id):
        path = self.state_dir/'vtls'/self.validate(vtl_id)
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        return path

    def operation_lock(self, vtl_id):
        return file_lock(self.operation_root(vtl_id)/'operation.lock')

    def registry_lock(self): return file_lock(self.root/'registry.lock')

    def load(self, vtl_id):
        path = self.path(vtl_id)/'storage.json'
        value = json.loads(path.read_text()) if path.exists() else {
            'mode':'managed','vtl_id':vtl_id,'phase':'unconfigured','revision':0,
            'path':None,'identity':None,'operation':None,'message':None}
        if value['vtl_id'] != vtl_id: raise ValueError('Storage state belongs to another VTL.')
        value['data_locked'] = (self.operation_root(vtl_id)/'data-locked').exists()
        return value

    def mark_data(self, vtl_id):
        path = self.operation_root(vtl_id)/'data-locked'
        try: fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        except FileExistsError: return
        try: os.write(fd,b'{"data_locked":true}\n'); os.fsync(fd)
        finally: os.close(fd)
        sync_directory(path.parent)

    def lock_data(self, vtl_id):
        with self.operation_lock(vtl_id): self.mark_data(vtl_id)

    def save(self, vtl_id, value):
        if value.get('vtl_id') != vtl_id: raise ValueError('Invalid VTL state.')
        with file_lock(self.path(vtl_id)/'state.lock'):
            if value.get('data_locked'): self.mark_data(vtl_id)
            value = {**value, 'data_locked':(self.operation_root(vtl_id)/'data-locked').exists()}
            atomic(self.path(vtl_id)/'storage.json',value)
