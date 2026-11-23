"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useCallback, useEffect, useRef, useState } from "react";
import { CircleCheck, RefreshCw } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { request } from "@/lib/api";
import { type CheckedPath, type StorageApplication, type TapeStorageStatus, space, storageStatus } from "@/lib/tape-storage";


const labels={unconfigured:"Stockage à configurer",ready:"Stockage disponible",applying:"Application en cours",unavailable:"Stockage indisponible",recovery_required:"Récupération nécessaire"};
function message(cause:unknown){return cause instanceof Error?cause.message:t("Impossible de joindre le service de stockage.");}
export function TapeStorage({vtlId}:{vtlId:string}){
  useI18n();
  const endpoint=`/v1/vtls/${encodeURIComponent(vtlId)}/settings/tape-storage`;
  const pendingKey=`vaultcenter-storage-application:${vtlId}`;
  const [status,setStatus]=useState<TapeStorageStatus|null>(null);
  const [path,setPath]=useState("");
  const [checked,setChecked]=useState<CheckedPath|null>(null);
  const [busy,setBusy]=useState<"check"|"apply"|null>(null);
  const [error,setError]=useState("");
  const [uncertain,setUncertain]=useState(false);
  const [pending,setPending]=useState<StorageApplication|null>(null);
  const dirty=useRef(false);const locked=useRef(false);const alive=useRef(true);
  const load=useCallback(async()=>{
    try{
      const value=storageStatus(await request<unknown>(endpoint),vtlId);
      if(!alive.current)return;
      setStatus(value);setError("");
      if((!dirty.current || value.data_locked) && value.path)setPath(value.path);
      if(!value.change_allowed)setChecked(null);
      if(value.phase==="ready" || value.phase==="recovery_required"){
        setUncertain(false);setPending(null);sessionStorage.removeItem(pendingKey);
      }
    }catch(cause){if(alive.current)setError(message(cause));}
  },[endpoint,pendingKey,vtlId]);
  useEffect(()=>{
    alive.current=true;
    try{const saved=JSON.parse(sessionStorage.getItem(pendingKey)||"null") as StorageApplication|null;if(saved?.request_id && saved?.path){setPending(saved);setPath(saved.path);dirty.current=true;setUncertain(true);}}catch{sessionStorage.removeItem(pendingKey);}
    void load();return()=>{alive.current=false;};
  },[load,pendingKey]);
  useEffect(()=>{
    if(status?.phase!=="applying" && !uncertain)return;
    let count=0;const timer=setInterval(()=>{if(++count<=30)void load();else clearInterval(timer);},3000);
    return()=>clearInterval(timer);
  },[status?.phase,uncertain,load]);
  const applying=busy==="apply" || status?.phase==="applying";
  const editable=!!status && status.mode==="managed" && status.change_allowed && !applying && !busy && !uncertain && status.phase!=="recovery_required";
  async function verify(){
    if(!editable || locked.current)return;
    locked.current=true;setBusy("check");setChecked(null);setError("");
    try{
      const value=await request<CheckedPath>(endpoint+"/check",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({path})});
      if(value.vtl_id!==vtlId || typeof value.check_id!=="string" || !value.path || !Number.isFinite(value.available_bytes))throw new Error(t("Vérification du chemin invalide."));
      if(alive.current){setChecked(value);setPath(value.path);}
    }catch(cause){if(alive.current)setError(message(cause));}
    finally{locked.current=false;if(alive.current)setBusy(null);}
  }
  async function apply(retry=false){
    if(locked.current || !status || !status.change_allowed || (!retry && (!checked || checked.path!==path)))return;
    const body=retry?pending:{path,revision:status.revision,check_id:checked!.check_id,request_id:Array.from(crypto.getRandomValues(new Uint8Array(16)),byte=>byte.toString(16).padStart(2,"0")).join("")};
    if(!body)return;
    locked.current=true;setBusy("apply");setError("");setPending(body);sessionStorage.setItem(pendingKey,JSON.stringify(body));
    try{
      const value=storageStatus(await request<unknown>(endpoint+"/apply",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)}),vtlId);
      if(alive.current){setStatus(value);setUncertain(false);setChecked(null);}
    }catch(cause){
      if(alive.current){setError(message(cause));setUncertain(true);setChecked(null);await load();}
      // A lost response does not tell us whether the host accepted the request.
      // Keep its ID and require explicit retry or a status refresh.
    }finally{locked.current=false;if(alive.current)setBusy(null);}
  }
  async function refresh(){
    await load();
    // A returned non-applying status can release a rejected initial request.
    // Its original ID remains available until a subsequent check replaces it.
    if(!locked.current){setUncertain(false);}
  }
  return <div className="max-w-3xl space-y-8">
    <section className="space-y-4" aria-label={t("État du stockage des bandes")}>
      <div className="flex flex-wrap items-center justify-between gap-4">
        <h2 className="text-lg font-semibold">{status?t(labels[status.phase]):t("Lecture du stockage…")}</h2>
        <Button variant="outline" size="sm" onClick={()=>void refresh()} disabled={!!busy}><RefreshCw className="size-4"/>{t("Vérifier le stockage")}</Button>
      </div>
      {status?.path && <dl className="space-y-2 text-sm"><dt className="text-muted-foreground">{t("Dossier actuellement utilisé sur la VM")}</dt><dd className="break-all font-mono">{status.path}</dd>{status.capacity && <dd className="tabular-nums">{t("{available} disponibles sur {total}", {available: space(status.capacity.available_bytes), total: space(status.capacity.total_bytes)})}</dd>}</dl>}
      <p className="max-w-prose text-sm text-muted-foreground">{t("Ce dossier de la VM VaultCenter est réservé aux fichiers des cartouches de cette bibliothèque.")}</p>
      {status?.message && <p className="text-sm">{t(status.message)}</p>}
      {status?.phase==="applying" && <p role="status" className="text-sm">{t("Le dossier est en cours d’application. Vous pouvez fermer cette fenêtre et retrouver le suivi en la rouvrant.")}</p>}
      {status?.phase==="recovery_required" && <p className="text-sm">{t("Les écritures restent bloquées. Un administrateur doit vérifier l’opération sur la VM avant de reprendre les services.")}</p>}
    </section>
    <form className="space-y-5 border-t pt-6" onSubmit={event=>{event.preventDefault();void apply();}}>
      <div className="space-y-2">
        <Label htmlFor="tape-storage-path">{t("Chemin de stockage des bandes")}</Label>
        <Input id="tape-storage-path" value={path} placeholder="/mnt/bandes/vaultcenter" autoComplete="off" spellCheck={false} readOnly={!editable} disabled={!status} aria-describedby="storage-path-help" onChange={event=>{dirty.current=true;setPath(event.target.value);setChecked(null);setPending(null);sessionStorage.removeItem(pendingKey);setError("");}} />
        <p id="storage-path-help" className="text-sm text-muted-foreground">{status?.data_locked ? t("Le chemin reste verrouillé après suppression ou archivage des cartouches.") : t("Un chemin absolu vers un dossier vide sur la VM. S’il n’existe pas, le dernier dossier est créé à la vérification ; son dossier parent doit exister. Montez le volume avant de vérifier le chemin.")}</p>
      </div>
      {status?.data_locked && <p role="status" className="text-sm font-medium">{t("Chemin verrouillé : cette VTL contient ou a contenu des données. Leur déplacement nécessite une migration dédiée.")}</p>}
      {checked && <div role="status" className="space-y-2 rounded-lg border p-4 text-sm"><p className="flex items-center gap-2 font-medium"><CircleCheck className="size-4 text-emerald-700 dark:text-emerald-400"/>{t("Dossier accessible en écriture")}</p><p className="break-all font-mono">{checked.path}</p><p className="tabular-nums">{checked.filesystem} · {t("{available} disponibles sur {total}", {available: space(checked.available_bytes), total: space(checked.total_bytes)})}</p></div>}
      {error && <Alert variant="destructive"><AlertTitle>{t("Action non effectuée ou non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
      {uncertain && <p role="status" className="text-sm">{t("Le résultat de l’enregistrement n’est pas confirmé. Actualisez l’état avant de modifier le chemin.")}</p>}
      <div className="flex flex-wrap gap-3">
        <Button type="button" variant="outline" disabled={!editable || !path.startsWith("/")} onClick={()=>void verify()}>{busy==="check"?t("Vérification…"):t("Vérifier le chemin")}</Button>
        <Button type="submit" disabled={!editable || !checked || checked.path!==path}>{t("Enregistrer")}</Button>
        {uncertain && pending && status?.change_allowed && status.phase!=="applying" && <Button type="button" variant="outline" disabled={!!busy} onClick={()=>void apply(true)}>{t("Retenter l’enregistrement")}</Button>}
      </div>
      {!status?.data_locked && <p className="text-sm text-muted-foreground">{t("Après configuration du chemin, activez ou reprenez explicitement les services de cette bibliothèque.")}</p>}
    </form>
  </div>;
}
