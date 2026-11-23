#!/usr/bin/env python3
"""Read-only mhVTL observations, its slot map, and optional asynchronous PBS slot inventory."""
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import uuid
import time
from datetime import datetime

BARCODE = re.compile(r'[A-Za-z0-9][A-Za-z0-9_-]{0,31}\Z')
EVENT = re.compile(r'^\w{3} \w{3} [ \d]\d \d\d:\d\d:\d\d \d{4} - (\d{1,6}): - ([^\r\n]+)\n$')
STATES = {'Initialising v2': 'unknown', 'Idle': 'idle', 'Unloading': 'unloading',
          'Loading': 'loading', 'Loading Cleaning Tape': 'loading',
          'Loading WORM media': 'loading', 'Loading NULL media': 'loading',
          'Loaded': 'loaded', 'Loaded - Idle': 'idle', 'Load failed': 'error',
          'Rewinding': 'loaded', 'Positioning': 'loaded', 'Locate': 'loaded',
          'Reading': 'reading', 'Writing': 'writing', 'Erasing': 'maintenance', 'Verifying': 'reading'}
ACTIVE = {'loading', 'unloading', 'writing', 'reading', 'maintenance'}


def parse_event(line):
    match = EVENT.fullmatch(line) if len(line) <= 4096 else None
    return (int(match[1]), match[2]) if match else None


def growth_rate(previous_bytes, current_bytes, elapsed_seconds):
    if elapsed_seconds <= 0 or current_bytes < previous_bytes:
        return None
    return (current_bytes - previous_bytes) / elapsed_seconds


def media_metadata(root, barcode):
    """Bounded dirfd traversal: symlinks/replacements cannot escape the root."""
    if not BARCODE.fullmatch(barcode):
        return None
    try:
        rootfd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            fd = os.open(barcode, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=rootfd)
        finally:
            os.close(rootfd)
        try:
            total, identity = 0, []
            with os.scandir(fd) as entries:
                for count, entry in enumerate(entries):
                    if count >= 128:
                        return None
                    info = entry.stat(follow_symlinks=False)
                    if not stat.S_ISREG(info.st_mode):
                        return None
                    total += info.st_size
                    identity.append((entry.name, info.st_dev, info.st_ino))
            return total, tuple(sorted(identity))
        finally:
            os.close(fd)
    except OSError:
        return None


def sample_media(root, barcode):
    result = media_metadata(root, barcode)
    return result[0] if result else None


def open_media(drive, root, proc_root):
    """Identify one configured daemon by PID + Linux starttime, then its open spool.

    Missing/ambiguous evidence is unknown. Reading proc metadata does not read
    the media's contents. The second starttime check detects a concurrent exit.
    """
    try:
        pid = int(Path(drive['pid_file']).read_text()[:32].strip())
        if pid <= 0:
            return None, None
        proc = proc_root / str(pid)
        start = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
        if (proc / 'comm').read_text().strip() != 'vtltape':
            return None, None
        found = set()
        with os.scandir(proc / 'fd') as entries:
            for count, entry in enumerate(entries):
                if count >= 256:
                    return None, None
                target = Path(os.readlink(entry.path))
                if target.name.startswith('data.') and target.parent.parent == root:
                    barcode = target.parent.name
                    if BARCODE.fullmatch(barcode) and media_metadata(root, barcode) is not None:
                        found.add(barcode)
        if (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19] != start:
            return None, None
        return (pid, start), next(iter(found)) if len(found) == 1 else None
    except (OSError, ValueError, KeyError, IndexError):
        return None, None


