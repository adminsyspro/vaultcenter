"""Durable, isolated bandwidth settings; projections are never authoritative."""
import json
import os
from pathlib import Path
import stat

from vtl_runtime import atomic, atomic_text, identifier, locked

FIELDS = {'schema', 'vtl_id', 'revision', 'tape_write_bps', 's3_bps'}


def directory(root, vtl_id):
    base = Path(root) / 'vtls'
    path = base / identifier(vtl_id)
    for item in (base, path, path / 'config'):
        if item.is_symlink():
            raise ValueError('Bandwidth limit directory belongs to another root.')
    return path


def rate(value):
    if value is not None and (type(value) is not int or not 1_000_000 <= value <= 100_000_000_000 or value % 1_000_000):
        raise ValueError('Enter a whole number from 1 to 100,000 MB/s or select Unlimited.')
    return value


def validate(value, vtl_id):
    if (not isinstance(value, dict) or set(value) != FIELDS or type(value['schema']) is not int
            or value['schema'] != 1 or value['vtl_id'] != vtl_id
            or type(value['revision']) is not int or not 1 <= value['revision'] < 2**63):
        raise ValueError('Bandwidth configuration is invalid or belongs to another VTL.')
    rate(value['tape_write_bps'])
    rate(value['s3_bps'])
    return value


def read_bandwidth(root: Path, vtl_id: str) -> dict:
    file = directory(root, vtl_id) / 'bandwidth.json'
    try:
        info = file.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022 or info.st_size > 4096:
            raise ValueError('Bandwidth configuration is not protected.')
        return validate(json.loads(file.read_text()), vtl_id)
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError('Bandwidth configuration is missing or unreadable; no limits were disabled.') from error


def project(root, value):
    folder = directory(root, value['vtl_id']) / 'config'
    atomic_text(folder / 'bandwidth-drive.conf',
                f"VAULTCENTER_BW1 {value['vtl_id']} {value['revision']} {value['tape_write_bps'] or 0}\n")


def initialize_bandwidth(root: Path, vtl_id: str) -> dict:
    folder = directory(root, vtl_id)
    with locked(folder / 'bandwidth.lock'):
        file = folder / 'bandwidth.json'
        marker = folder / 'bandwidth-initialized'
        if marker.exists() or file.exists() or file.is_symlink():
            value = read_bandwidth(root, vtl_id)
        else:
            value = dict(schema=1, vtl_id=vtl_id, revision=1, tape_write_bps=None, s3_bps=None)
            atomic(file, value)
        atomic(marker, {'schema': 1, 'vtl_id': vtl_id})
        project(root, value)
        return value


def reconcile_bandwidth(root: Path, vtl_id: str) -> dict:
    with locked(directory(root, vtl_id) / 'bandwidth.lock'):
        value = read_bandwidth(root, vtl_id)
        project(root, value)
        return value


def update_bandwidth(root: Path, vtl_id: str, expected_revision: int,
                     tape_write_bps: int | None, s3_bps: int | None) -> dict:
    rate(tape_write_bps)
    rate(s3_bps)
    folder = directory(root, vtl_id)
    with locked(folder / 'bandwidth.lock'):
        previous = read_bandwidth(root, vtl_id)
        if type(expected_revision) is not int or previous['revision'] != expected_revision:
            raise ValueError('Bandwidth revision changed; reload before saving.')
        value = validate(dict(previous, revision=expected_revision + 1,
                              tape_write_bps=tape_write_bps, s3_bps=s3_bps), vtl_id)
        atomic(folder / 'bandwidth.json', value)
        try:
            project(root, value)
        except (ValueError, OSError):
            return dict(value, application='pending', message='Setting saved; publication to the drive is unconfirmed.')
        return dict(value, application='published', message=None)


def tape_observation(value, sample, now_ms, identity):
    result = dict(state='pending', observed_revision=None)
    if (sample.get('version') != 1 or sample.get('vtl_id') != value['vtl_id']
            or type(sample.get('generated_at')) is not int or not -5000 <= now_ms-sample['generated_at'] <= 15000):
        return result
    drives = sample.get('drives')
    if not isinstance(drives, list) or len(drives) != 1:
        return result
    drive = drives[0]
    stamp = drive.get('bandwidth_observed_at')
    if (type(stamp) is not int or not -5000 <= now_ms-stamp <= 15000
            or (drive.get('bandwidth_pid'), drive.get('bandwidth_start')) != identity):
        return result
    result['observed_revision'] = drive.get('bandwidth_revision')
    if (drive.get('bandwidth_error') is False and drive.get('bandwidth_revision') == value['revision']
            and drive.get('tape_write_limit_bps') == (value['tape_write_bps'] or 0)):
        result['state'] = 'confirmed'
    return result


def bandwidth_status(registry, vtl_id):
    import time
    value = read_bandwidth(registry.root, vtl_id)
    result = dict(value, tape=dict(state='unavailable', observed_revision=None),
                  s3=dict(state='configured'), message=None)
    try:
        manifest = registry.current(vtl_id)
        if len(manifest['drives']) == 1:
            unit = registry.units(manifest)[1]
            properties = dict(line.split('=',1) for line in registry.run([
                'systemctl','show','--property=LoadState,ActiveState,MainPID',unit]).splitlines() if '=' in line)
            pid = int(properties.get('MainPID','0'))
            if properties.get('LoadState') == 'loaded' and properties.get('ActiveState') in ('inactive','failed') and pid == 0:
                result['tape']['state'] = 'stopped'
            elif properties.get('ActiveState') == 'active' and pid > 0:
                result['tape']['state'] = 'pending'
                proc = Path('/proc')/str(pid)
                start = (proc/'stat').read_text().rsplit(')',1)[1].split()[19]
                if (proc/'comm').read_text().strip() == 'vtltape':
                    file = Path('/run/vaultcenter/telemetry')/(vtl_id+'.json')
                    if file.stat().st_size <= 1_048_576:
                        result['tape'] = tape_observation(value,json.loads(file.read_text()),int(time.time()*1000),(pid,start))
    except (OSError, ValueError, KeyError, IndexError):
        pass
    for warning in directory(registry.root,vtl_id).glob('bandwidth-error-*.json'):
        try:
            data = json.loads(warning.read_text())
            if data.get('vtl_id') == vtl_id and type(data.get('pid')) is int and data['pid'] > 0 and Path('/proc',str(data['pid'])).exists():
                result['s3']['state'] = 'degraded'
        except (OSError,ValueError):
            result['s3']['state'] = 'degraded'
    return result


def require_parallel_host(hosts=Path('/sys/class/scsi_host')):
    """A one-command host serializes every VTL behind a throttled writer."""
    try:
        values=[int((host/'can_queue').read_text()) for host in hosts.glob('host*')
                if (host/'proc_name').read_text().strip()=='mhvtl']
        if not values or min(values)<128:raise ValueError()
    except (OSError,ValueError):
        raise ValueError('Update the host’s mhVTL module (shared queue of 128 commands) before enabling an independent PBS limit.') from None
