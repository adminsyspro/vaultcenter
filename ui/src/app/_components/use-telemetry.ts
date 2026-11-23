"use client";
import {useI18n} from '@/lib/i18n';
import {useCallback,useEffect,useRef,useState} from 'react';
import {request} from '@/lib/api';
import {ageTelemetry,appendRateHistory,appendTransferHistory,type RateHistory,type Telemetry} from '@/lib/telemetry';
export function useTelemetry(vtlId:string|null,enabled:boolean) {
 const {t}=useI18n();
 const reload=useRef<()=>Promise<void>>(async()=>{});
 const refresh=useCallback(()=>reload.current(),[]);
 const [result,setResult]=useState<{id:string;value:Telemetry}|null>(null);
 const [history,setHistory]=useState<RateHistory>({});
 const [transferHistory,setTransferHistory]=useState<ReturnType<typeof appendTransferHistory>>(null);
 const [error,setError]=useState<string|null>(null);
 const [refreshing,setRefreshing]=useState(false);
 const [now,setNow]=useState(0);
 useEffect(()=>{
  setResult(null);setHistory({});setTransferHistory(null);setError(null);setRefreshing(false);setNow(Date.now());
  if(!vtlId||!enabled)return;
  let disposed=false;let pending:AbortController|null=null;let timer:ReturnType<typeof setTimeout>|undefined;
  const clock=setInterval(()=>setNow(Date.now()),1000);
  async function poll() {
   if(disposed||document.hidden||pending)return;
   const controller=new AbortController();pending=controller;setRefreshing(true);
   try {
    const value=await request<Telemetry>(`/v1/vtls/${encodeURIComponent(vtlId!)}/telemetry`,{signal:AbortSignal.any([controller.signal,AbortSignal.timeout(8000)])});
    if(!disposed&&pending===controller&&!controller.signal.aborted) {
     if(value.version!==1||value.vtl_id!==vtlId||!Array.isArray(value.drives)||!Array.isArray(value.slots)||!Array.isArray(value.media))throw Error('Réponse de télémétrie invalide.');
     setHistory(previous=>appendRateHistory(previous,ageTelemetry(value,Date.now())));
     setTransferHistory(previous=>appendTransferHistory(previous,ageTelemetry(value,Date.now())));
     setResult({id:vtlId!,value});setError(null);setNow(Date.now());
    }
   } catch(cause) {if(!disposed&&pending===controller&&!controller.signal.aborted)setError(cause instanceof Error?cause.message:'Source indisponible');}
   finally {if(pending===controller){pending=null;if(!disposed){setRefreshing(false);timer=setTimeout(poll,2000);}}}
  }
  reload.current=async()=>{clearTimeout(timer);pending?.abort();pending=null;await poll();};
  function visibility(){clearTimeout(timer);if(document.hidden)pending?.abort();else void poll();}
  document.addEventListener('visibilitychange',visibility);void poll();
  return ()=>{disposed=true;reload.current=async()=>{};clearTimeout(timer);clearInterval(clock);pending?.abort();document.removeEventListener('visibilitychange',visibility);};
 },[vtlId,enabled]);
 const raw=result?.id===vtlId?result.value:null;
 return {refresh,transferHistory:result?.id===vtlId?transferHistory:null,history:result?.id===vtlId?history:{},data:raw?ageTelemetry(error?{...raw,status:'stale'}:raw,now):null,error:error?t(error):null,refreshing};
}
