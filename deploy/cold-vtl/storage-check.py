#!/usr/bin/env python3
"""Managed storage gate. Missing host bridge is a failure, never legacy mode."""
import json
import os
from pathlib import Path
import socket
import sys
import uuid

SOCKET='/run/vaultcenter-host/storage.sock'

def request(path, vtl_id=None):
    try:
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as connection:
            connection.settimeout(8); connection.connect(path)
            payload={'action':'status'}
            if vtl_id is not None: payload['vtl_id']=vtl_id
            connection.sendall((json.dumps(payload)+'\n').encode())
            with connection.makefile('rb') as stream: data=stream.readline(65537)
        if len(data)>65536 or not data.endswith(b'\n'): raise ValueError('Invalid storage response.')
        value=json.loads(data)
        if value.get('ok') is not True or not isinstance(value.get('result'),dict): raise ValueError('Invalid storage response.')
        return value['result']
    except (OSError,KeyError,TypeError,json.JSONDecodeError) as error:
        raise ValueError('The host storage service is unavailable.') from error

def check(root=Path('/var/lib/cold-vtl'),socket_path=SOCKET,service=False,vtl_id=None):
    if not (root/'storage-managed').exists(): return
    isolated=(root/'storage-isolated').exists()
    if isolated:
        try:
            if str(uuid.UUID(vtl_id))!=vtl_id: raise ValueError()
        except (ValueError,TypeError,AttributeError): raise ValueError('An explicit VTL is required for the storage check.') from None
        root=root/'vtls'/vtl_id
    if (root/'storage-block').exists(): raise ValueError('Storage blocked: finish or recover its configuration.')
    if service and (root/'storage-services-held').exists(): raise ValueError('Explicit service resume required.')
    result=request(socket_path,vtl_id) if isolated else request(socket_path)
    if isolated and result.get('vtl_id')!=vtl_id: raise ValueError('Storage response belongs to another VTL.')
    if result.get('phase')!='ready': raise ValueError(result.get('message') or 'Configure available storage before using tapes.')

if __name__=='__main__':
    try: check(socket_path=os.environ.get('COLD_VTL_STORAGE_SOCKET',SOCKET),service='--service' in sys.argv,vtl_id=os.environ.get('COLD_VTL_VTL_ID'))
    except ValueError as error:
        print(str(error),file=sys.stderr); sys.exit(1)
