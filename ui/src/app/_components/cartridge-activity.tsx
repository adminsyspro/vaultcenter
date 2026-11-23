"use client";
import {useEffect,useState} from 'react';
import {useI18n} from '@/lib/i18n';
import {formatBytes,formatDate,isActive,jobLabels,type Job} from '@/lib/api';
import {Button} from '@/components/ui/button';
import {MediaThroughput} from './media-throughput';
import {useScopedRequest} from './vtl-scope';

export function CartridgeActivity({barcode,jobs}:{barcode:string;jobs:Job[]}) {
 const {t}=useI18n();
 const [selected,setSelected]=useState<Job|null>(null);
 const [selectionId,setSelectionId]=useState(0);
 const request=useScopedRequest();
 const [exports,setExports]=useState<Job[]|null>(null);
 const [error,setError]=useState(false);
 const [retry,setRetry]=useState(0);
 const revision=jobs.filter(job=>job.kind==='export'&&job.subject===barcode).map(job=>`${job.id}:${job.status}`).join(',');
 useEffect(()=>{
  const controller=new AbortController();setError(false);
  void request<Job[]>(`/v1/jobs?${new URLSearchParams({barcode,kind:'export'})}`,{signal:controller.signal})
   .then(values=>{if(!Array.isArray(values)||values.some(job=>job.kind!=='export'||job.subject!==barcode))throw Error('Invalid export history');if(!controller.signal.aborted)setExports(values);})
   .catch(()=>{if(!controller.signal.aborted)setError(true);});
  return()=>controller.abort();
 },[barcode,revision,retry,request]);
 const finishedAt=selected&&!isActive(selected)?Date.parse(selected.updated_at.includes('T')?selected.updated_at:selected.updated_at.replace(' ','T')+'Z'):NaN;
 return <div className="lto-cartridge-activity">
  <MediaThroughput key={selectionId} barcode={barcode} transferId={selected?.id} metricTitle={selected?t('Débit vers S3'):undefined} initialEndMs={Number.isFinite(finishedAt)?Math.min(Date.now(),finishedAt+1000):undefined} points={[]} writeRate={null} localGrowth={null} available={false} cartridgeHistory/>
  <section aria-label={t('Exports S3 de la cartouche')} className="space-y-3">
   <h3 className="font-medium">{t('Exports S3 précédents')}</h3>
   {error?<div className="space-y-2"><p role="alert">{t('Historique des exports S3 indisponible.')}</p><Button variant="outline" size="sm" onClick={()=>setRetry(value=>value+1)}>{t('Réessayer')}</Button></div>:exports===null?<p>{t('Chargement des exports…')}</p>:exports.length?<><ul className="divide-y">{exports.map(job=>{
    let bytes:number|undefined;
    try{const result=JSON.parse(job.message??'null');if(typeof result?.logical_size_bytes==='number'&&Number.isFinite(result.logical_size_bytes)&&result.logical_size_bytes>=0)bytes=result.logical_size_bytes;}catch{}
    return <li key={job.id} className="flex flex-wrap items-center justify-between gap-2 py-3">
     <div className="space-y-1"><p>{formatDate(job.created_at)}</p><p className="text-xs text-muted-foreground">{t(jobLabels[job.status]??job.status)}{bytes!==undefined?` · ${formatBytes(bytes)}`:''}{job.generation?` · ${t('Version {generation}',{generation:job.generation})}`:''}</p></div>
     <Button variant="outline" size="sm" aria-label={t('Graphique de l’export du {date}',{date:formatDate(job.created_at)})} onClick={()=>{setSelected(job);setSelectionId(value=>value+1);}}>{t('Voir le graphique')}</Button>
    </li>;
   })}</ul>{exports.length===100&&<p className="text-xs text-muted-foreground">{t('Les 100 derniers exports de cette cartouche sont affichés.')}</p>}</>:<p className="text-sm text-muted-foreground">{t('Aucun export S3 enregistré pour cette cartouche.')}</p>}
  </section>
 </div>;
}
