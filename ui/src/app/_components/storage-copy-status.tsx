"use client";
import {CircleOff,Cloud,FileWarning,LockKeyhole,Snowflake,LoaderCircle,TriangleAlert,Trash2} from 'lucide-react';
import {Badge} from '@/components/ui/badge';
import {useI18n} from '@/lib/i18n';
import type {RecoveryCopy} from './storage-recovery-dialog';

type Status={label:string;icon:typeof Cloud;color:string;busy:boolean};
const blue='border-sky-300 bg-sky-50 text-sky-900 dark:border-sky-500/40 dark:bg-sky-500/10 dark:text-sky-200';
const amber='border-amber-300 bg-amber-50 text-amber-900 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200';
/** State of a bucket as seen through its S3 access probe; `readable` must already account for the manifest when describing a copy. */
export function accessStatus({raw_status,readable,writable}:{raw_status?:string|null;readable:boolean;writable?:boolean}):Status {
 switch(raw_status){
  case 'Archived':return {label:'Archivé · lecture seule',icon:Snowflake,color:blue,busy:false};
  case 'Archiving':return {label:'Archivage en cours',icon:LoaderCircle,color:blue,busy:true};
  case 'Restoring':return {label:'Rappel en cours',icon:LoaderCircle,color:amber,busy:true};
  case 'Restored':return {label:'Disponible · lecture seule',icon:LockKeyhole,color:blue,busy:false};
  case 'Deleting':return {label:'Suppression du bucket en cours',icon:LoaderCircle,color:amber,busy:true};
 }
 if(readable)return {label:writable?'Disponible sur S3':'Disponible · lecture seule',icon:Cloud,color:'border-emerald-300 bg-emerald-50 text-emerald-900 dark:border-emerald-500/40 dark:bg-emerald-500/10 dark:text-emerald-200',busy:false};
 return {label:'Accès à vérifier',icon:TriangleAlert,color:amber,busy:false};
}
export function copyStatus(copy:RecoveryCopy):Status {
 if(copy.deletion_status==='running'||copy.deletion_status==='queued')return {label:'Suppression en cours',icon:LoaderCircle,color:amber,busy:true};
 if(copy.deletion_status==='failed')return {label:'Suppression interrompue',icon:Trash2,color:'border-destructive/40 text-destructive',busy:false};
 return accessStatus({...copy,readable:copy.readable&&!!copy.manifest_sha256});
}
function StatusBadge({status}:{status:Status}){
 const {t}=useI18n();const Icon=status.icon;
 return <Badge variant="outline" className={`gap-1.5 whitespace-nowrap ${status.color}`}><Icon aria-hidden="true" className={`size-3.5 ${status.busy?'motion-safe:animate-spin':''}`}/>{t(status.label)}</Badge>;
}
export function StorageCopyStatus({copy}:{copy:RecoveryCopy}){
 return <StatusBadge status={copyStatus(copy)}/>;
}
/** Bucket-level badge: an inventory that could not be read is reported as such, not guessed from a missing access probe. */
export function StorageAccessStatus({access}:{access:{raw_status:string|null;readable:boolean;writable:boolean}|null}){
 return <StatusBadge status={access?accessStatus(access):{label:'Lecture impossible',icon:TriangleAlert,color:'border-destructive/40 text-destructive',busy:false}}/>;
}
const problems:Record<'gone'|'unreadable'|'manifests',Status>={
 gone:{label:'Introuvable chez OVH',icon:CircleOff,color:'border-destructive/40 text-destructive',busy:false},
 unreadable:{label:'Lecture impossible',icon:TriangleAlert,color:'border-destructive/40 text-destructive',busy:false},
 manifests:{label:'Manifestes illisibles',icon:FileWarning,color:amber,busy:false},
};
/** What went wrong while reading a bucket, shown on its row instead of the raw error. */
export function StorageBucketProblem({problem}:{problem:keyof typeof problems}){
 return <StatusBadge status={problems[problem]}/>;
}
