import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from test_telemetry import t, event as recorded_event, UUID, MODULE

def event(message):
    return recorded_event(message, at_ms=int(time.time()*1000))

class FifoTests(unittest.TestCase):
    def test_cli_split_burst_recreate_and_shutdown(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); fifo = root/'events'; os.mkfifo(fifo)
            config = root/'config.json'
            config.write_text(json.dumps(dict(output_dir=temp, vtls=[dict(vtl_id=UUID,
                fifo=str(fifo), media_root=temp, library_queue_id=10,
                drives=[dict(queue_id=11, ordinal=1, label='Reader')])])))
            process = subprocess.Popen(['python3', str(MODULE), '--config', str(config)], stderr=subprocess.PIPE)
            self.addCleanup(lambda: process.poll() is None and process.kill())
            cache = root/f'{UUID}.json'
            def wait_for(predicate):
                end = time.monotonic()+5
                while time.monotonic()<end:
                    if cache.exists() and predicate(json.loads(cache.read_text())):
                        return
                    if process.poll() is not None:
                        self.fail(f'collector exited: {process.stderr.read().decode()}')
                    time.sleep(.05)
                self.fail('snapshot deadline exceeded')
            wait_for(lambda s: s['status']=='unavailable')
            writer = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
            time.sleep(1.1)  # producer timestamps have whole-second resolution
            os.write(writer, event('Writing')[:-1].encode())
            time.sleep(1.1)
            self.assertEqual(json.loads(cache.read_text())['drives'][0]['state'], 'unknown')
            os.write(writer, b'\n')
            wait_for(lambda s: s['drives'][0]['state']=='writing')
            for _ in range(5000):
                try:
                    os.write(writer, event('Reading').encode())
                except BlockingIOError:
                    break
            os.close(writer)
            fifo.unlink(); os.mkfifo(fifo)
            wait_for(lambda s: s['drives'][0]['state']=='unknown')
            writer=os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
            time.sleep(1.1)
            os.write(writer, event('Loaded - Idle').encode()); os.close(writer)
            wait_for(lambda s: s['drives'][0]['state']=='idle')
            process.terminate(); process.wait(timeout=3)
            self.assertEqual(process.returncode, 0)
            process.stderr.close()

    def test_framer_drops_oversize_and_resynchronizes(self):
        f = t.Framer()
        self.assertEqual(f.feed(b'x'*9000), [])
        self.assertEqual(f.feed(b'\n'+event('Writing').encode()), [event('Writing')])
        self.assertEqual(f.feed(b'partial'), [])

    def test_retained_backlog_is_not_fresh_after_restart(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); fifo=root/'events'; os.mkfifo(fifo)
            writer=os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
            config=root/'config.json'
            config.write_text(json.dumps(dict(output_dir=temp,vtls=[dict(vtl_id=UUID,
                fifo=str(fifo),media_root=temp,library_queue_id=10,
                drives=[dict(queue_id=11,ordinal=1,label='Reader')])])))
            # RDWR is held by the real producer while no collector is running.
            os.write(writer, recorded_event('Writing',at_ms=int(time.time()*1000)-86400000).encode())
            process=subprocess.Popen(['python3',str(MODULE),'--config',str(config)],stderr=subprocess.PIPE)
            try:
                cache=root/f'{UUID}.json'
                deadline=time.monotonic()+5
                while not cache.exists() and time.monotonic()<deadline:
                    time.sleep(.05)
                time.sleep(1.2)  # include a publication tick after consuming backlog
                snapshot=json.loads(cache.read_text())
                self.assertEqual(snapshot['drives'][0]['state'],'unknown')
                self.assertIsNone(snapshot['drives'][0]['observed_at'])
            finally:
                process.terminate();process.wait(timeout=3);process.stderr.close();os.close(writer)
