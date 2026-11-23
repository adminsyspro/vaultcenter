"use client";
import {useI18n} from '@/lib/i18n';
import {useContext,useEffect,useState} from 'react';
import {driveLabels,type Telemetry} from '@/lib/telemetry';
import {type Job,formatBytes,kindLabels,jobSummary,isActive} from '@/lib/api';
import {transferProgress} from '@/lib/export-progress';
import {Clock,LoaderCircle,CircleCheck,CircleAlert} from 'lucide-react';
import {VtlScope} from './vtl-scope';
import {maintenanceLine,onMaintenance,type MaintenanceSnapshot} from '@/lib/maintenance-events';

type Observation={message:string;params?:Record<string,string>;state?:string;job?:Job};
type Entry=Observation&{id:number;at:number};
type Journal={observed:Map<string,string>;entries:Entry[];nextId:number;cleared:boolean};

type Props={data:Telemetry|null;jobs:Job[];connected:boolean};
export function MediaLcd({data,jobs,connected}:Props) {
 const vtlId=useContext(VtlScope);
 return <LcdJournal key={vtlId} data={data} jobs={jobs} connected={connected}/>;
}

function LcdJournal({data,jobs,connected}:Props) {
 const {t,locale}=useI18n();
 const vtlId=useContext(VtlScope);
 const active=jobs.filter(isActive);
 // The queue is one line here: its tapes are listed on the S3 shelf and in the activity tab.
 const running=active.filter(job=>job.status!=='queued'),queued=active.length-running.length;
 const operationText=(job:Job)=>`${t(kindLabels[job.kind]??'Opération en arrière-plan')} · ${job.subject} · ${job.status==='failed'?`${t('Échec')} · ${jobSummary(job)}`:job.status==='completed'?t('Terminé'):transferProgress(job)?t(transferProgress(job)!.label):jobSummary(job)}`;
 const [journal,setJournal]=useState<Journal>({observed:new Map(),entries:[],nextId:0,cleared:false});
 const [maintenance,setMaintenance]=useState<MaintenanceSnapshot|null>(null);
 useEffect(()=>onMaintenance(vtlId??null,setMaintenance),[vtlId]);
 useEffect(()=>{

  // Store source messages and raw identifiers so changing language preserves history.
  const observations=new Map<string,Observation>();
  if(data)observations.set('source',{message:data.status==='available'?'Suivi du lecteur disponible':data.status==='stale'?'Relevé ancien · activité non confirmée':'Suivi du lecteur indisponible'});
  if(data?.status==='available'){
   for(const drive of data.drives)observations.set(`drive:${drive.id}`,{message:'{label} · {state}{barcode}',params:{label:drive.label,barcode:drive.barcode?` · ${drive.barcode}`:''},state:driveLabels[drive.state]});
   const transfer=data.source==='demo'?data.demo_transfer:undefined;
   if(transfer)observations.set(`transfer:${transfer.id}`,{message:'{barcode} → {bucket} · {state}',params:{barcode:transfer.barcode,bucket:transfer.bucket},state:transfer.stale?'Mesures interrompues':transfer.status==='running'?'Externalisation en cours':'Externalisation terminée'});
  }
  const line=maintenance?maintenanceLine(maintenance):null;
  if(line)observations.set('maintenance',{message:line});
  for(const job of [...jobs].sort((a,b)=>a.updated_at.localeCompare(b.updated_at)).slice(-20))observations.set(`job:${job.id}`,{message:'operation',job});
  setJournal(previous=>{
   let nextId=previous.nextId;
   const additions:Entry[]=[];
   const observed=new Map<string,string>();
   for(const [key,observation] of observations){
    const signature=observation.job?`${observation.job.status}:${transferProgress(observation.job)?.phase??(observation.job.message?.includes('installing')?'installing':observation.job.kind)}`:JSON.stringify(observation);
    observed.set(key,signature);
    if(previous.observed.get(key)!==signature)additions.push({id:nextId++,at:observation.job?(Date.parse(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(observation.job.updated_at)?observation.job.updated_at.replace(' ','T')+'Z':observation.job.updated_at)||Date.now()):data?.status==='available'?(data.generated_at??Date.now()):Date.now(),...observation});
   }
   return {observed,entries:[...additions.reverse(),...previous.entries].slice(0,40),nextId,cleared:previous.cleared};
  });
 },[data,jobs,maintenance]);
 return <section className="lto-lcd" aria-label={t('Journal d’activité de la bibliothèque')}>
  <header><h3>{t('Journal d’activité')}</h3><span>{t(data?.source==='demo'?'Simulation · session en cours':'Session en cours')}</span>
   {/* Only the lines go: what is already observed stays known, so the same states do not come back. */}
   <button type="button" className="lto-lcd-clear" disabled={!journal.entries.length} onClick={()=>setJournal(previous=>({...previous,entries:[],cleared:true}))} aria-label={t('Effacer le journal d’activité')}>{t('Effacer')}</button></header>
  {!connected&&<p role="status" className="mt-2 flex items-center gap-2 text-xs"><CircleAlert className="size-3 shrink-0" aria-hidden="true"/>{t('Suivi interrompu · dernier état connu')}</p>}
  {active.length>0&&<div className="lto-lcd-operations" aria-label={t('Opérations en cours')}>
   {running.map(job=>{const progress=transferProgress(job),Icon=LoaderCircle;return <a key={job.id} href={`#jobs?vtl=${encodeURIComponent(vtlId??'')}`} className="lto-lcd-operation">
    <Icon className={`size-3 shrink-0 ${connected&&job.status==='running'?'motion-safe:animate-spin':''}`} aria-hidden="true"/>
    <span><strong>{t(kindLabels[job.kind]??'Opération en arrière-plan')} · {job.subject}</strong><span>{progress?t(progress.label):jobSummary(job)}{progress?.percent!=null?` · ${progress.percent} % · ${formatBytes(progress.completed)} / ${formatBytes(progress.total!)}`:''}</span></span>
   </a>;})}
   {queued>0&&<a href={`#jobs?vtl=${encodeURIComponent(vtlId??'')}`} className="lto-lcd-operation"><Clock className="size-3 shrink-0" aria-hidden="true"/><span>{t(running.length?'+ {count} en attente':'{count} en attente',{count:queued})}</span></a>}
  </div>}
  <div className="lto-lcd-log" role="log" aria-live="off" aria-label={t('Dernières observations du lecteur')} tabIndex={0}>
   {journal.entries.length?journal.entries.map(entry=><div className="lto-lcd-entry" key={entry.id}><time dateTime={new Date(entry.at).toISOString()}>{new Date(entry.at).toLocaleTimeString(locale,{hour:'2-digit',minute:'2-digit',second:'2-digit'})}</time><span>{entry.job?<><span className="inline-flex items-center gap-1">{entry.job.status==='failed'?<CircleAlert className="size-3 shrink-0" aria-hidden="true"/>:entry.job.status==='completed'?<CircleCheck className="size-3 shrink-0" aria-hidden="true"/>:null}{operationText(entry.job)}</span></>:t(entry.message,{...entry.params,...(entry.params?.label?{label:t(entry.params.label)}:{}),...(entry.state?{state:t(entry.state)}:{})})}</span></div>):<p>{t(journal.cleared?'Journal effacé · les prochains changements s’afficheront ici.':'En attente du premier relevé du lecteur…')}</p>}
  </div>
 </section>;
}
