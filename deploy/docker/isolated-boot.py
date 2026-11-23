#!/usr/bin/env python3
"""Prepare isolated systemd units before PID 1 starts; never starts hardware here."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0,'/usr/local/libexec')
from vtl_runtime import Registry, atomic_text, atomic, resolve_runtime


def configure(root=Path('/')):
    state=root/'var/lib/cold-vtl'
    if not (state/'storage-isolated').exists():return
    system=root/'etc/systemd/system'
    # Disable only the former singleton units, before the initial mode-switch boot.
    for unit in ('mhvtl.target','vtllibrary@10.service','vtltape@11.service','vaultcenter-telemetry.service'):
        atomic_text(system/(unit+'.d')/'zz-isolated.conf','[Unit]\nConditionPathExists=!/var/lib/cold-vtl/storage-isolated\n')
    # systemd dependency lists cannot be cleared with an empty drop-in setting.
    legacy=system/'tgt.service.d/cold-vtl.conf'
    if legacy.exists() and not legacy.is_symlink():
        atomic_text(state/'legacy-systemd/tgt-cold-vtl.conf',legacy.read_text())
        legacy.unlink();legacy.symlink_to('/dev/null')
    atomic_text(system/'tgt.service.d/zz-isolated.conf','''[Unit]
Requires=
After=
After=network.target
ConditionPathExists=
[Service]
ExecCondition=
ExecStartPre=
ExecStartPost=
ExecStartPost=/usr/sbin/tgtadm --op update --mode sys --name State -v ready
ExecReload=
''')
    atomic_text(system/'cold-vtld.service.d/zz-isolated.conf','''[Unit]
After=
After=network-online.target tgt.service
[Service]
ReadWritePaths=/var/lib/vaultcenter-media /var/lib/cold-vtl
''')
    settings=json.loads((root/'etc/cold-vtl/vtl-runtime.json').read_text())
    if not (state/'catalog.db').exists():return
    registry=Registry(state,settings,runner=lambda args:'')
    from bandwidth import initialize_bandwidth
    with registry.db() as db:
        ids=db.execute('SELECT id FROM vtls').fetchall() if db.execute("SELECT 1 FROM sqlite_master WHERE name='vtls' AND type='table'").fetchone() else []
    for item in ids:
        try:initialize_bandwidth(state,item['id'])
        except (ValueError,OSError) as error:
            atomic(state/'vtls'/item['id']/'storage-services-held',{'reason':'bandwidth_unavailable'})
            print('VTL '+item['id']+' : '+str(error),file=sys.stderr)
    with registry.db() as db:rows=db.execute("SELECT vtl_id,phase FROM vtl_provisioning WHERE phase IN ('configured','active')").fetchall()
    for row in rows:
        # A corrupt A must not prevent B or the control plane from booting.
        try:
            m=resolve_runtime(row['vtl_id'],root=state)
            if root!=Path('/'):continue  # Offline validation never writes manifest absolute paths.
            registry.write_configuration(m)
            for unit in [*registry.units(m),registry.telemetry_unit(m)]:
                path=system/unit;path.unlink(missing_ok=True);path.symlink_to(state/'units'/unit)
                if row['phase']=='active':
                    wants=system/'multi-user.target.wants'/unit
                    wants.parent.mkdir(parents=True,exist_ok=True)
                    wants.unlink(missing_ok=True);wants.symlink_to(path)
        except (ValueError,OSError) as error:
            atomic(state/'vtls'/row['vtl_id']/'storage-services-held',{'reason':'runtime_unavailable'})
            print('VTL '+row['vtl_id']+' kept stopped: '+str(error),file=sys.stderr)

if __name__=='__main__':configure()