class Collector:
    def __init__(self, config):
        self.config = config
        self.vtl_id = str(uuid.UUID(config['vtl_id']))
        self.root = Path(config['media_root']).absolute()
        self.drives = {}
        self.identities = {}
        self.pending = {}
        self.counters = {}
        self.boundaries = {}
        self.started_at_ms = int(time.time() * 1000)
        self.baselines = {}
        self.media = []
        self.slots = []
        self.inventory = None
        if 'pbs' in config:
            from pbs_inventory import InventoryPoller
            self.inventory = InventoryPoller(config['pbs'], self.vtl_id)
        self.observed_at = None
        # Without a backup-client inventory (Veeam), positions come from the
        # library's own slot map on the FIFO: no SCSI command reaches the changer.
        self.map_parts = {}
        pid_file = config.get('library_pid_file')
        if pid_file is None and 'fifo' in config:
            pid_file = str(Path(config['fifo']).parent / f"pid-{int(config['library_queue_id'])}")
        self.library_pid_file = pid_file
        for drive in config['drives']:
            queue = int(drive['queue_id'])
            if queue in self.drives or len(self.drives) >= 64:
                raise ValueError('duplicate or excessive drive definitions')
            self.boundaries[queue] = self.started_at_ms
            self.drives[queue] = dict(id=str(queue), label=drive['label'], barcode=None,
                origin_slot=None, state='unknown', observed_at=None,
                bytes_written=None, write_rate_bps=None, bandwidth_revision=None,
                         tape_write_limit_bps=None, bandwidth_error=None, bandwidth_observed_at=None)

    def invalidate(self):
        self.started_at_ms = int(time.time() * 1000)
        self.boundaries = {queue: self.started_at_ms for queue in self.drives}
        self.pending.clear()
        self.counters.clear()
        self.baselines.clear()
        self.identities.clear()
        for drive in self.drives.values():
            drive.update(state='unknown', barcode=None, origin_slot=None, observed_at=None,
                         bytes_written=None, write_rate_bps=None, bandwidth_revision=None,
                         tape_write_limit_bps=None, bandwidth_error=None, bandwidth_observed_at=None)
        self.observed_at = None

    def accept_heartbeat(self, line, now_ms):
        try:
            data = json.loads(line)
            if not isinstance(data, dict) or data.get('vaultcenter') != 1:
                return
            for key in ('queue', 'pid', 'at_ms', 'mono_ms', 'generation', 'bytes_written'):
                if type(data.get(key)) is not int or not 0 <= data[key] < 2**64:
                    return
            queue = data['queue']
            identity = self.identities.get(queue)
            if queue not in self.drives or identity is None or identity[0] != data['pid']:
                return
            drive = self.drives[queue]
            stamp = data['at_ms']
            if (stamp < self.boundaries[queue] or not -5000 <= now_ms - stamp <= 15000
                    or data.get('barcode') != drive['barcode'] or data.get('state') not in STATES):
                return
            previous = self.counters.get(queue)
            if previous and (stamp <= previous['at_ms'] or data['mono_ms'] <= previous['mono_ms']):
                return
            rate = None
            if (previous and data['generation'] == previous['generation']
                    and 0 < data['mono_ms'] - previous['mono_ms'] <= 5000
                    and 0 < stamp - previous['at_ms'] <= 5000):
                rate = growth_rate(previous['bytes_written'], data['bytes_written'],
                                   (data['mono_ms'] - previous['mono_ms']) / 1000)
            drive.update(bandwidth_revision=None, tape_write_limit_bps=None,
                         bandwidth_error=None, bandwidth_observed_at=None)
            revision, limit, fault = (data.get(k) for k in
                                     ('bandwidth_revision', 'tape_write_limit_bps', 'bandwidth_error'))
            if (type(revision) is int and 0 < revision < 2**63 and type(limit) is int
                    and (limit == 0 or 1_000_000 <= limit <= 100_000_000_000 and limit % 1_000_000 == 0)
                    and type(fault) is bool):
                drive.update(bandwidth_revision=revision, tape_write_limit_bps=limit,
                             bandwidth_error=fault, bandwidth_observed_at=stamp,
                             bandwidth_pid=identity[0], bandwidth_start=identity[1])
            self.counters[queue] = data
            drive.update(state=STATES[data['state']], observed_at=stamp,
                         bytes_written=data['bytes_written'] if drive['barcode'] else None,
                         write_rate_bps=rate if drive['barcode'] else None)
            self.observed_at = stamp
        except (ValueError, TypeError, KeyError):
            return

    def library_pid(self):
        try:
            text = Path(self.library_pid_file).read_text()
        except (OSError, TypeError, ValueError):
            return None
        return int(text) if re.fullmatch(r'[1-9][0-9]{0,9}\n?', text) else None

    def accept_slot_map(self, data, now_ms):
        if self.inventory is not None or data.get('queue') != self.config['library_queue_id']:
            return
        for key in ('pid', 'at_ms', 'slot_map', 'part', 'parts'):
            if type(data.get(key)) is not int or not 0 <= data[key] < 2**63:
                return
        stamp, parts, part = data['at_ms'], data['parts'], data['part']
        if (not 1 <= parts <= 43 or part >= parts or not -5000 <= now_ms - stamp <= 15000
                or data['pid'] != self.library_pid() or not isinstance(data.get('slots'), list)):
            return
        entries = []
        for entry in data['slots']:
            if (not isinstance(entry, list) or len(entry) != 3 or entry[0] not in (2, 3)
                    or type(entry[1]) is not int or not 0 <= entry[1] < 2**32
                    or not (entry[2] is None or isinstance(entry[2], str) and BARCODE.fullmatch(entry[2]))):
                return
            entries.append(tuple(entry))
        key = (data['pid'], data['slot_map'])
        received = self.map_parts.setdefault(key, dict(parts=parts, at_ms=stamp, chunks={}))
        if received['parts'] != parts or received['at_ms'] != stamp:
            self.map_parts.pop(key, None)
            return
        received['chunks'][part] = entries
        # Keep only the newest map in flight; older incomplete ones are dropped.
        for other in [k for k in self.map_parts if k != key and (k[0] != key[0] or k[1] < key[1])]:
            self.map_parts.pop(other)
        if len(received['chunks']) < parts:
            return
        self.map_parts.pop(key)
        entries = [e for i in range(parts) for e in received['chunks'][i]]
        addresses = [e[1] for e in entries]
        barcodes = [e[2] for e in entries if e[2] is not None]
        if len(entries) > 4096 or len(set(addresses)) != len(addresses) or len(set(barcodes)) != len(barcodes):
            return
        storage = sorted((e for e in entries if e[0] == 2), key=lambda e: e[1])
        exchange = sorted((e for e in entries if e[0] == 3), key=lambda e: e[1])
        # Element order, as the changer reports it: storage slots first, then the import/export cart.
        self.slots = [dict(number=number, kind='storage' if kind == 2 else 'import_export', barcode=barcode,
                           position_known=True, observed_at=stamp)
                      for number, (kind, _, barcode) in enumerate(storage + exchange, start=1)]

    def accept(self, line, now_ms):
        if line.startswith('{'):
            if len(line) <= 4096:
                try:
                    data = json.loads(line)
                except ValueError:
                    return
                if isinstance(data, dict) and data.get('vaultcenter') == 1 and 'slot_map' in data:
                    self.accept_slot_map(data, now_ms)
                else:
                    self.accept_heartbeat(line, now_ms)
            return
        event = parse_event(line)
        if not event:
            return
        queue, message = event
        try:
            # ctime is in the producer's local timezone, on this same host.
            # Receipt time must never turn a retained FIFO backlog into live I/O.
            emitted_at = int(datetime.strptime(line.split(' - ', 1)[0], '%a %b %d %H:%M:%S %Y').timestamp() * 1000)
        except (ValueError, OverflowError, OSError):
            return
        boundary = self.boundaries.get(queue, self.started_at_ms)
        if emitted_at < boundary or now_ms - emitted_at > 15000 or emitted_at > now_ms + 5000:
            return
        if queue == self.config['library_queue_id']:
            match = re.fullmatch(r'Moving ([A-Za-z0-9_-]{1,32}) from Storage slot (\d{1,4}) to drive (\d{1,4})', message)
            if match:
                for drive in self.config['drives']:
                    if drive['ordinal'] == int(match[3]) and BARCODE.fullmatch(match[1]):
                        self.pending[drive['queue_id']] = (match[1], int(match[2]), emitted_at)
            return
        if queue not in self.drives or message not in STATES:
            return
        if queue in self.counters or self.config.get('require_heartbeat', False):
            return
        drive = self.drives[queue]
        if message in {'Initialising v2', 'Load failed', 'Unloading', 'Loading'}:
            drive.update(barcode=None, origin_slot=None)
            self.pending.pop(queue, None)
            self.baselines.clear()
        drive.update(state=STATES[message], observed_at=emitted_at)
        self.observed_at = emitted_at

    def sample(self, now_ms, monotonic_seconds):
        if self.inventory is not None:
            self.slots = self.inventory.poll(monotonic_seconds, now_ms)
        for config in self.config['drives']:
            queue = config['queue_id']
            identity, barcode = open_media(config, self.root, Path(self.config.get('proc_root', '/proc')))
            drive = self.drives[queue]
            if identity != self.identities.get(queue):
                drive.update(state='unknown', observed_at=None, origin_slot=None,
                             bytes_written=None, write_rate_bps=None, bandwidth_revision=None,
                         tape_write_limit_bps=None, bandwidth_error=None, bandwidth_observed_at=None)
                self.counters.pop(queue, None)
                self.boundaries[queue] = now_ms
                self.pending.pop(queue, None)
                self.baselines.clear()
            self.identities[queue] = identity
            if barcode != drive['barcode']:
                drive.update(state='unknown', observed_at=None, origin_slot=None,
                             bytes_written=None, write_rate_bps=None, bandwidth_revision=None,
                         tape_write_limit_bps=None, bandwidth_error=None, bandwidth_observed_at=None)
                self.counters.pop(queue, None)
                self.boundaries[queue] = now_ms
                self.baselines.clear()
            drive['barcode'] = barcode
            pending = self.pending.get(queue)
            if pending and barcode == pending[0] and 0 <= now_ms - pending[2] <= 15000:
                drive['origin_slot'] = pending[1]
        observations = []
        try:
            with os.scandir(self.root) as entries:
                for count, entry in enumerate(entries):
                    if count >= 1024:
                        break
                    if not BARCODE.fullmatch(entry.name) or not entry.is_dir(follow_symlinks=False):
                        continue
                    result = media_metadata(self.root, entry.name)
                    if result is None:
                        self.baselines.pop(entry.name, None)
                        continue
                    size, identity = result
                    previous = self.baselines.get(entry.name)
                    rate = growth_rate(previous[0], size, monotonic_seconds - previous[1]) if previous and previous[2] == identity else None
                    self.baselines[entry.name] = (size, monotonic_seconds, identity)
                    observations.append(dict(barcode=entry.name, observed_at=now_ms,
                        local_size_bytes=size, local_growth_bps=rate,
                        logical_used_bytes=None, logical_capacity_bytes=None))
        except OSError:
            self.baselines.clear()
        self.media = observations

    def snapshot(self, now_ms):
        drives = []
        for original in self.drives.values():
            drive = original.copy()
            observed = drive['observed_at']
            if observed is None or now_ms - observed > 15000 or observed > now_ms + 5000:
                drive.update(state='unknown', origin_slot=None, bytes_written=None, write_rate_bps=None, bandwidth_revision=None,
                         tape_write_limit_bps=None, bandwidth_error=None, bandwidth_observed_at=None)
            confirmed = drive.get('bandwidth_observed_at')
            if confirmed is None or not -5000 <= now_ms - confirmed <= 15000:
                drive.update(bandwidth_revision=None, tape_write_limit_bps=None, bandwidth_error=None)
            drives.append(drive)
        slots = [dict(s) for s in self.slots]
        slot_times = []
        for slot in slots:
            if not -5000 <= now_ms - slot['observed_at'] <= 15000:
                slot.update(position_known=False, barcode=None)
            else:
                slot_times.append(slot['observed_at'])
        # Local file sizes remain observable while tape services are stopped.
        # Missing drive/slot observations must not discard these independent samples.
        media_times = [m['observed_at'] for m in self.media
                       if -5000 <= now_ms - m['observed_at'] <= 10000]
        observed = max([self.observed_at or 0, *slot_times, *media_times]) or None
        available = observed is not None
        return dict(version=1, vtl_id=self.vtl_id, generated_at=now_ms,
            observed_at=observed, source='mhvtl',
            status='available' if available else 'unavailable',
            reason=None if available else 'source_missing', drives=drives,
            slots=slots, media=self.media)


