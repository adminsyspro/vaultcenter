"use client";
import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronLeft, ChevronRight, RefreshCw, LoaderCircle, Search, Trash2, CloudDownload } from "lucide-react";
import { translate as t, isPlural, useI18n } from "@/lib/i18n";
import { ApiError, type Job, formatBytes, jobSummary } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { StorageAccessStatus, StorageBucketProblem, StorageCopyStatus, accessStatus, copyStatus } from "./storage-copy-status";
import { Badge } from "@/components/ui/badge";
import { StorageDeleteDialog } from "./storage-delete-dialog";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import { useScopedRequest } from "./vtl-scope";
import { type StorageBucket, bucketPath } from "./bucket-selector";
import { StorageCopyDialog } from "./storage-copy-dialog";
import { StorageRecoveryDialog, type RecoveryCopy } from "./storage-recovery-dialog";

type Inventory = {destination_id: string; bucket: string; configuration_revision?: number | null; copies: Omit<RecoveryCopy, "destination_id" | "bucket" | "readable">[]; errors: {key: string; error: string}[]; complete: boolean; access: {readable:boolean; writable:boolean; raw_status:string | null; supported:boolean}};
const PAGE = 10;
type BucketResult = {bucket: StorageBucket; inventory: Inventory | null; error: string | null; gone: boolean};
export function StorageRecovery({canManage, connected, jobs, onRefresh, onBucket}: {canManage:boolean; connected:boolean; jobs:Job[]; onRefresh:()=>Promise<void>; onBucket:(id:string,name:string,createdAt?:string)=>void}) {
  useI18n();
  const request = useScopedRequest();
  const [results,setResults] = useState<BucketResult[]>([]);
  const [busy,setBusy] = useState(true);
  const [error,setError] = useState("");
  // Per bucket: folded by the user (or by default once archived), and the page of its copies.
  const [folded,setFolded] = useState<Record<string,boolean>>({});
  const [pages,setPages] = useState<Record<string,number>>({});
  const [search,setSearch] = useState("");
  const [details,setDetails] = useState<RecoveryCopy | null>(null);
  const [deleting,setDeleting] = useState<RecoveryCopy | null>(null);
  const [selected,setSelected] = useState<RecoveryCopy | null>(null);
  const [started,setStarted] = useState<Job | null>(null);
  const [retiring,setRetiring] = useState<{id:string;busy:boolean;error:string} | null>(null);
  const sequence = useRef(0);
  const refresh = useCallback(async (signal?:AbortSignal) => {
    const current = ++sequence.current;
    setBusy(true); setError("");
    try {
      const buckets = await request<StorageBucket[]>("/v1/storage/buckets", {signal});
      if (!Array.isArray(buckets)) throw new Error(t("Liste des buckets invalide."));
      const output: BucketResult[] = [];
      // Limit simultaneous remote inventories to avoid overwhelming the S3 connection.
      for (let offset=0;offset<buckets.length;offset+=3) {
        if (signal?.aborted || current !== sequence.current) return;
        const batch = await Promise.all(buckets.slice(offset,offset+3).map(async bucket => {
          try {
            const inventory = await request<Inventory>(bucketPath("/v1/storage/recovery",bucket.id), {signal:AbortSignal.any([...(signal ? [signal] : []),AbortSignal.timeout(45000)])});
            if (!Array.isArray(inventory.copies) || !inventory.access) throw new Error(t("Inventaire S3 invalide."));
            return {bucket,inventory,error:null,gone:false};
          } catch (cause) {return {bucket,inventory:null,error:cause instanceof Error ? cause.message : t("Lecture impossible."),gone:cause instanceof ApiError && cause.status===410};}
        }));
        output.push(...batch);
      }
      if (!signal?.aborted && current === sequence.current) {
        setResults(output);
        // An archived bucket starts folded when another one holds copies; a bucket already shown keeps its state.
        setFolded(previous=>{const next={...previous};for(const value of output)if(!(value.bucket.id in next))next[value.bucket.id]=value.inventory?.access.raw_status==="Archived"&&output.some(other=>other!==value&&!!other.inventory?.copies.length);return next;});
      }
    } catch (cause) {if (!signal?.aborted && current === sequence.current) {setResults([]);setError(cause instanceof Error ? cause.message : t("Inventaire indisponible."));}}
    finally {if (!signal?.aborted && current === sequence.current) setBusy(false);}
  },[request]);
  const revision=jobs.filter(job => /^(export|recovery|s3_delete|bucket_create|recall_|archive_)/.test(job.kind)).map(job => `${job.id}:${job.status}`).join(",");
  useEffect(() => {const controller=new AbortController();void refresh(controller.signal);return () => controller.abort();},[refresh,revision]);
  const transitioning=results.some(value=>["Archiving","Restoring","Deleting"].includes(value.inventory?.access.raw_status??""));
  useEffect(()=>{if(!transitioning||!connected)return;const timer=setInterval(()=>{if(!document.hidden)void refresh();},30000);return()=>clearInterval(timer);},[transitioning,connected,refresh]);
  const visible=results;
  const copies:RecoveryCopy[]=visible.flatMap(value=>(value.inventory?.copies??[]).map(copy=>({...copy,destination_id:value.bucket.id,bucket:value.bucket.bucket,...value.inventory!.access,configuration_revision:value.inventory!.configuration_revision,deletion_status:jobs.find(job=>job.kind==="s3_delete"&&job.destination_id===value.bucket.id&&job.subject===copy.barcode&&job.generation===copy.generation)?.status??copy.deletion_status})));
  const withDeletion=(value:BucketResult,copy:Inventory["copies"][number])=>jobs.find(job=>job.kind==="s3_delete"&&job.destination_id===value.bucket.id&&job.subject===copy.barcode&&job.generation===copy.generation)?.status??copy.deletion_status;
  // Only copies that could still be restored are offered as reassurance before a deletion.
  const usableCopies:RecoveryCopy[]=results.flatMap(value=>(value.inventory?.copies??[]).map(copy=>({...copy,destination_id:value.bucket.id,bucket:value.bucket.bucket,...value.inventory!.access,configuration_revision:value.inventory!.configuration_revision,deletion_status:withDeletion(value,copy)}))).filter(copy=>copy.readable&&!!copy.manifest_sha256&&!["Archived","Archiving","Deleting"].includes(copy.raw_status??"")&&!["queued","running","failed"].includes(copy.deletion_status??""));
  const query=search.trim().toLowerCase();
  const matches=(copy:RecoveryCopy)=>`${copy.barcode} ${copy.bucket} ${copy.generation}`.toLowerCase().includes(query);
  const rows=copies.filter(matches);
  // While searching, only the buckets holding a match remain.
  const shown=query ? results.filter(value=>rows.some(copy=>copy.destination_id===value.bucket.id)) : results;
  // A bucket OVH no longer knows holds nothing: it does not make the inventory partial.
  const incomplete=visible.some(value=>!value.gone && (value.error || !value.inventory?.complete));
  const statusJob=started ? jobs.find(job=>job.id===started.id) ?? started : null;
  const currentCopy=(copy:RecoveryCopy)=>copies.find(value=>value.destination_id===copy.destination_id&&value.barcode===copy.barcode&&value.generation===copy.generation)??copy;
  const pendingDelete=(copy:RecoveryCopy)=>["queued","running"].includes(copy.deletion_status??"");
  const blocked=jobs.some(job=>["queued","running"].includes(job.status));
  async function retire(bucket:StorageBucket){
    setRetiring({id:bucket.id,busy:true,error:""});
    try {
      await request(`/v1/storage/buckets/${encodeURIComponent(bucket.id)}/retire`,{method:"POST"});
      setRetiring(null);
      await refresh();
    } catch (cause) {setRetiring({id:bucket.id,busy:false,error:cause instanceof Error ? cause.message : t("Le bucket n’a pas été retiré.")});}
  }
  function deletionStarted(job:Job){setStarted(job);setDeleting(null);void onRefresh();}
  return <section aria-label={t("Copies S3")} className="space-y-5">
    <div className="flex flex-wrap justify-end gap-2"><Button variant="outline" disabled={busy || !connected} onClick={()=>void refresh()}>{busy ? <LoaderCircle className="motion-safe:animate-spin"/> : <RefreshCw/>}{t("Actualiser le stockage")}</Button></div>
    {!connected && <Alert><AlertTitle>{t("Connexion interrompue")}</AlertTitle><AlertDescription>{t("Les opérations seront disponibles après reconnexion au serveur.")}</AlertDescription></Alert>}
    {error && <Alert variant="destructive"><AlertTitle>{t("Inventaire indisponible")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
    {statusJob && <Alert role="status" variant={statusJob.status==="failed"?"destructive":"default"}><AlertTitle>{t(statusJob.kind==="s3_delete" ? statusJob.status==="completed"?"Copie S3 supprimée":statusJob.status==="failed"?"Suppression interrompue":"Suppression en cours" : statusJob.status === "completed" ? "Réintégration terminée" : statusJob.status === "failed" ? "Réintégration interrompue" : "Réintégration en cours")}</AlertTitle><AlertDescription>{statusJob.subject} · {jobSummary(statusJob)}</AlertDescription></Alert>}
    <div className="space-y-2"><Label htmlFor="storage-search">{t("Rechercher une cartouche")}</Label><div className="relative"><Search className="pointer-events-none absolute top-2.5 left-3 size-4 text-muted-foreground" aria-hidden="true"/><Input id="storage-search" className="bg-card pl-9 dark:bg-card" value={search} onChange={e=>setSearch(e.target.value)} placeholder={t("Code-barres, bucket ou génération")}/></div></div>
    {busy && <p role="status" className="text-sm text-muted-foreground">{t("Lecture des copies dans les buckets…")}</p>}
    {incomplete && !busy && <Alert><AlertTitle>{t("Inventaire partiel")}</AlertTitle><AlertDescription>{t("Les résultats ci-dessous ne représentent pas toutes les copies. Les erreurs de lecture ne signifient pas que les buckets sont vides.")}</AlertDescription></Alert>}
    {shown.length>0 && <ul aria-label={t("Buckets")} aria-busy={busy} className="space-y-4">{shown.map(({bucket,inventory,error:problem,gone})=>{
      const confirming=retiring?.id===bucket.id;
      const details=problem ?? (inventory?.errors.length ? inventory.errors.map(value=>`${value.key} : ${value.error}`).join("\n") : null);
      const all=copies.filter(copy=>copy.destination_id===bucket.id),found=all.filter(matches);
      const open=!!query || !folded[bucket.id];
      const last=Math.max(0,Math.ceil(found.length/PAGE)-1),page=Math.min(pages[bucket.id]??0,last);
      const bucketLabel=inventory ? t(accessStatus(inventory.access).label) : null;
      const size=all.reduce((sum,copy)=>sum+(copy.logical_size_bytes??0),0);
      const bodyId=`storage-bucket-${bucket.id}`;
      return <li key={bucket.id} className="vc-surface text-sm">
       <div className="vc-surface-heading flex flex-wrap items-center justify-between gap-x-4 gap-y-2 px-4 py-3">
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2"><button type="button" className="flex min-w-0 items-center gap-2 rounded-sm text-left font-mono font-medium focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring" aria-expanded={open} aria-controls={bodyId} aria-label={bucket.bucket} disabled={!!query} onClick={()=>setFolded(value=>({...value,[bucket.id]:!value[bucket.id]}))}><ChevronDown aria-hidden="true" className={`size-4 shrink-0 text-muted-foreground transition-transform motion-reduce:transition-none ${open?"":"-rotate-90"}`}/><span className="break-all">{bucket.bucket}</span></button><Badge variant={bucket.active?"secondary":"outline"}>{t(bucket.active?"Actif":"Historique")}</Badge>{problem ? <StorageBucketProblem problem={gone?"gone":"unreadable"}/> : <StorageAccessStatus access={inventory?.access ?? null}/>}{!!inventory?.errors.length && <StorageBucketProblem problem="manifests"/>}{inventory && <span className="text-xs text-muted-foreground tabular-nums">{all.length ? `${t(isPlural(all.length)?"{count} copies":"{count} copie",{count:all.length})} · ${formatBytes(size)}` : t("Aucune copie")}</span>}</div>
        <div className="flex flex-wrap gap-2">{canManage && gone && !bucket.active && !confirming && <Button size="sm" variant="outline" disabled={!connected} onClick={()=>setRetiring({id:bucket.id,busy:false,error:""})}>{t("Retirer")}</Button>}<Button size="sm" variant="outline" aria-label={t("Gérer le bucket {bucket}",{bucket:bucket.bucket})} onClick={()=>onBucket(bucket.id,bucket.bucket,bucket.created_at)}>{t("Gérer le bucket")}</Button></div></div>
       {confirming && <div className="mx-4 my-3 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2"><p>{t("Retirer {bucket} du catalogue ? Aucune copie ni historique n’est supprimé.",{bucket:bucket.bucket})}</p><div className="flex gap-2"><Button size="sm" variant="ghost" disabled={retiring.busy} onClick={()=>setRetiring(null)}>{t("Annuler")}</Button><Button size="sm" variant="destructive" disabled={retiring.busy || !connected} aria-label={t("Retirer {bucket}",{bucket:bucket.bucket})} onClick={()=>void retire(bucket)}>{retiring.busy && <LoaderCircle className="motion-safe:animate-spin"/>}{t("Retirer")}</Button></div>{retiring.error && <p role="alert" className="w-full text-destructive">{t(retiring.error)}</p>}</div>}
       {details && <details className="px-4 py-2 text-muted-foreground"><summary className="w-fit cursor-pointer select-none text-xs font-medium">{t("Détails")}</summary><p className="mt-1 whitespace-pre-wrap break-words font-mono text-xs">{t(details)}</p></details>}
       <div id={bodyId} hidden={!open}>{found.length>0 && <div className="overflow-x-auto"><table className="w-full min-w-[560px] text-left text-sm"><caption className="sr-only">{t("Copies S3 de {bucket}",{bucket:bucket.bucket})}</caption><thead className="border-b text-xs text-muted-foreground"><tr>{["Code-barres","Génération","Volume"].map(label=><th key={label} scope="col" className="px-4 py-2 font-medium">{t(label)}</th>)}<th scope="col" className="px-4 py-2 text-right font-medium">{t("Action")}</th></tr></thead><tbody>{found.slice(page*PAGE,page*PAGE+PAGE).map(copy=>{
         // The block states its bucket's access once; a copy shows a state only when its own differs.
         const own=copyStatus(copy).label;
         return <tr key={`${copy.destination_id}:${copy.barcode}:${copy.generation}`} tabIndex={0} aria-haspopup="dialog" className="cursor-pointer border-b last:border-0 hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring" onClick={()=>setDetails(copy)} onKeyDown={event=>{if(event.target===event.currentTarget&&(event.key==='Enter'||event.key===' ')){event.preventDefault();setDetails(copy);}}}><th scope="row" className="px-4 py-2 font-medium">{copy.barcode}</th><td className="px-4 py-2 tabular-nums">{copy.generation}</td><td className="px-4 py-2 tabular-nums">{copy.logical_size_bytes == null ? t("Non confirmé") : formatBytes(copy.logical_size_bytes)}</td><td className="px-4 py-2"><div className="flex items-center justify-end gap-2">{t(own)!==bucketLabel && <StorageCopyStatus copy={copy}/>}{copy.readable && copy.manifest_sha256 ? <Button size="sm" variant="outline" disabled={!canManage || !connected || busy || pendingDelete(copy) || copy.deletion_status==="failed"} onClick={event=>{event.stopPropagation();setSelected(copy);}}>{t("Réintégrer")}</Button> : <Button size="sm" variant="outline" onClick={event=>{event.stopPropagation();onBucket(copy.destination_id,copy.bucket);}}>{copy.raw_status==="Archived"&&<CloudDownload/>}{t(copy.raw_status==="Archived"?"Rappeler le bucket":"Voir le bucket")}</Button>}{canManage&&<span title={t(!copy.writable?"La suppression est indisponible, car le bucket est en lecture seule ou son accès n’est pas confirmé.":"Supprimer uniquement cette copie S3")}><Button size="icon-sm" variant="ghost" className="text-muted-foreground hover:text-destructive" aria-label={t("Supprimer la copie S3")} disabled={!connected||busy||blocked||!copy.writable||!copy.readable||!copy.manifest_sha256} onClick={event=>{event.stopPropagation();setDeleting(copy);}}><Trash2 aria-hidden="true"/></Button></span>}</div></td></tr>;})}</tbody></table></div>}
        {last>0 && <div className="flex items-center justify-end gap-2 border-t px-4 py-2 text-xs"><Button variant="outline" size="icon-sm" aria-label={t("Page précédente des copies de {bucket}",{bucket:bucket.bucket})} disabled={!page} onClick={()=>setPages(value=>({...value,[bucket.id]:page-1}))}><ChevronLeft/></Button><span>{t("Page {page} sur {total}",{page:page+1,total:last+1})}</span><Button variant="outline" size="icon-sm" aria-label={t("Page suivante des copies de {bucket}",{bucket:bucket.bucket})} disabled={page===last} onClick={()=>setPages(value=>({...value,[bucket.id]:page+1}))}><ChevronRight/></Button></div>}
       </div>
      </li>;})}</ul>}
    {!rows.length && !busy && <p className="vc-surface px-5 py-10 text-center text-sm text-muted-foreground">{t(error || incomplete ? "Aucune copie confirmée dans les résultats disponibles." : query ? "Aucune copie ne correspond aux filtres." : "Aucune copie S3 repérée. Rattachez un ancien bucket ou envoyez des cartouches vers S3.")}</p>}
    {details && <StorageCopyDialog key={`${details.destination_id}:${details.barcode}:${details.generation}`} copy={currentCopy(details)} canRestore={canManage && connected && !busy && !pendingDelete(currentCopy(details)) && currentCopy(details).deletion_status!=="failed"} canDelete={canManage && connected && !busy && !blocked} onDelete={canManage?()=>{setDeleting(currentCopy(details));setDetails(null);}:undefined} onClose={()=>setDetails(null)} onRestore={()=>{setSelected(currentCopy(details));setDetails(null);}} onBucket={()=>{onBucket(details.destination_id,details.bucket);setDetails(null);}}/>}
    {deleting && <StorageDeleteDialog copy={currentCopy(deleting)} others={usableCopies.filter(copy=>copy.barcode===deleting.barcode&&!(copy.destination_id===deleting.destination_id&&copy.generation===deleting.generation))} available={canManage&&connected&&!busy&&!blocked} onClose={()=>setDeleting(null)} onStarted={deletionStarted}/>}
    {selected && <StorageRecoveryDialog key={`${selected.destination_id}:${selected.barcode}:${selected.generation}`} copy={selected} canManage={canManage} connected={connected} onClose={()=>setSelected(null)} onStarted={job=>{setStarted(job);setSelected(null);void onRefresh();}}/>}
  </section>;
}
