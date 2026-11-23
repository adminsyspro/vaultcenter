"use client";
import {useCallback,useContext,useEffect,useRef,useState} from 'react';
import {type Job} from '@/lib/api';
import type {Telemetry} from '@/lib/telemetry';
import {Maintenance,type MaintenanceStatus} from './maintenance';
import {MediaPower} from './media-power';
import {VtlScope} from './vtl-scope';
import {vtlHash} from './use-vtl-selection';
import {publishMaintenance} from '@/lib/maintenance-events';

/** Persistent controls: changing the central screen never remounts the power state.
 * `routed` ties the maintenance dialog to #media/maintenance; the fleet view keeps it local. */
export function LibraryControls({canManage,connected,jobs,onRefresh,data,routed=true}:{canManage:boolean;connected:boolean;jobs:Job[];onRefresh:()=>Promise<void>;data:Telemetry|null;routed?:boolean}) {
 const id=useContext(VtlScope);
 const [open,setOpen]=useState(false);
 const [confirmed,setStatus]=useState<MaintenanceStatus|null>(null);
 const setConfirmed=useCallback((status:MaintenanceStatus)=>{setStatus(status);publishMaintenance(id,status);},[id]);
 const [pendingVtl,setPendingVtl]=useState<string|null>(null);
 const onTransitionPending=useCallback((pending:boolean)=>setPendingVtl(previous=>pending?id:previous===id?null:previous),[id]);
 const returnHash=useRef<string|null>(null);
 useEffect(()=>{
  if(!routed)return;
  const read=()=>setOpen(location.hash.split('?')[0]==='#media/maintenance');
  read();window.addEventListener('hashchange',read);
  return()=>window.removeEventListener('hashchange',read);
 },[routed]);
 function show(){if(routed){returnHash.current=location.hash;location.hash=vtlHash('media/maintenance',id);}setOpen(true);}
 function close(){setOpen(false);if(routed&&location.hash.split('?')[0]==='#media/maintenance')location.hash=returnHash.current??vtlHash('media',id);returnHash.current=null;}
 return <>
  <MediaPower canManage={canManage} connected={connected} jobs={jobs} onRefresh={onRefresh} data={data} onMaintenance={show} maintenancePending={pendingVtl===id} maintenanceUpdate={confirmed} onMaintenanceStatus={setConfirmed}/>
  {(open||pendingVtl===id)&&<Maintenance key={id} open={open} canManage={canManage} connected={connected} jobs={jobs} onRefresh={onRefresh} onStatusConfirmed={setConfirmed} onTransitionPending={onTransitionPending} onClose={close}/>}
 </>;
}
