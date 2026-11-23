"use client";
import {useContext,useEffect,useState} from 'react';
import {VtlScope,useScopedRequest} from './vtl-scope';
import type {RatePoint} from '@/lib/telemetry';

export type ThroughputHistory = {
 vtl_id:string;source:string;status:'available'|'unavailable';retention_days:number;
 start_ms:number;end_ms:number;resolution_ms:number;
 series:{drive_id:string;barcode:string;last_at:number;transfer_id?:string}[];
 points:RatePoint[];
};
/** `aggregate` asks for one curve summing every series of the scope; it never goes with a barcode or a transfer. */
export function useThroughputHistory({barcode,driveId,kind,transferId,seconds,endMs,enabled=true,aggregate=false}:{barcode:string;driveId?:string;kind:'pbs'|'s3'|'s3_restore';transferId?:string;seconds:number;endMs?:number;enabled?:boolean;aggregate?:boolean}) {
 const vtlId=useContext(VtlScope),request=useScopedRequest();
 const query=new URLSearchParams({kind,window_seconds:String(seconds)});
 if(aggregate)query.set('aggregate','true');
 else{if(barcode)query.set('barcode',barcode);if(transferId)query.set('transfer_id',transferId);}
 if(driveId)query.set('drive_id',driveId);
 if(endMs!==undefined)query.set('end_ms',String(endMs));
 const path=`/v1/telemetry/history?${query}`;
 const key=`${vtlId}:${path}`;
 const [result,setResult]=useState<{key:string;data:ThroughputHistory}|null>(null);
 const [failure,setFailure]=useState<string|null>(null);
 useEffect(()=>{
  if(!vtlId||!enabled)return;
  let disposed=false,pending:AbortController|null=null,timer:ReturnType<typeof setTimeout>|undefined;
  setFailure(null);
  async function load(){
   if(disposed||pending||document.hidden)return;
   pending=new AbortController();
   try{
    const data=await request<ThroughputHistory>(path,{signal:AbortSignal.any([pending.signal,AbortSignal.timeout(8000)])});
    const validRate=(rate:unknown)=>rate===null||(typeof rate==='number'&&Number.isFinite(rate)&&rate>=0);
    if(data.vtl_id!==vtlId||!Array.isArray(data.series)||!Array.isArray(data.points)||data.points.length>600||
      !Number.isFinite(data.start_ms)||!Number.isFinite(data.end_ms)||data.end_ms<data.start_ms||
      !data.points.every(p=>Number.isFinite(p.at)&&validRate(p.writeRate)&&validRate(p.localGrowth)))throw Error('Historique invalide');
    if(!disposed&&!pending.signal.aborted){setResult({key,data});setFailure(null);}
   }catch{if(!disposed&&!pending.signal.aborted)setFailure(key);}
   finally{pending=null;if(!disposed)timer=setTimeout(load,seconds===120?5000:seconds<=3600?15000:60000);}
  }
  function visibility(){clearTimeout(timer);if(document.hidden)pending?.abort();else void load();}
  document.addEventListener('visibilitychange',visibility);void load();
  return()=>{disposed=true;clearTimeout(timer);pending?.abort();document.removeEventListener('visibilitychange',visibility);};
 },[vtlId,request,path,key,seconds,enabled]);
 return {data:result?.key===key?result.data:null,error:failure===key,loading:result?.key!==key&&failure!==key};
}
