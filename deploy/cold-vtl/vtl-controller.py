#!/usr/bin/env python3
"""Fixed privileged runtime commands; caller cannot provide device/unit paths."""
import argparse
import json
import os
from pathlib import Path
import sys
from vtl_runtime import Registry, identifier

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('action',choices=['prepare','apply','status','storage-idle','publish','withdraw','activate',
                                         'bandwidth-status','bandwidth-update','bandwidth-initialize','pbs-connection-status','pbs-connection-update','client-status','client-update'])
    parser.add_argument('--vtl',required=True)
    args=parser.parse_args();identifier(args.vtl);os.umask(0o077)
    registry=Registry()
    if args.action.startswith('bandwidth-'):
        from bandwidth import bandwidth_status, initialize_bandwidth, update_bandwidth, require_parallel_host
        with registry.db() as db:
            if not db.execute('SELECT 1 FROM vtls WHERE id=?',(args.vtl,)).fetchone():
                raise ValueError('Unknown VTL.')
        if args.action=='bandwidth-status':return bandwidth_status(registry,args.vtl)
        if args.action=='bandwidth-initialize':return initialize_bandwidth(registry.root,args.vtl)
        raw=sys.stdin.buffer.read(8193)
        if len(raw)>8192:raise ValueError('Request too large.')
        request=json.loads(raw)
        if set(request)!={'revision','tape_write_bps','s3_bps'}:raise ValueError('Invalid request.')
        if request['tape_write_bps'] is not None:
            require_parallel_host()
            manifest=registry.current(args.vtl)
            if len(manifest['drives'])!=1:raise ValueError('The PBS limit requires a single drive per VTL.')
        saved=update_bandwidth(registry.root,args.vtl,request['revision'],request['tape_write_bps'],request['s3_bps'])
        result=bandwidth_status(registry,args.vtl)
        result['message']=saved.get('message')
        return result
    if args.action.startswith('pbs-connection-'):
        from pbs_connection import connection_info, save_portal
        if args.action=='pbs-connection-status':return connection_info(registry,args.vtl)
        raw=sys.stdin.buffer.read(8193)
        if len(raw)>8192:raise ValueError('Request too large.')
        request=json.loads(raw)
        if set(request)!={'revision','host','port'}:raise ValueError('Invalid request.')
        return save_portal(registry,args.vtl,request['revision'],request['host'],request['port'])
    if args.action.startswith('client-'):
        from client_binding import status, save
        if args.action=='client-status':return status(registry,args.vtl)
        raw=sys.stdin.buffer.read(8193)
        if len(raw)>8192:raise ValueError('Request too large.')
        request=json.loads(raw)
        if set(request)!={'revision','client','initiator'}:raise ValueError('Invalid request.')
        return save(registry,args.vtl,request['revision'],request['client'],request['initiator'])
    if args.action in ('prepare','apply','activate'):
        raw=sys.stdin.buffer.read(8193)
        if len(raw)>8192:raise ValueError('Request too large.')
        request=json.loads(raw)
        if args.action=='activate':
            if set(request)!={'actor'}:raise ValueError('Invalid request.')
            return registry.activate(args.vtl,request['actor'])
        if args.action=='prepare':
            if set(request)!={'revision'}:raise ValueError('Invalid request.')
            return registry.prepare(args.vtl,request['revision'])
        if set(request)!={'token','actor'}:raise ValueError('Invalid request.')
        return registry.apply(args.vtl,request['token'],request['actor'])
    if args.action=='status':return registry.status(args.vtl)
    return getattr(registry,args.action.replace('-','_'))(args.vtl)

if __name__=='__main__':
    try:print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'error':str(error)}));sys.exit(1)
