import type {Job} from './api';

export const exportPhases = {
 preparing: 'Préparation de la cartouche',
 uploading: 'Envoi vers S3',
 verifying: 'Vérification de la copie S3',
 finalizing: 'Finalisation de la copie S3',
};
// Only the worker's measured counters describe payload progress. job.progress
// on older workers is a fixed lifecycle milestone, not a byte percentage.
export function exportProgress(job: Job) {
 if(job.kind !== 'export' || !['running','queued'].includes(job.status))return null;
 try {
  const value=JSON.parse(job.message??'null')?.export_progress;
  if(!value || !Object.hasOwn(exportPhases,value.phase) || !Number.isSafeInteger(value.completed_bytes) || value.completed_bytes<0 ||
    (value.total_bytes!==null && (!Number.isSafeInteger(value.total_bytes)||value.total_bytes<value.completed_bytes)))return null;
  return {phase:value.phase as keyof typeof exportPhases,completed:value.completed_bytes as number,total:value.total_bytes as number|null,
   percent:value.total_bytes>0?Math.floor(value.completed_bytes/value.total_bytes*100):null};
 }catch{return null;}
}

export const restorePhases = {
 preparing: 'Préparation du téléchargement',
 downloading: 'Téléchargement depuis S3',
 verifying: 'Vérification des données téléchargées',
 installing: 'Installation dans la bibliothèque',
};
export function restoreProgress(job: Job) {
 if(!['restore','recovery','recovery_install'].includes(job.kind) || !['running','queued'].includes(job.status))return null;
 try {
  const value=JSON.parse(job.message??'null')?.restore_progress;
  if(!value || !Object.hasOwn(restorePhases,value.phase) || !Number.isSafeInteger(value.completed_bytes) || value.completed_bytes<0 ||
    (value.total_bytes!==null && (!Number.isSafeInteger(value.total_bytes)||value.total_bytes<value.completed_bytes)))return null;
  return {phase:value.phase as keyof typeof restorePhases,completed:value.completed_bytes as number,total:value.total_bytes as number|null,
   percent:value.total_bytes>0?Math.floor(value.completed_bytes/value.total_bytes*100):null};
 }catch{return null;}
}
export function transferProgress(job: Job) {
 const upload=exportProgress(job);
 if(upload)return {...upload,label:exportPhases[upload.phase],download:false};
 const download=restoreProgress(job);
 return download?{...download,label:restorePhases[download.phase],download:true}:null;
}
