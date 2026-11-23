"use client";
import {useContext,useEffect,useId,useState} from 'react';
import {ArrowRight,CheckCircle2,ChevronDown,ChevronLeft,ChevronRight,CircleHelp,Clock,Cloud,Database,HardDrive,LoaderCircle,RotateCcw,TriangleAlert} from 'lucide-react';
import {Button} from '@/components/ui/button';
import {type Job,formatBytes,isActive} from '@/lib/api';
import {exportPhases,exportProgress} from '@/lib/export-progress';
import {getLocale,isPlural,useI18n} from '@/lib/i18n';
import {type StorageBucket,bucketLink} from './bucket-selector';
import {useThroughputHistory} from './use-throughput-history';
import {useScopedRequest,VtlScope} from './vtl-scope';

// Keep completed evidence separate from lifecycle percentages and old receipts.
function verifiedBytes(job:Job,vtlId:string) {
 if(job.status!=='completed')return null;
 try {
  const output=JSON.parse(job.message??'null'),receipt=output?.receipt;
  const proof=receipt?.integrity,scope=receipt?.scope,copy=receipt?.generation;
  if(output.status!=='verified'||proof?.status!=='verified'||proof.method!=='s3_sha256'||
   !Number.isSafeInteger(proof.verified_at)||proof.verified_at<=0||
   !Number.isSafeInteger(proof.verified_bytes)||proof.verified_bytes<0||
   !Number.isSafeInteger(proof.verified_segments)||proof.verified_segments<0||
   scope?.job_id!==job.id||scope.vtl_id!==vtlId||scope.destination_id!==job.destination_id||
   scope.configuration_revision!==job.configuration_revision||copy?.barcode!==job.subject||copy.generation!==job.generation||
   copy.logical_size_bytes!==proof.verified_bytes||copy.segment_count!==proof.verified_segments)return null;
  return proof.verified_bytes as number;
 }catch{return null;}
}

function TapeFlow({job,connected,now}:{job:Job;connected:boolean;now:number}) {
 const {t}=useI18n(),progress=exportProgress(job);
 const history=useThroughputHistory({barcode:job.subject,transferId:job.id,kind:'s3',seconds:120,enabled:connected&&job.status==='running'});
 const point=history.data?.points.at(-1);
 const rate=connected&&!history.error&&history.data?.status==='available'&&point&&point.at<=now+5000&&now-point.at<20000?point.writeRate:null;
 const flowing=job.status==='running'&&progress?.phase==='uploading'&&rate!==null&&rate>0;
 const percent=progress?.total?progress.completed/progress.total*100:null;
 const percentage=percent===null?null:percent>0&&percent<0.1?`< ${(0.1).toLocaleString(getLocale())} %`:`${percent.toLocaleString(getLocale(),{minimumFractionDigits:1,maximumFractionDigits:1})} %`;
 const status=!connected?t('Connexion interrompue'):progress?t(exportPhases[progress.phase]):t('Transfert en cours');
 return <article className="lto-s3-flow" aria-label={t('Externalisation de {barcode}',{barcode:job.subject})} data-flowing={flowing}>
  <div className="lto-s3-source"><HardDrive aria-hidden="true"/><strong>{job.subject}</strong><span>{t('Vers S3')}</span></div>
  <div className="lto-s3-path">
   <div className="lto-s3-flow-values"><span>{progress?`${formatBytes(progress.completed)}${progress.total!==null?` / ${formatBytes(progress.total)}`:''}`:'—'}</span><strong>{percentage??''}</strong></div>
   <div className="lto-s3-track" role="progressbar" aria-label={t('Progression S3 de {barcode}',{barcode:job.subject})} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent??undefined} aria-valuetext={percentage??status}>
    <span className="lto-s3-fill" style={{width:`${percent??0}%`}}/>
    <span className="lto-s3-binary" aria-hidden="true"><span className="lto-s3-packet">{'0101100100111010'.repeat(64)}</span></span>
   </div>
   <span className="lto-s3-phase">{status}</span>
  </div>
  <div className="lto-s3-rate"><strong>{rate===null?'—':`${formatBytes(rate)}/s`}</strong><span>{t(rate===null?'Débit indisponible':'Débit actuel')}</span></div>
 </article>;
}

