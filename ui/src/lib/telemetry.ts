import type { Cartridge } from './api';
export type DriveState = 'empty'|'loading'|'loaded'|'reading'|'writing'|'unloading'|'idle'|'maintenance'|'error'|'unknown';
export type DriveObservation = {id:string;label:string;barcode:string|null;origin_slot:number|null;state:DriveState;observed_at:number|null;bytes_written:number|null;write_rate_bps:number|null};
export type SlotObservation = {number:number;kind:'storage'|'import_export';barcode:string|null;position_known:boolean;observed_at:number|null};
export type MediaObservation = {barcode:string;observed_at:number|null;local_size_bytes:number|null;local_growth_bps:number|null;logical_used_bytes:number|null;logical_capacity_bytes:number|null};
export type DemoTransfer = {id:string;barcode:string;bucket:string;status:'running'|'completed';transferred_bytes:number;total_bytes:number;rate_bps:number|null;observed_at:number;stale?:boolean};
export type Telemetry = {demo_scenario?:string;demo_drive?:string;demo_fail_next?:boolean;demo_transfer?:DemoTransfer;version:1;vtl_id:string;generated_at:number|null;observed_at:number|null;source:'mhvtl'|'demo';status:'available'|'stale'|'unavailable';reason:'not_configured'|'source_missing'|'stale'|'invalid_snapshot'|null;drives:DriveObservation[];slots:SlotObservation[];media:MediaObservation[]};
export const driveLabels:Record<DriveState,string>={empty:'Lecteur vide',loading:'Chargement',loaded:'Cartouche chargée',reading:'Lecture en cours',writing:'Écriture en cours',unloading:'Déchargement',idle:'Au repos',maintenance:'Maintenance',error:'Erreur de chargement',unknown:'Activité inconnue'};
const recent=(time:number|null,now:number,age:number)=>time!==null&&time<=now+5000&&now-time<=age;
export function ageTelemetry(value:Telemetry,now:number):Telemetry {
 const stale=value.status==='stale'||!recent(value.generated_at,now,10000);
 const available=value.status!=='unavailable'&&!stale;
 return {...value,demo_transfer:value.source==='demo'&&value.demo_transfer?{...value.demo_transfer,stale:!available||!recent(value.demo_transfer.observed_at,now,10000),rate_bps:available&&recent(value.demo_transfer.observed_at,now,10000)?value.demo_transfer.rate_bps:null}:undefined,status:value.status==='unavailable'?'unavailable':stale?'stale':'available',
  drives:value.drives.map(d=>available&&recent(d.observed_at,now,15000)?d:{...d,state:'unknown',barcode:null,origin_slot:null,bytes_written:null,write_rate_bps:null}),
  slots:value.slots.map(s=>available&&recent(s.observed_at,now,15000)?s:{...s,position_known:false,barcode:null}),
  media:value.media.map(m=>available&&recent(m.observed_at,now,10000)?m:{...m,local_size_bytes:null,local_growth_bps:null,logical_used_bytes:null,logical_capacity_bytes:null})};
}
export function occupancy(media:Cartridge,observation?:MediaObservation) {
 const matches=observation?.barcode===media.barcode;
 const live=matches&&observation?.logical_used_bytes!=null&&observation.logical_capacity_bytes!=null&&observation.logical_capacity_bytes>0;
 // The spool includes metadata and compressed data: this is an estimate of
 // cartridge occupancy, never a PBS job's completion percentage. Once sealed,
 // keep the catalogue so eviction or partial restores cannot reduce occupancy.
 const local=!live&&matches&&media.state==='writable'&&observation?.local_size_bytes!=null&&Number.isFinite(observation.local_size_bytes)&&observation.local_size_bytes>=0;
 const used=live?observation!.logical_used_bytes!:local?observation!.local_size_bytes!:media.bytes_written;
 const capacity=live?observation!.logical_capacity_bytes!:media.capacity_bytes;
 const valid=Number.isFinite(used)&&used>=0&&Number.isFinite(capacity)&&capacity>0;
 return {used,capacity,percent:valid?Math.min(100,used/capacity*100):null,label:live?'Occupation mesurée':local?'Occupation locale estimée':'Dernier relevé du catalogue'};
}

export type RatePoint = { at:number; writeRate:number|null; localGrowth:number|null };
export type RateHistory = Record<string,{barcode:string;points:RatePoint[]}>;
/** One independent, bounded series per observed drive/cartridge association. */
export function appendRateHistory(previous:RateHistory,value:Telemetry):RateHistory {
 const next:RateHistory={};
 for(const drive of value.drives) {
  if(!drive.barcode)continue;
  const media=value.media.find(m=>m.barcode===drive.barcode);
  const at=value.generated_at;
  if(at===null)continue;
  const prior=previous[drive.id];
  const points=prior?.barcode===drive.barcode?prior.points.filter(p=>at-p.at<=120000):[];
  const valid=(rate:number|null|undefined)=>value.status==='available'&&rate!=null&&Number.isFinite(rate)&&rate>=0?rate:null;
  const point={at,writeRate:valid(drive.write_rate_bps),localGrowth:valid(media?.local_growth_bps)};
  next[drive.id]={barcode:drive.barcode,points:[...points.filter(p=>p.at!==at),point].slice(-60)};
 }
 return next;
}

/** Demo S3 measurements stay independent from physical drive counters. */
export function appendTransferHistory(previous:{id:string;points:RatePoint[]}|null,value:Telemetry) {
 const transfer=value.source==='demo'?value.demo_transfer:undefined;
 if(!transfer)return null;
 const at=transfer.observed_at;
 const prior=previous?.id===transfer.id?previous.points:[];
 const rate=transfer.rate_bps;
 const point={at,writeRate:!transfer.stale&&rate!==null&&Number.isFinite(rate)&&rate>=0?rate:null,localGrowth:null};
 return {id:transfer.id,points:[...prior.filter(p=>p.at!==at&&at-p.at<=120000),point].slice(-60)};
}

/** Live runs replace sparse buckets; server samples fill pauses in browser polling. */
export function mergeRateHistory(historic:RatePoint[],live:RatePoint[]):RatePoint[] {
 if(!live.length)return historic;
 const ordered=[...live].sort((a,b)=>a.at-b.at);
 const runs:{start:number;end:number}[]=[];
 for(const point of ordered){
  const run=runs.at(-1);
  if(run&&point.at-run.end<=6000)run.end=point.at;
  else runs.push({start:point.at,end:point.at});
 }
 // Only discard server buckets where the browser actually sampled continuously.
 const combined=new Map(historic
  .filter(point=>!runs.some(run=>point.at>=run.start&&point.at<=run.end))
  .map(point=>[point.at,point]));
 for(const point of ordered)combined.set(point.at,point);
 const result:RatePoint[]=[];
 for(const point of [...combined.values()].sort((a,b)=>a.at-b.at)){
  const previous=result.at(-1);
  if(previous&&point.at-previous.at>6000)result.push({at:previous.at+1,writeRate:null,localGrowth:null});
  result.push(point);
 }
 return result;
}
