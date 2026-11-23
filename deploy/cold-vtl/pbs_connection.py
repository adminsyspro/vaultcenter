"""Render scoped, repeatable PBS instructions from verified hardware identities."""
import ipaddress
import json
import re
import shlex
from pathlib import Path
import client_binding
from vtl_runtime import SERIAL_PREFIXES, atomic, identifier, locked, valid_iqn
from bandwidth import directory


def portal(host, port):
    if not isinstance(host, str) or not host or host != host.strip() or type(port) is not int or not 1 <= port <= 65535:
        raise ValueError('Enter an iSCSI host and a port between 1 and 65535.')
    try:
        address = ipaddress.ip_address(host)
        if '%' in host or getattr(address, 'scope_id', None):
            raise ValueError('Invalid IP address or DNS name; enter the port separately.')
        host = str(address)
        rendered = f'[{host}]' if address.version == 6 else host
    except ValueError:
        if len(host) > 253 or not all(re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?', label) for label in host.split('.')):
            raise ValueError('Invalid IP address or DNS name; enter the port separately.')
        host = host.lower()
        rendered = host
    return host, f'{rendered}:{port}'


def settings(root, vtl_id):
    path = directory(root, vtl_id) / 'pbs-connection.json'
    if not path.exists() and not path.is_symlink():
        return dict(schema=1, vtl_id=vtl_id, revision=0, host=None, port=3260)
    if path.is_symlink() or path.stat().st_size > 4096:
        raise ValueError('Invalid PBS connection configuration.')
    value = json.loads(path.read_text())
    if (set(value) != {'schema','vtl_id','revision','host','port'} or value['schema'] != 1
            or value['vtl_id'] != vtl_id or type(value['revision']) is not int or not 1 <= value['revision'] < 2**63):
        raise ValueError('Connection configuration is invalid or belongs to another VTL.')
    portal(value['host'], value['port'])
    return value


def connection_info(registry, vtl_id):
    identifier(vtl_id)
    with registry.db() as db:
        if not db.execute('SELECT 1 FROM vtls WHERE id=?', (vtl_id,)).fetchone():
            raise ValueError('Unknown VTL.')
    saved = settings(registry.root, vtl_id)
    try:
        bound = client_binding.effective(registry, vtl_id)
    except (ValueError, OSError, KeyError):
        bound = None
    draft_client = 'veeam' if isinstance(bound, dict) and bound.get('client') == 'veeam' else 'pbs'
    result = dict(saved, client=draft_client, phase='missing_prerequisite', provisioning_revision=None,
                  pbs=None, iqn=None, changer=None, drive=None, steps=[],
                  message='Associate the backup client and prepare this library before generating commands.')
    try:
        manifest = registry.current(vtl_id)
    except (ValueError, OSError):
        return result
    if len(manifest['drives']) != 1:
        result['message'] = 'This procedure supports one drive per VTL.'
        return result
    veeam = manifest.get('client') == 'veeam'
    result['client'] = 'veeam' if veeam else 'pbs'
    if not veeam:
        binding = manifest['pbs']
        for name in ('changer', 'drive'):
            if not isinstance(binding.get(name), str) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{0,63}', binding[name]):
                raise ValueError('Invalid PBS name in the manifest.')
    for item in [manifest['library'], manifest['drives'][0]]:
        if not re.fullmatch(f"(?:{'|'.join(SERIAL_PREFIXES)})(?:[A-F0-9]{{8}}|[A-F0-9]{{12}})", item['serial']):
            raise ValueError('Invalid device identity.')
    if not valid_iqn(manifest['target']['iqn'], vtl_id):
        raise ValueError('Unexpected iSCSI identity.')
    changer_serial, drive_serial = manifest['library']['serial'], manifest['drives'][0]['serial']
    if veeam:
        result.update(provisioning_revision=manifest['revision'], pbs=None, iqn=manifest['target']['iqn'],
                      changer=dict(name=None, serial=changer_serial, path=None),
                      drive=dict(name=None, serial=drive_serial, path=None, ordinal=0))
        if saved['host'] is None:
            result['message'] = 'Enter the VaultCenter iSCSI address reachable from the Veeam server.'
            return result
        result.update(phase='ready', message='Activate the library in VaultCenter before connecting. Then, in Veeam: Tape Infrastructure, library, Rescan, then Inventory.')
        result['steps'] = render_windows_steps(result, saved['host'], saved['port'])
        return result
    result.update(provisioning_revision=manifest['revision'], pbs=binding['pbs'], iqn=manifest['target']['iqn'],
                  changer=dict(name=binding['changer'], serial=changer_serial, path='/dev/tape/by-id/scsi-'+changer_serial),
                  drive=dict(name=binding['drive'], serial=drive_serial, path='/dev/tape/by-id/scsi-'+drive_serial+'-sg', ordinal=0))
    if saved['host'] is None:
        result['message'] = 'Enter the VaultCenter iSCSI address reachable from this PBS.'
        return result
    result.update(phase='ready', message='Activate the library in VaultCenter before connecting. After saving, resume activation if it is pending.')
    result['steps'] = render_steps(result, saved['host'], saved['port'])
    return result


def save_portal(registry, vtl_id, revision, host, port):
    host, _ = portal(host, port)
    with registry.db() as db:
        if not db.execute('SELECT 1 FROM vtls WHERE id=?', (identifier(vtl_id),)).fetchone():
            raise ValueError('Unknown VTL.')
    root = directory(registry.root, vtl_id)
    with locked(root/'pbs-connection.lock'):
        old = settings(registry.root, vtl_id)
        if type(revision) is not int or old['revision'] != revision or revision >= 2**63-1:
            raise ValueError('Connection revision changed; reload before saving.')
        atomic(root/'pbs-connection.json', dict(schema=1, vtl_id=vtl_id, revision=revision+1, host=host, port=port))
    return connection_info(registry, vtl_id)


PRELUDE = '''import json, subprocess, socket, ipaddress, re, time
def run(args, allowed=(0,)):
    result = subprocess.run(args, text=True, capture_output=True, timeout=30)
    if result.returncode not in allowed:
        raise SystemExit("Command refused: " + " ".join(args) + "\\n" + result.stderr.strip())
    return result
'''

CONNECT = '''
node = ["iscsiadm", "-m", "node", "-T", identity["iqn"], "-p", identity["portal"]]
existing = run(node + ["-o", "show"], allowed=(0, 21))
if existing.returncode == 21:
    run(node + ["-o", "new"])
profile = {"node.startup":"automatic", "node.session.iscsi.InitialR2T":"No",
    "node.session.iscsi.ImmediateData":"Yes", "node.session.iscsi.FirstBurstLength":"262144",
    "node.session.iscsi.MaxBurstLength":"262144", "node.conn[0].iscsi.MaxRecvDataSegmentLength":"262144"}
for key, value in profile.items():
    run(node + ["-o", "update", "-n", key, "-v", value])
sessions = run(["iscsiadm", "-m", "session"], allowed=(0, 21)).stdout
addresses = {ipaddress.ip_address(row[4][0]).compressed for row in socket.getaddrinfo(identity["host"], identity["port"], type=socket.SOCK_STREAM)}
connected = False
for line in sessions.splitlines():
    parts = line.split()
    if len(parts) < 4 or parts[3] != identity["iqn"]:
        continue
    endpoint = parts[2].split(",",1)[0]
    host, port = endpoint.rsplit(":",1)
    if ipaddress.ip_address(host.strip("[]")).compressed not in addresses or int(port) != identity["port"]:
        raise SystemExit("This VTL is already connected through another portal; check before continuing.")
    connected = True
if connected:
    print("Existing session kept. The saved profile will be renegotiated at the next connection, between two jobs.")
else:
    run(node + ["--login"])
    print("iSCSI session connected.")
'''

REGISTER = '''
# Verify both device identities and both PBS names before creating either entry.
expected = [("changer", identity["changer"]), ("drive", identity["drive"])]
create = []
for kind, device in expected:
    deadline = time.monotonic() + 15
    while True:
        inquiry = run(["sg_inq", "--page=0x80", "--export", device["path"]], allowed=tuple(range(256)))
        if inquiry.returncode == 0:
            break
        if time.monotonic() >= deadline:
            raise SystemExit("Expected device missing: " + device["path"])
        time.sleep(.5)
    serial = re.search(r"^SCSI_IDENT_SERIAL=(\\S+)$", inquiry.stdout, re.MULTILINE)
    if not serial or serial.group(1) != device["serial"]:
        raise SystemExit("SCSI identity differs; registration refused.")
    entries = json.loads(run(["proxmox-tape", kind, "list", "--output-format", "json"]).stdout)
    if not isinstance(entries, list):
        raise SystemExit("Invalid PBS inventory.")
    found = False
    for entry in entries:
        same = entry.get("name") == device["name"]
        overlap = entry.get("path") == device["path"] or entry.get("serial") == device["serial"]
        if same:
            if entry.get("path") != device["path"] or entry.get("serial") not in (None, device["serial"]):
                raise SystemExit("PBS name already associated with another device: " + device["name"])
            if kind == "drive" and (entry.get("changer") != identity["changer"]["name"] or entry.get("changer-drivenum",0) != 0):
                raise SystemExit("PBS drive already associated with another changer or slot.")
            found = True
        elif overlap:
            raise SystemExit("This device is already registered under another PBS name.")
    if not found:
        create.append((kind, device))
for kind, device in create:
    command = ["proxmox-tape", kind, "create", device["name"], "--path", device["path"]]
    if kind == "drive":
        command += ["--changer", identity["changer"]["name"], "--changer-drivenum", "0"]
    run(command)
print("Changer and drive verified and registered in PBS.")
'''


def render_steps(identity, host, port):
    host, address = portal(host, port)
    # Only the small allowlisted identity projection is embedded, never the binding.
    values = {key: identity[key] for key in ('vtl_id','iqn','changer','drive')}
    values.update(host=host, port=port, portal=address)
    def script(body):
        return "python3 - <<'VAULTCENTER_PBS'\n" + PRELUDE + 'identity = json.loads(' + repr(json.dumps(values)) + ')\n' + body + '\nVAULTCENTER_PBS'
    return [
        dict(id='iscsi', title='Connect the iSCSI target', explanation='Run in a root shell on the associated PBS with open-iscsi installed. An existing session stays connected.', command=script(CONNECT)),
        dict(id='register', title='Register the changer and drive', explanation='Requires sg_inq (sg3-utils package). Checks identities before creating anything; an identical configuration is retained.', command=script(REGISTER)),
        dict(id='verify', title='Check inventory', explanation='Then resume activation in VaultCenter if it is waiting for the PBS connection.', command='proxmox-tape changer status '+shlex.quote(identity['changer']['name'])+' --output-format json-pretty'),
    ]


def render_windows_steps(identity, host, port):
    host, _ = portal(host, port)
    # Whitelist: only these characters can reach the PowerShell text, so no quote, $ or backtick.
    iqn = identity['iqn']
    if (not re.fullmatch(r'[A-Za-z0-9.:-]+', host)
            or not valid_iqn(iqn)):
        raise ValueError('Invalid connection value.')
    connect = (
        f"$portal = '{host}'; $port = {int(port)}; $iqn = '{iqn}'\n"
        "if (-not (Get-IscsiTargetPortal | Where-Object { $_.TargetPortalAddress -eq $portal -and $_.TargetPortalPortNumber -eq $port })) {\n"
        "  New-IscsiTargetPortal -TargetPortalAddress $portal -TargetPortalPortNumber $port | Out-Null\n"
        "}\n"
        "Update-IscsiTargetPortal -TargetPortalAddress $portal -TargetPortalPortNumber $port | Out-Null\n"
        "if (-not (Get-IscsiTarget -NodeAddress $iqn).IsConnected) {\n"
        "  Connect-IscsiTarget -NodeAddress $iqn -TargetPortalAddress $portal -TargetPortalPortNumber $port -IsPersistent $true | Out-Null\n"
        "}\n"
        "Get-IscsiSession | Where-Object { $_.TargetNodeAddress -eq $iqn } | Select-Object TargetNodeAddress, IsConnected, IsPersistent"
    )
    devices = ("Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -match 'MSL_G3|Ultrium_8' } | "
               "Select-Object FriendlyName, Status, @{n='HardwareID';e={$_.HardwareID[0]}}")
    return [
        dict(id='iscsi', title='Connect the iSCSI target', explanation='Run in an administrator PowerShell on the Veeam server (tape server). An existing session stays connected.', command=connect),
        dict(id='devices', title='Check the changer and drive', explanation='The HP MSL G3 changer and the HPE Ultrium 8 drive must appear. The drive requires the HPE StoreEver driver.', command=devices),
    ]
