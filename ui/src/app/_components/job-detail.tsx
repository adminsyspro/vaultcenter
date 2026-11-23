"use client";
import {type Job,formatBytes,formatDate,isActive,jobSummary} from '@/lib/api';
import {transferProgress} from '@/lib/export-progress';
import {useI18n} from '@/lib/i18n';
import {Progress} from '@/components/ui/progress';
import {StatusBadge} from './status-badge';
import {WorkerResult} from './worker-result';

export function JobDetail({job}:{job:Job}) {
 const {t}=useI18n();
 const transfer=transferProgress(job),active=isActive(job);
 return <div className="min-w-0 space-y-5">
  <StatusBadge state={job.status} job/>
  {job.status!=='failed'&&<p className="text-sm leading-relaxed">{transfer?t(transfer.label):jobSummary(job)}</p>}
  {transfer?.percent!=null&&<div className="space-y-2">
   <div className="flex justify-between gap-3 text-sm tabular-nums"><span>{formatBytes(transfer.completed)} / {formatBytes(transfer.total!)}</span><span>{transfer.percent} %</span></div>
   <Progress value={transfer.percent} aria-label={t(transfer.download?'Données téléchargées depuis S3':'Données envoyées vers S3')} aria-valuenow={transfer.percent}/>
   {transfer.percent===100&&<p className="text-xs text-muted-foreground">{t(transfer.download?'Téléchargement terminé. La vérification et la réintégration peuvent se poursuivre.':'Envoi terminé. La copie doit encore être vérifiée et finalisée.')}</p>}
  </div>}
  {job.destination&&<p className="break-all text-sm"><span className="text-muted-foreground">{t('Dossier restauré sur l’appliance')} : </span>{job.destination}</p>}
  <WorkerResult job={job} compact hideMessages={active} metadata={<dl className="grid grid-cols-2 gap-4 text-sm">
   <div className="col-span-2"><dt className="text-muted-foreground">{t('Identifiant')}</dt><dd className="mt-1 break-all font-mono text-xs">{job.id}</dd></div>
   {job.generation!=null&&<div><dt className="text-muted-foreground">{t('Version')}</dt><dd>{job.generation}</dd></div>}
   <div><dt className="text-muted-foreground">{t('Création')}</dt><dd>{formatDate(job.created_at)}</dd></div>
   <div><dt className="text-muted-foreground">{t('Dernière mise à jour')}</dt><dd>{formatDate(job.updated_at)}</dd></div>
  </dl>}/>
 </div>;
}