/** When a send ended: the hour today, the date before. */
function Ended({job}:{job:Job}) {
 const {locale}=useI18n(),date=new Date(job.updated_at);
 if(Number.isNaN(date.getTime()))return null;
 const today=date.toDateString()===new Date().toDateString();
 return <time dateTime={job.updated_at} title={date.toLocaleString(locale)}>{today?date.toLocaleTimeString(locale,{hour:'2-digit',minute:'2-digit'}):date.toLocaleDateString(locale,{day:'2-digit',month:'2-digit'})}</time>;
}

// A waiting tape has nothing to measure yet: its size and its place in the queue are enough.
function QueuedFlow({job}:{job:Job}) {
 const {t}=useI18n(),total=exportProgress(job)?.total;
 return <article className="lto-s3-row" aria-label={t('Externalisation de {barcode}',{barcode:job.subject})}>
  <div className="lto-s3-source"><HardDrive aria-hidden="true"/><strong>{job.subject}</strong><span>{t('Vers S3')}</span></div>
  <span className="lto-s3-phase"><Clock aria-hidden="true"/>{t('En attente')}</span>
  {total!=null&&<span className="lto-s3-row-size">{formatBytes(total)}</span>}
 </article>;
}

// A failure stays until its tape is sent again or deleted; it has nothing to measure.
function FailedFlow({job,onRetry}:{job:Job;onRetry?:()=>void}) {
 const {t}=useI18n();
 return <article className="lto-s3-row" aria-label={t('Externalisation de {barcode}',{barcode:job.subject})}>
  <div className="lto-s3-source"><HardDrive aria-hidden="true"/><strong>{job.subject}</strong><span>{t('Vers S3')}</span></div>
  <span className="lto-s3-phase lto-s3-error"><TriangleAlert aria-hidden="true"/>{t('Échec')}</span>
  <Ended job={job}/>
  {onRetry&&<Button size="sm" variant="outline" aria-label={t('Reprendre l’envoi de {barcode}',{barcode:job.subject})} onClick={onRetry}><RotateCcw data-icon="inline-start"/>{t('Reprendre')}</Button>}
 </article>;
}

const PAGE=5,QUEUE=5;

// Finished sends are evidence, not activity: one line each, five per page.
function FinishedList({jobs,vtlId}:{jobs:Job[];vtlId:string}) {
 const {t}=useI18n(),[page,setPage]=useState(0);
 const last=Math.max(0,Math.ceil(jobs.length/PAGE)-1),current=Math.min(page,last);
 if(!jobs.length)return null;
 const title=t('Terminés récemment');
 return <div className="lto-s3-finished">
  <h4>{title}</h4>
  <ul aria-label={title}>{jobs.slice(current*PAGE,current*PAGE+PAGE).map(job=>{
   const verified=verifiedBytes(job,vtlId);
   return <li key={job.id}>
    {verified!==null?<CheckCircle2 aria-hidden="true" className="lto-s3-verified-mark"/>:<CircleHelp aria-hidden="true" className="lto-s3-unconfirmed-mark"/>}
    <strong>{job.subject}</strong>
    <span className="lto-s3-finished-state">{t(verified!==null?'Empreinte validée':'Empreinte non confirmée')}</span>
    <span className="lto-s3-finished-size">{verified!==null?formatBytes(verified):'—'}</span>
    <Ended job={job}/>
   </li>;
  })}</ul>
  {last>0&&<div className="lto-s3-pager">
   <Button variant="outline" size="icon-sm" aria-label={t('Page précédente des envois terminés')} disabled={!current} onClick={()=>setPage(current-1)}><ChevronLeft/></Button>
   <span>{t('Page {page} sur {total}',{page:current+1,total:last+1})}</span>
   <Button variant="outline" size="icon-sm" aria-label={t('Page suivante des envois terminés')} disabled={current===last} onClick={()=>setPage(current+1)}><ChevronRight/></Button>
  </div>}
 </div>;
}