def write_snapshot(path, snapshot):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(prefix='.telemetry-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(snapshot, stream, allow_nan=False, separators=(',', ':'))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class Framer:
    def __init__(self):
        self.buffer = b''
        self.dropping = False

    def feed(self, chunk):
        lines = []
        for part in chunk.splitlines(keepends=True):
            complete = part.endswith(b'\n')
            if self.dropping or len(self.buffer) + len(part) > 4096:
                self.buffer = b''
                self.dropping = not complete
                continue
            self.buffer += part
            if complete:
                lines.append(self.buffer.decode('ascii', errors='replace'))
                self.buffer = b''
        return lines


def run(config):
    import selectors
    import signal
    import time
    output = Path(config['output_dir'])
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    definitions = config['vtls']
    if len(definitions) > 64 or len({x['vtl_id'] for x in definitions}) != len(definitions) or len({x['fifo'] for x in definitions}) != len(definitions):
        raise ValueError('duplicate or excessive VTL streams')
    streams = [dict(collector=Collector(v), path=Path(v['fifo']), fd=None, framer=Framer()) for v in definitions]
    running = True
    def stop(_signum, _frame):
        nonlocal running
        running = False
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    with selectors.DefaultSelector() as selector:
        next_sample = 0
        try:
            while running:
                monotonic = time.monotonic()
                now = int(time.time() * 1000)
                if monotonic >= next_sample:
                    for stream in streams:
                        fd = stream['fd']
                        try:
                            info = stream['path'].lstat()
                            same = fd is not None and os.fstat(fd).st_ino == info.st_ino and os.fstat(fd).st_dev == info.st_dev
                            if not stat.S_ISFIFO(info.st_mode):
                                raise OSError('not FIFO')
                            if not same:
                                if fd is not None:
                                    selector.unregister(fd); os.close(fd); stream['fd'] = None
                                stream['collector'].invalidate(); stream['framer'] = Framer()
                                fd = os.open(stream['path'], os.O_RDWR | os.O_NONBLOCK | os.O_NOFOLLOW)
                                if not stat.S_ISFIFO(os.fstat(fd).st_mode):
                                    os.close(fd); raise OSError('not FIFO')
                                stream['fd'] = fd
                                selector.register(fd, selectors.EVENT_READ, stream)
                        except OSError:
                            if stream['fd'] is not None:
                                selector.unregister(stream['fd']); os.close(stream['fd']); stream['fd'] = None
                            stream['collector'].invalidate()
                        stream['collector'].sample(now, monotonic)
                        write_snapshot(output / (stream['collector'].vtl_id + '.json'), stream['collector'].snapshot(now))
                    next_sample = monotonic + 1
                for key, _mask in selector.select(timeout=min(.2, max(0, next_sample-time.monotonic()))):
                    stream = key.data
                    try:
                        chunk = os.read(key.fd, 65536)
                    except BlockingIOError:
                        continue
                    for line in stream['framer'].feed(chunk):
                        stream['collector'].accept(line, int(time.time()*1000))
        finally:
            for stream in streams:
                if stream['fd'] is not None:
                    os.close(stream['fd'])
                stream['collector'].invalidate()
                if stream['collector'].inventory is not None:
                    stream['collector'].inventory.close()
                stream['collector'].slots = []
                write_snapshot(output / (stream['collector'].vtl_id + '.json'), stream['collector'].snapshot(int(time.time()*1000)))


def load_config(path):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022 or info.st_size > 65536:
        raise ValueError('configuration must be a private, owned, regular file (max 64 KiB)')
    config = json.loads(path.read_text())
    definitions = config.get('vtls', [])
    # Read the existing binding without changing the qualified isolation runtime.
    manifest_path = path.with_name('runtime.json')
    if len(definitions) == 1 and 'pbs' not in definitions[0] and manifest_path.exists():
        info = manifest_path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022 or info.st_size > 65536:
            raise ValueError('runtime manifest must be a private, owned, regular file (max 64 KiB)')
        manifest = json.loads(manifest_path.read_text())
        if manifest.get('vtl_id') != definitions[0]['vtl_id']:
            raise ValueError('runtime manifest belongs to another VTL')
        if 'pbs' in manifest:
            definitions[0]['pbs'] = manifest['pbs']
    return config


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, type=Path)
    args = parser.parse_args()
    run(load_config(args.config))
