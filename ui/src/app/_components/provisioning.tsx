"use client";
import { translate as t, useI18n } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { request } from "@/lib/api";

type Status={vtl_id:string;phase:string;qualified?:boolean;message?:string};
type Step="prepare"|"apply"|"activate";
const labels:Record<Step,string>={prepare:"Étape 1/3 : réservation du changeur et du lecteur…",apply:"Étape 2/3 : configuration de la bibliothèque…",activate:"Étape 3/3 : démarrage et publication iSCSI…"};
const activation=["configured","awaiting_pbs","activation_incomplete"];
// One operator action; the controller keeps its resumable prepare/apply/activate journal.
// A preparation is applied only in the same run (its token expires after 5 min);
// an older one is reused only to resume an application already in progress.
function nextStep(status:Status,token:string|null,ownToken:boolean):Step|null{
 if(status.phase==="active"||status.phase==="unavailable")return null;
 if(activation.includes(status.phase))return status.qualified?"activate":null;
 if(token && (status.phase==="applying" || (ownToken && status.phase==="prepared")))return "apply";
 return status.phase==="applying"?null:"prepare";
}
export function Provisioning({vtlId,revision,blocked=null,onStatus}:{vtlId:string;revision:number;blocked?:string|null;onStatus?:(status:Status)=>void}){
  useI18n();
 const [status,setStatus]=useState<Status|null>(null),[error,setError]=useState(""),[step,setStep]=useState<Step|null>(null),[token,setToken]=useState<string|null>(null);
 const alive=useRef(true),locked=useRef(false);
 const endpoint=`/v1/vtls/${encodeURIComponent(vtlId)}`,key=`vaultcenter-provisioning:${vtlId}`;
 useEffect(()=>{
  alive.current=true;const controller=new AbortController();
  setToken(sessionStorage.getItem(key));
  void request<Status>(endpoint+"/provisioning",{signal:controller.signal}).then(value=>{
   if(value.vtl_id!==vtlId)throw new Error(t("Réponse associée à une autre VTL."));setStatus(value);onStatus?.(value);
  }).catch(cause=>{if(!controller.signal.aborted)setError(cause instanceof Error?cause.message:t("Provisionnement indisponible."));});
  return()=>{alive.current=false;controller.abort();};
 },[endpoint,key,vtlId]);
 async function fresh(){
  const value=await request<Status>(endpoint+"/provisioning");
  if(value.vtl_id!==vtlId)throw new Error(t("Réponse associée à une autre VTL."));
  if(alive.current){setStatus(value);onStatus?.(value);}
  return value;
 }
 async function activate(){
  if(locked.current||!status||blocked)return;locked.current=true;setError("");
  let current=status,saved=token,own=false;const done=new Set<Step>();
  try{
   for(let action=nextStep(current,saved,own);action && !done.has(action);action=nextStep(current,saved,own)){
    done.add(action);if(alive.current)setStep(action);
    const result=await request<Status & {token?:string}>(endpoint+"/"+action,{method:"POST",signal:AbortSignal.timeout(310000),headers:{"Content-Type":"application/json"},body:JSON.stringify(action==="prepare"?{revision}:action==="apply"?{token:saved}:{})});
    if(result.vtl_id!==vtlId)throw new Error(t("Réponse associée à une autre VTL."));
    if(action==="prepare" && result.token){saved=result.token;own=true;sessionStorage.setItem(key,saved);}
    if(action==="apply"){saved=null;sessionStorage.removeItem(key);}
    if(alive.current)setToken(saved);
    current=await fresh();
    window.dispatchEvent(new Event("cold-vtl-libraries-changed"));
    // PBS registration or an interrupted start needs the operator before resuming.
    if(action==="activate")break;
   }
  }catch(cause){
   if(alive.current)setError(cause instanceof Error?cause.message:t("Résultat non confirmé. Consultez à nouveau les détails avant de reprendre."));
   try{await fresh();}catch{}
  }
  finally{locked.current=false;if(alive.current)setStep(null);}
 }
 const action=status?nextStep(status,token,false):null;
 const started=!!status && (activation.includes(status.phase) || status.phase==="applying");
 return <section className="space-y-2" aria-label={t("Activation de la bibliothèque")}>
  {status?.message && <p>{t(status.message)}</p>}
  {error && <Alert variant="destructive"><AlertDescription>{t(error)}</AlertDescription></Alert>}
  {action && <Button disabled={!!step || !!blocked} onClick={()=>void activate()}>{t(started?"Reprendre l’activation":"Activer la bibliothèque")}</Button>}
  {action && blocked && !step && <p className="text-muted-foreground">{blocked}</p>}
  {status && activation.includes(status.phase) && !status.qualified && <p>{t("L’activation attend la validation de l’isolation sur cette version.")}</p>}
  {status?.phase==="applying" && !token && <p>{t("La préparation enregistrée n’est plus disponible dans ce navigateur. Reprenez depuis le navigateur qui l’a lancée.")}</p>}
  {step && <p role="status" className="text-muted-foreground">{t(labels[step])} {t("Fermer cette fenêtre ne l’annule pas.")}</p>}
  {status?.phase==="active" && <p role="status">{t("Bibliothèque activée")}</p>}
 </section>;
}