// One bucket: what moves, then what failed, then what is done.
function DestinationFlows({active,failed,finished,connected,now,vtlId,retryable,onRetry}:{active:Job[];failed:Job[];finished:Job[];connected:boolean;now:number;vtlId:string;retryable:Set<string>;onRetry?:(barcode:string)=>void}) {
 const {t}=useI18n(),[unfolded,setUnfolded]=useState(false);
 const running=active.filter(j=>j.status!=='queued'),queued=active.filter(j=>j.status==='queued');
 const hidden=Math.max(0,queued.length-QUEUE);
 const shown=[...running,...(unfolded?queued:queued.slice(0,QUEUE))];
 if(!active.length&&!failed.length&&!finished.length)return <div className="lto-s3-flows"><p className="lto-s3-empty">{t('Aucune cartouche en cours d’envoi vers ce bucket.')}</p></div>;
 return <div className="lto-s3-flows">
  {shown.map(job=>job.status==='queued'?<QueuedFlow key={job.id} job={job}/>:<TapeFlow key={job.id} job={job} connected={connected} now={now}/>)}
  {hidden>0&&<button type="button" className="lto-s3-more" aria-expanded={unfolded} onClick={()=>setUnfolded(value=>!value)}>{unfolded?t('Réduire la file d’attente'):t('+ {count} en attente',{count:hidden})}</button>}
  {failed.map(job=><FailedFlow key={job.id} job={job} onRetry={onRetry&&retryable.has(job.subject)?()=>onRetry(job.subject):undefined}/>)}
  <FinishedList jobs={finished} vtlId={vtlId}/>
 </div>;
}

const FOLDED='vaultcenter.media.s3-shelf.folded';

type ShelfProps={jobs:Job[];connected:boolean;
 /** Barcodes deleted locally; their finished transfers leave the shelf. */
 deleted:string[];
 /** Barcodes whose failed send may be resumed now, and how to ask for it. */
 retryable?:string[];onRetry?:(barcode:string)=>void};
