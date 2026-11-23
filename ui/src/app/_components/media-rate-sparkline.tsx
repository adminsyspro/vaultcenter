"use client";
import {useI18n} from '@/lib/i18n';

import {formatBytes} from '@/lib/api';
import {mergeRateHistory, type RatePoint} from '@/lib/telemetry';
import {useThroughputHistory} from './use-throughput-history';

/** A compact view of measured PBS writes, preserving missing samples as gaps. */
export function MediaRateSparkline({barcode,points,rate}:{barcode:string;points:RatePoint[];rate:number|null}) {
 const {t}=useI18n();
 const {data,error,loading}=useThroughputHistory({barcode,kind:'pbs',seconds:120});
 const end=Date.now(),start=end-120000;
 const samples=mergeRateHistory(data?.points??[],points).filter(p=>p.at>=start&&p.at<=end);
 const valid=samples.filter(p=>p.writeRate!==null&&Number.isFinite(p.writeRate)&&p.writeRate>=0);
 const ceiling=Math.max(1,...valid.map(p=>p.writeRate!));
 const x=(at:number)=>2+(at-start)/120000*140;
 const y=(value:number)=>30-value/ceiling*28;
 const segments:{at:number;value:number}[][]=[];
 let segment:{at:number;value:number}[]=[];
 for(const point of samples) {
  if(point.writeRate===null||!Number.isFinite(point.writeRate)||point.writeRate<0) {segment=[];continue;}
  if(!segment.length||point.at-segment.at(-1)!.at>Math.max(6000,(data?.resolution_ms??2000)*3)) {segment=[];segments.push(segment);}
  segment.push({at:point.at,value:point.writeRate});
 }
 return <figure className="w-full min-w-36" aria-label={t("Débit Backup Client → VAULTCENTER de {barcode}", {barcode})}>
  {valid.length?<>
   <svg viewBox="0 0 144 32" preserveAspectRatio="none" className="h-8 w-full text-primary" role="img" aria-label={t("Mesures des deux dernières minutes")}>
    <title>{t("Écritures PBS sur les deux dernières minutes. Les interruptions de mesure restent visibles.")}</title>
    {segments.map((s,i)=>s.length===1?<circle key={i} cx={x(s[0].at)} cy={y(s[0].value)} r="2" fill="currentColor"/>:<path key={i} d={s.map((p,j)=>`${j?'L':'M'}${x(p.at).toFixed(2)},${y(p.value).toFixed(2)}`).join(' ')} fill="none" stroke="currentColor" strokeWidth="1.75" vectorEffect="non-scaling-stroke" strokeLinecap="round" strokeLinejoin="round"/>)}
   </svg>
   <figcaption className="mt-1 text-xs tabular-nums text-muted-foreground">{rate===null?t('Historique'):`${formatBytes(rate)}/s`}</figcaption>
  </>:<figcaption className="text-xs text-muted-foreground">{loading?t('Chargement…'):error?t('Débit indisponible'):t('Aucune mesure')}</figcaption>}
 </figure>;
}
