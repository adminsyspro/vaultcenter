"""Per-VTL backup client binding. PBS bindings stay root-managed in settings."""
import ipaddress
import json
from vtl_runtime import atomic, identifier, locked
from bandwidth import directory

CLIENTS = ('veeam',)


def _initiator(value):
    if not isinstance(value, str) or value != value.strip():
        raise ValueError('Invalid initiator IP address.')
    try:
        return str(ipaddress.ip_address(value))
    except ValueError:
        raise ValueError('Invalid initiator IP address.') from None


def _stored(registry, vtl_id):
    path = directory(registry.root, vtl_id) / 'client.json'
    if not path.exists() and not path.is_symlink():
        return dict(schema=1, vtl_id=vtl_id, revision=0, client=None, initiator=None)
    if path.is_symlink() or path.stat().st_size > 4096:
        raise ValueError('Invalid client binding.')
    value = json.loads(path.read_text())
    if (set(value) != {'schema','vtl_id','revision','client','initiator'} or value['schema'] != 1
            or value['vtl_id'] != vtl_id or type(value['revision']) is not int or value['revision'] < 1
            or value['client'] not in CLIENTS):
        raise ValueError('Client binding is invalid or belongs to another VTL.')
    _initiator(value['initiator'])
    return value


def _editable(registry, vtl_id):
    with registry.db() as db:
        vtl = db.execute('SELECT lifecycle FROM vtls WHERE id=?', (vtl_id,)).fetchone()
        if not vtl:
            raise ValueError('Unknown VTL.')
        row = db.execute('SELECT phase FROM vtl_provisioning WHERE vtl_id=?', (vtl_id,)).fetchone()
    return vtl['lifecycle'] == 'draft' and (row is None or row['phase'] == 'failed')


def effective(registry, vtl_id):
    identifier(vtl_id)
    pbs = registry.settings.get('bindings', {}).get(vtl_id)
    if pbs:
        return pbs
    stored = _stored(registry, vtl_id)
    if stored['client'] is None:
        return None
    return {'client': stored['client'], 'initiator': stored['initiator']}


def status(registry, vtl_id):
    identifier(vtl_id)
    editable = _editable(registry, vtl_id)
    pbs = registry.settings.get('bindings', {}).get(vtl_id)
    if pbs:
        return dict(schema=1, vtl_id=vtl_id, revision=0, client='pbs', initiator=pbs['initiator'], editable=False,
                    message='PBS binding managed by the appliance configuration.')
    stored = _stored(registry, vtl_id)
    message = None if editable else 'Binding locked: the library is already prepared.'
    return dict(schema=1, vtl_id=vtl_id, revision=stored['revision'], client=stored['client'],
                initiator=stored['initiator'], editable=editable, message=message)


def save(registry, vtl_id, revision, client, initiator):
    identifier(vtl_id)
    if client not in CLIENTS:
        raise ValueError('Unsupported client type.')
    initiator = _initiator(initiator)
    if registry.settings.get('bindings', {}).get(vtl_id):
        raise ValueError('PBS binding managed by the appliance configuration.')
    root = directory(registry.root, vtl_id)
    with locked(root/'client.lock'):
        if not _editable(registry, vtl_id):
            raise ValueError('Binding locked: the library is already prepared.')
        old = _stored(registry, vtl_id)
        if type(revision) is not int or old['revision'] != revision or revision >= 2**63-1:
            raise ValueError('Binding revision changed; reload before saving.')
        atomic(root/'client.json', dict(schema=1, vtl_id=vtl_id, revision=revision+1, client=client, initiator=initiator))
    return status(registry, vtl_id)
