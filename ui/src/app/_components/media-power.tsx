"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import {useCallback,useContext,useEffect,useRef,useState} from 'react';
import {Power,LoaderCircle,Wrench} from 'lucide-react';
import {localizedToast as toast} from './providers';
import {type Job,isActive} from '@/lib/api';
import type {Telemetry} from '@/lib/telemetry';
import type {MaintenanceStatus} from './maintenance';
import {VtlScope,useScopedRequest} from './vtl-scope';
import {Button} from '@/components/ui/button';
import {Checkbox} from '@/components/ui/checkbox';
import {Dialog,DialogContent,DialogDescription,DialogFooter,DialogHeader,DialogTitle} from '@/components/ui/dialog';

export type PowerControls={canManage:boolean;connected:boolean;jobs:Job[];onRefresh:()=>Promise<void>;onMaintenance:()=>void;maintenanceUpdate?:MaintenanceStatus|null;maintenancePending?:boolean;onMaintenanceStatus?:(status:MaintenanceStatus)=>void};
type PowerStatus=MaintenanceStatus&{original?:Record<string,string>};
function serviceState(status:PowerStatus|null) {
 const services=Object.values(status?.services??{});
 if(!status?.configured||!services.length)return 'unknown';
 if(status.phase==='online'&&!status.blocked&&services.every(value=>value==='active'))return 'on';
 if(status.phase==='maintenance'&&status.ready&&status.blocked&&services.every(value=>value==='inactive'))return 'off';
 return 'unknown';
}
function blocker(status:PowerStatus|null,jobs:Job[],data:Telemetry|null):string|null {
 if(!status?.configured)return t("La commande d’alimentation n’est pas configurée pour cette bibliothèque.");
 if(status.reintegration_pending)return t("Une réintégration doit être terminée depuis le bouton Maintenance de la bibliothèque.");
 if(jobs.some(isActive))return t("Une opération VaultCenter est en cours. Attendez sa fin.");
 if(status.message)return status.message;
 const power=serviceState(status);
 if(power==='unknown')return t("L’état des services ne permet pas cette action. Consultez le bouton Maintenance de la bibliothèque.");
 if(power==='off') {
  if(!status.original||!Object.values(status.original).length||!Object.values(status.original).every(value=>value==='active'))return t("La reprise nécessite une vérification de l’état initial depuis le bouton Maintenance de la bibliothèque.");
  return null;
 }
 if(data?.status==='available') {
  if(data.drives.some(d=>['writing','reading','loading','unloading'].includes(d.state))||data.demo_transfer?.status==='running')return t("Une copie ou un mouvement de cartouche est en cours. Attendez sa fin.");
  if(data.drives.some(d=>d.barcode))return status.client==='veeam'?t("Déchargez la cartouche du lecteur avant d’arrêter la bibliothèque."):t("Déchargez la cartouche du lecteur depuis PBS avant d’arrêter la bibliothèque.");
 }
 // The maintenance controller acquires PBS’s drive lock and verifies tasks and
 // unloaded media atomically. Missing browser telemetry cannot bypass that guard.
 return null;
}
export function MediaPower(props:PowerControls&{data:Telemetry|null}) {
  useI18n();
 const id=useContext(VtlScope);
 return <PowerControl key={id} {...props}/>;
}
function PowerControl({canManage,connected,jobs,data,onRefresh,onMaintenance,maintenanceUpdate,maintenancePending=false,onMaintenanceStatus}:PowerControls&{data:Telemetry|null}) {
  useI18n();
 const request=useScopedRequest();
 const [status,setStatus]=useState<PowerStatus|null>(null);
 const [open,setOpen]=useState(false);
 const [checking,setChecking]=useState(false);
 const [pending,setPending]=useState(false);
 const [error,setError]=useState('');
 const [idleConfirmed,setIdleConfirmed]=useState(false);
 const busy=useRef(false);
 const alive=useRef(true);
 const revision=useRef(0);
 const readStatus=useCallback(async()=>{
  const value=await request<PowerStatus>('/v1/maintenance');
  if(typeof value.configured!=='boolean')throw Error(t("État de la bibliothèque indisponible."));
  return value;
 },[request]);
 const refresh=useCallback(async()=>{
  if(busy.current)return;
  const token=++revision.current;
  setChecking(true);
  try{const value=await readStatus();if(alive.current&&token===revision.current){setStatus(value);onMaintenanceStatus?.(value);}}
  catch{if(alive.current&&token===revision.current)setStatus(null);}
  finally{if(alive.current&&token===revision.current)setChecking(false);}
 },[readStatus,onMaintenanceStatus]);
 useEffect(()=>{
  alive.current=true;void refresh();
  const timer=setInterval(()=>{if(!document.hidden)void refresh();},15000);
  return()=>{alive.current=false;revision.current++;clearInterval(timer);};
 },[refresh]);
 useEffect(()=>{
  if(!maintenanceUpdate)return;
  // Invalidate reads started before the confirmed transition.
  revision.current++;setStatus(maintenanceUpdate);setChecking(false);
 },[maintenanceUpdate]);
 const transitionPending=pending||maintenancePending||['entering','leaving'].includes(status?.phase??'');
 const power=serviceState(status);
 const reason=!connected?t("Connexion à la bibliothèque indisponible."):!canManage?t("Un compte administrateur est nécessaire."):blocker(status,jobs,data);
 const veeam=status?.client==='veeam';
 const needsIdleConfirmation=power==='on'&&veeam&&!idleConfirmed;
 async function confirm(){
  if(busy.current||transitionPending||checking||reason||needsIdleConfirmation)return;
  const confirmIdle=power==='on'&&veeam&&idleConfirmed;
  busy.current=true;revision.current++;setPending(true);setError('');setIdleConfirmed(false);
  try{
   const [fresh,freshJobs]=await Promise.all([readStatus(),request<Job[]>('/v1/jobs')]);
   const denied=blocker(fresh,freshJobs,data);
   if(denied)throw Error(denied);
   if(serviceState(fresh)!==power)throw Error(t("L’état de la bibliothèque a changé. Vérifiez-le avant de confirmer à nouveau."));
   const action=power==='on'?'enter':'leave';
   const result=await request<PowerStatus>(`/v1/maintenance/${action}`,{method:'POST',signal:AbortSignal.timeout(310000),...(action==='enter'&&confirmIdle?{headers:{'Content-Type':'application/json'},body:JSON.stringify({confirm_idle:true})}:{})});
   if(!alive.current)return;
   setStatus(result);onMaintenanceStatus?.(result);
   if(serviceState(result)!==(power==='on'?'off':'on'))throw Error(t("La transition n’est pas confirmée. Consultez le bouton Maintenance de la bibliothèque."));
   setOpen(false);toast.success(power==='on'?t("Bibliothèque arrêtée"):t("Bibliothèque remise en service"));
  }catch(cause){if(alive.current)setError(cause instanceof Error?cause.message:t("Transition non confirmée. Vérifiez l’état de la bibliothèque."));}
  finally{
   busy.current=false;
   if(alive.current){setPending(false);void refresh();void onRefresh().catch(()=>{});}
  }
 }
 return <>
  <div className="lto-chassis-actions">
   <span className="lto-chassis-status" data-signal={power==='on'?'live':'unknown'}><i aria-hidden="true"/>{transitionPending?t("Transition en cours"):power==='on'?t("En service"):power==='off'?t("Arrêtée"):t("État à vérifier")}</span>
   <div className="flex shrink-0 items-center gap-3">
   <button type="button" className="lto-maintenance" data-maintenance={status?.phase==='maintenance'&&status.ready?'active':'inactive'} aria-label={t("Maintenance")} title={transitionPending?t("Transition en cours…"):status?.phase==='maintenance'&&status.ready?t("Maintenance active"):t("Maintenance")} aria-haspopup="dialog" aria-busy={transitionPending} disabled={pending} onClick={onMaintenance}><Wrench aria-hidden="true" />{transitionPending&&<LoaderCircle className="lto-maintenance-ring" aria-hidden="true"/>}</button>
   <button type="button" className="lto-power" data-power={power} aria-label={t("Alimentation de la bibliothèque")} aria-haspopup="dialog" disabled={!canManage||!connected||transitionPending} title={!canManage?t("Administrateur requis"):t("Commander l’alimentation de la bibliothèque")} onClick={()=>{setError('');setIdleConfirmed(false);setOpen(true);void refresh();}}><Power aria-hidden="true"/></button>
   </div>
  </div>
  <Dialog open={open} onOpenChange={value=>{if(!pending){setOpen(value);if(!value)setIdleConfirmed(false);}}}>
   <DialogContent showCloseButton={!pending} className="sm:max-w-md">
    <DialogHeader icon={Power}>
     <DialogTitle>{power==='off'?t("Remettre la bibliothèque en service ?"):t("Arrêter la bibliothèque ?")}</DialogTitle>
     <DialogDescription>{power==='off'?t("Le lecteur, le changeur et l’accès iSCSI seront rétablis."):t("Le lecteur, le changeur et l’accès iSCSI seront arrêtés. La console reste accessible.")}</DialogDescription>
    </DialogHeader>
    {data?.source==='demo'&&<p className="text-sm text-muted-foreground">{t("Simulation locale : aucun service réel ne sera modifié.")}</p>}
    {power==='on'&&veeam&&<label className="flex items-start gap-2 text-sm"><Checkbox className="mt-0.5" checked={idleConfirmed} disabled={pending||!!reason} onCheckedChange={value=>setIdleConfirmed(value===true)}/>{t("Aucun job Veeam n’utilise cette bibliothèque")}</label>}
    {(error||reason)&&<p role="alert" className="text-sm text-destructive">{t(error || reason || "")}</p>}
    {status?.configured&&serviceState(status)==='unknown'&&<button type="button" className="self-start text-sm underline underline-offset-4" onClick={()=>{setOpen(false);onMaintenance();}}>{t("Voir la maintenance")}</button>}
    <DialogFooter>
     <Button variant="outline" disabled={pending} onClick={()=>setOpen(false)}>{t("Annuler")}</Button>
     <Button variant={power==='on'?'destructive':'default'} disabled={!!reason||checking||pending||needsIdleConfirmation} onClick={()=>void confirm()}>{pending?<><LoaderCircle aria-hidden="true" className="size-4 motion-safe:animate-spin"/>{t("Transition en cours…")}</>:checking?t("Vérification…"):power==='off'?t("Remettre en service"):t("Arrêter")}</Button>
    </DialogFooter>
   </DialogContent>
  </Dialog>
 </>;
}
