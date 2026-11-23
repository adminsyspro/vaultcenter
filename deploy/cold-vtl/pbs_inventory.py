"""Bounded PBS inventory queries, independent of the reader sampling loop."""
from concurrent.futures import ThreadPoolExecutor
import json
import re
import subprocess


def fetch_inventory(config, vtl_id):
    args = ['ssh', '-F', '/dev/null', '-i', config['key_file'],
            '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3',
            '-o', 'StrictHostKeyChecking=yes', '-o', 'IdentitiesOnly=yes',
            '-o', 'UserKnownHostsFile=' + config['known_hosts'],
            '--', config['pbs'], '--vtl', vtl_id, 'inventory']
    result = subprocess.run(args, capture_output=True, text=True, timeout=8, check=True)
    if len(result.stdout) > 1_048_576:
        raise ValueError('Oversized PBS inventory')
    return json.loads(result.stdout)


def validated_slots(value, vtl_id, now_ms):
    if not isinstance(value, dict) or value.get('ok') is not True or value.get('vtl_id') != vtl_id:
        raise ValueError('PBS inventory scope mismatch')
    stamp = value.get('observed_at')
    if type(stamp) is not int or stamp < 0 or not -5000 <= now_ms - stamp <= 15000:
        raise ValueError('Expired PBS inventory')
    slots = value.get('slots')
    if not isinstance(slots, list) or len(slots) > 4096:
        raise ValueError('Invalid PBS slots')
    numbers, barcodes = set(), set()
    for slot in slots:
        if not isinstance(slot, dict):
            raise ValueError('Invalid PBS slot')
        number, barcode = slot.get('number'), slot.get('barcode')
        if (type(number) is not int or not 1 <= number <= 4096 or number in numbers
                or slot.get('kind') not in ('storage', 'import_export')
                or slot.get('position_known') is not True or slot.get('observed_at') != stamp):
            raise ValueError('Invalid PBS slot position')
        if barcode is not None:
            if (not isinstance(barcode, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,31}', barcode)
                    or barcode in barcodes):
                raise ValueError('Invalid PBS barcode')
            barcodes.add(barcode)
        numbers.add(number)
    return [{k: s[k] for k in ('number', 'kind', 'barcode', 'position_known', 'observed_at')} for s in slots]


class InventoryPoller:
    def __init__(self, config, vtl_id):
        self.config, self.vtl_id = config, vtl_id
        self.executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix='pbs-inventory')
        self.pending, self.next_poll, self.slots = None, 0, []

    def poll(self, monotonic, now_ms):
        if self.pending is not None and self.pending.done():
            try:
                self.slots = validated_slots(self.pending.result(), self.vtl_id, now_ms)
            except Exception:
                self.slots = []
            self.pending = None
            self.next_poll = monotonic + 5
        if self.pending is None and monotonic >= self.next_poll:
            self.pending = self.executor.submit(fetch_inventory, self.config, self.vtl_id)
        return self.slots

    def close(self):
        self.executor.shutdown(wait=False, cancel_futures=True)