export function MediaS3Flows(props:ShelfProps) {
 const vtlId=useContext(VtlScope);
 return vtlId?<S3Shelf key={vtlId} vtlId={vtlId} {...props}/>:null;
}
function S3Shelf({jobs,connected,vtlId,deleted,retryable=[],onRetry}:ShelfProps&{vtlId:string}) {
 const {t}=useI18n(),request=useScopedRequest(),contentId=useId();
 // Remembered per browser; read after mounting so the static page hydrates unfolded.
 const [folded,setFolded]=useState(false);
 useEffect(()=>{try{setFolded(localStorage.getItem(FOLDED)==='1');}catch{}},[]);
 function toggle(){setFolded(value=>{try{localStorage.setItem(FOLDED,value?'0':'1');}catch{}return !value;});}
 const [buckets,setBuckets]=useState<StorageBucket[]>([]),[loading,setLoading]=useState(true),[error,setError]=useState(false),[now,setNow]=useState(()=>Date.now());
 useEffect(()=>{const timer=setInterval(()=>setNow(Date.now()),5000);return()=>clearInterval(timer);},[]);
 useEffect(()=>{
  const controller=new AbortController();let pending=false;
  const refresh=async()=>{
   if(pending||document.hidden)return;
   pending=true;
   try{
    const values=await request<StorageBucket[]>('/v1/storage/buckets',{signal:AbortSignal.any([controller.signal,AbortSignal.timeout(10000)])});
    if(!Array.isArray(values)||!values.every(b=>typeof b.id==='string'&&typeof b.bucket==='string'))throw Error('Invalid buckets');
    if(!controller.signal.aborted){setBuckets(values);setError(false);}
   }catch{if(!controller.signal.aborted)setError(true);}
   finally{pending=false;if(!controller.signal.aborted)setLoading(false);}
  };
  void refresh();const timer=setInterval(refresh,30000);
  document.addEventListener('visibilitychange',refresh);
  return()=>{controller.abort();clearInterval(timer);document.removeEventListener('visibilitychange',refresh);};
 },[request]);
 // All active tapes remain visible across magazine filters/pages, the oldest request
 // first. Each other tape keeps its most recent result only, so a failed attempt is
 // not shown beside its retry.
 const exports=jobs.filter(j=>j.kind==='export').toSorted((a,b)=>b.created_at.localeCompare(a.created_at));
 const active=exports.filter(isActive).toSorted((a,b)=>Number(a.status==='queued')-Number(b.status==='queued')||a.created_at.localeCompare(b.created_at));
 const seen=new Set(active.map(j=>j.subject)),gone=new Set(deleted);
 const latest=exports.filter(j=>{if(seen.has(j.subject))return false;seen.add(j.subject);return !gone.has(j.subject);});
 const failed=latest.filter(j=>j.status==='failed');
 const finished=latest.filter(j=>j.status==='completed').toSorted((a,b)=>b.updated_at.localeCompare(a.updated_at));
 const visible=[...active,...failed,...finished];
 const destinations=[...new Set([...visible.map(j=>j.destination_id??''),...buckets.filter(b=>b.active).map(b=>b.id)])];
 const running=active.filter(j=>j.status!=='queued').length,queued=active.length-running;
 const counts=[running&&t('{count} en cours',{count:running}),queued&&t('{count} en attente',{count:queued})].filter(Boolean).join(' · ');
 const failures=failed.length?t(isPlural(failed.length)?'{count} échecs':'{count} échec',{count:failed.length}):'';
 const resumable=new Set(retryable),of=(list:Job[],id:string)=>list.filter(j=>(j.destination_id??'')===id);
 return <section className="lto-s3-shelf" aria-label={t('Stockage S3 — externalisation')}>
  <header className="lto-s3-heading"><h3><button type="button" className="lto-s3-toggle" aria-expanded={!folded} aria-controls={contentId} onClick={toggle}><ChevronDown aria-hidden="true" className="lto-s3-chevron"/><Cloud aria-hidden="true"/>{t('Stockage S3')}</button></h3><span>{folded&&active.length>0&&<LoaderCircle aria-hidden="true" className="lto-s3-running"/>}<span>{counts}{counts&&failures?' · ':''}{failures&&<span className="lto-s3-error">{failures}</span>}{!counts&&!failures&&t('Aucun envoi en cours')}</span></span></header>
  <div id={contentId} hidden={folded}>
  {error&&<p className="lto-s3-notice" role="status">{t('Destinations S3 indisponibles — nouvelle tentative automatique.')}</p>}
  {!connected&&<p className="lto-s3-notice" role="status">{t('Connexion interrompue — dernières informations reçues.')}</p>}
  {destinations.map(id=>{
   const bucket=buckets.find(b=>b.id===id);
   const name=bucket?.bucket??t('Destination non confirmée');
   return <div className="lto-s3-destination" role="group" aria-label={t('Destination {bucket}',{bucket:name})} key={id}>
    <DestinationFlows active={of(active,id)} failed={of(failed,id)} finished={of(finished,id)} connected={connected} now={now} vtlId={vtlId} retryable={resumable} onRetry={onRetry}/>
    <div className="lto-s3-bucket"><Database aria-hidden="true"/><div><strong>{name}</strong><span>{t(bucket?.active?'Destination des nouveaux envois':'Destination de ces cartouches')}</span>{bucket&&<a href={bucketLink('storage',bucket.id)}>{t('Voir le stockage S3')}<ArrowRight aria-hidden="true"/></a>}</div></div>
   </div>;
  })}
  {!destinations.length&&<p className="lto-s3-empty">{t(loading?'Chargement des buckets…':error?'Les flux apparaîtront dès que les informations seront disponibles.':'Aucune destination S3 active')}</p>}
  </div>
 </section>;
}
