"use client";
import { translate as t, useI18n, getLocale, isPlural } from "@/lib/i18n";

import { useCallback, useEffect, useRef, useState } from "react";
import { CircleCheck, Clock3, CircleAlert, CircleMinus, CircleHelp, Database, Videotape, Plus, Search, Server, Pencil, LoaderCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { ApiError, request } from "@/lib/api";
import { vtlHash } from "./use-vtl-selection";
import { VtlWindow, vtlTabs, type StorageTab, type VtlTab } from "./vtl-window";
import { StorageGauge } from "./storage-gauge";

type Vtl = { id: string; name: string; lifecycle: string; provisioning_revision: number; storage?: { provider: string; bucket: string; prefix: string } | null };
const states: Record<string, string> = { draft: "À configurer", ready: "Prête", failed: "À vérifier", disabled: "Désactivée" };
function LibraryState({ lifecycle }: { lifecycle: string }) {
  useI18n();
  const { icon: Icon, color } = ({
    ready: { icon: CircleCheck, color: "text-emerald-600 dark:text-emerald-400" },
    draft: { icon: Clock3, color: "text-amber-600 dark:text-amber-400" },
    failed: { icon: CircleAlert, color: "text-red-600 dark:text-red-400" },
    disabled: { icon: CircleMinus, color: "text-muted-foreground" },
  } as Record<string, { icon: typeof CircleCheck; color: string }>)[lifecycle] ?? { icon: CircleHelp, color: "text-muted-foreground" };
  const label = t(states[lifecycle] ?? "État inconnu");
  return <span role="img" aria-label={label} title={label} tabIndex={0} className={`absolute right-4 top-4 flex size-8 items-center justify-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring lg:static ${color}`}><Icon aria-hidden="true" className="size-5" /></span>;
}
function validLibrary(value: unknown): value is Vtl {
  if (!value || typeof value !== "object") return false;
  const item = value as Partial<Vtl>;
  return typeof item.id === "string" && typeof item.name === "string" && typeof item.lifecycle === "string" && typeof item.provisioning_revision === "number";
}
function message(error: unknown) {
  const text = error instanceof Error ? error.message : t("Impossible de joindre le service.");
  if (error instanceof ApiError && error.status === 404) return t("Le service connecté ne propose pas encore la gestion des bibliothèques VTL. Sa mise à jour est nécessaire pour les afficher et en créer.");
  if (text.includes("VTL provisioning is not enabled")) return t("La création n’est pas encore activée sur cette appliance. Aucune bibliothèque n’a été créée.");
  return text;
}

export function Vtls({ canManage }: { canManage: boolean }) {
  useI18n();
  const [items, setItems] = useState<Vtl[] | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const [opened, setOpened] = useState<{ id: string; tab: VtlTab; storage: StorageTab } | null>(null);
  const [renaming, setRenaming] = useState<Vtl | null>(null);
  const [renameName, setRenameName] = useState("");
  const [renameError, setRenameError] = useState("");
  const [renameBusy, setRenameBusy] = useState(false);
  const [renamed, setRenamed] = useState("");
  const renamePending = useRef(false);
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [notice, setNotice] = useState<{name: string} | null>(null);
  const savingRef = useRef(false);
  const load = useCallback(async (signal: AbortSignal) => {
    setError("");
    try {
      const result = await request<unknown>("/v1/vtls", { signal });
      if (!Array.isArray(result) || !result.every(validLibrary)) throw new Error(t("Le serveur a renvoyé une liste de bibliothèques invalide."));
      if (!signal.aborted) setItems(result);
    } catch (cause) {
      if (!signal.aborted) setError(message(cause));
    }
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    const changed=()=>void load(controller.signal);
    window.addEventListener("cold-vtl-libraries-changed",changed);
    return () => {controller.abort();window.removeEventListener("cold-vtl-libraries-changed",changed);};
  }, [load]);

  // Deep links: #vtls?vtl=<id>&tab=<setup|storage|connection|bandwidth>&storage=<local|s3>.
  // A link with only storage=… (former « Stockage » dialog) opens the Stockage tab.
  useEffect(() => {
    if (!canManage || !items) return;
    const open = () => {
      const [route, query = ""] = location.hash.slice(1).split("?");
      const params = new URLSearchParams(query);
      const tab = params.get("tab"), storage = params.get("storage");
      if (route !== "vtls" || (!tab && !storage)) return;
      const item = items.find(value => value.id === params.get("vtl"));
      if (!item) return;
      history.replaceState(null, "", vtlHash("vtls", item.id));
      setOpened({ id: item.id, tab: vtlTabs.includes(tab as VtlTab) ? tab as VtlTab : "storage", storage: storage === "s3" ? "s3" : "local" });
    };
    open(); window.addEventListener("hashchange", open);
    return () => window.removeEventListener("hashchange", open);
  }, [canManage, items]);
  const openedItem = opened ? items?.find(item => item.id === opened.id) ?? null : null;

  async function create(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (savingRef.current || !canManage || !name.trim() || error || !items) return;
    savingRef.current = true;
    setSaving(true);
    setSaveError("");
    try {
      const result = await request<unknown>("/v1/vtls", {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: name.trim() }),
      });
      if (!validLibrary(result)) throw new Error(t("Réponse de création invalide. Vérifiez la liste avant de réessayer."));
      setItems(current => [...(current ?? []).filter(item => item.id !== result.id), result]);
      setNotice({name: result.name});setRenamed("");
      setOpened({ id: result.id, tab: "setup", storage: "local" });
      window.dispatchEvent(new Event("cold-vtl-libraries-changed"));
      setCreating(false);
      setName("");
    } catch (cause) {
      setSaveError(message(cause));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }
  async function rename(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const next=renameName.trim();
    if (!renaming || renamePending.current || !canManage || !next || next===renaming.name || error) return;
    renamePending.current=true;setRenameBusy(true);setRenameError("");
    try {
      const result=await request<unknown>(`/v1/vtls/${encodeURIComponent(renaming.id)}`, {
        method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({name:next,expected_name:renaming.name}),
      });
      if (!validLibrary(result) || result.id!==renaming.id) throw new Error(t("Renommage non confirmé. Actualisez la liste avant de réessayer."));
      setItems(current=>current?.map(item=>item.id===result.id?result:item)??null);
      setRenamed(result.name);setNotice(null);setRenaming(null);
      window.dispatchEvent(new Event("cold-vtl-libraries-changed"));
    } catch(cause) {setRenameError(cause instanceof Error?cause.message:t("Renommage non confirmé. Actualisez la liste avant de réessayer."));}
    finally {renamePending.current=false;setRenameBusy(false);}
  }
  const filtered = items?.filter(item => item.name.toLocaleLowerCase(getLocale()).includes(query.toLocaleLowerCase(getLocale())));
  return <>
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div className="flex items-center gap-2 text-sm text-muted-foreground"><Videotape className="size-4" />{items ? t(isPlural(items.length) ? "{count} bibliothèques" : "{count} bibliothèque", {count: items.length}) : t("Bibliothèques de l’appliance")}</div>
      <div className="flex w-full flex-wrap items-center gap-2 sm:w-auto">
        <div className="relative min-w-40 flex-1 sm:w-64"><Search className="pointer-events-none absolute top-2.5 left-3 size-4 text-muted-foreground" /><Input aria-label={t("Rechercher une bibliothèque")} placeholder={t("Rechercher…")} className="pl-9" value={query} onChange={event => setQuery(event.target.value)} /></div>
        {canManage && <Button onClick={() => { setSaveError(""); setCreating(true); }}><Plus />{t("Créer une VTL")}</Button>}
      </div>
    </div>
    {error && <Alert variant="destructive"><AlertTitle>{t("Bibliothèques indisponibles")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
    {renamed && <p role="status" className="text-sm text-emerald-700 dark:text-emerald-300">{t("Bibliothèque renommée : {name}",{name:renamed})}</p>}
    {notice && <p role="status" className="text-sm text-muted-foreground">{t("« {name} » a été enregistrée. Sa configuration reste à terminer avant activation.", notice)}</p>}
    {!items && !error && <div role="status" aria-label={t("Chargement des bibliothèques")} className="space-y-2"><Skeleton className="h-20" /><Skeleton className="h-20" /></div>}
    {filtered?.length === 0 && <Card><CardContent className="flex flex-col items-center gap-3 py-10 text-center"><Videotape className="size-8 text-muted-foreground" /><p className="font-medium">{items?.length ? t("Aucune bibliothèque ne correspond à la recherche.") : t("Aucune bibliothèque configurée.")}</p>{!items?.length && <p className="text-sm text-muted-foreground">{t("Chaque bibliothèque dispose de ses cartouches, de son dossier local et, si besoin, d’une destination S3 dédiée.")}</p>}</CardContent></Card>}
    {!!filtered?.length && <ul aria-label={t("Bibliothèques virtuelles")} className="divide-y overflow-hidden rounded-xl border bg-card">
      {filtered.map(item => <li key={item.id} className="relative grid min-w-0 gap-4 p-4 pr-16 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto_auto_auto] lg:items-center lg:pr-4">
        <div className="flex min-w-0 items-center gap-3">
          <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted"><Videotape className="size-4" /></div>
          <h2 className="min-w-0 truncate text-sm font-semibold" title={item.name}>{item.name}</h2>
        </div>
        <div className="flex min-w-0 items-center gap-2 text-sm">
          {item.storage && <><Database className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0"><p className="truncate font-medium" title={item.storage.bucket}>{item.storage.bucket}</p><p className="text-xs text-muted-foreground">{item.storage.provider === "ovh_cold_archive_v1" ? t("OVHcloud Cold Archive v1") : t("S3 compatible")}</p></div></>}
        </div>
        <div className="flex min-w-0 items-center">{item.lifecycle !== "draft" && <StorageGauge vtlId={item.id} variant="row" />}</div>
        <div className="flex flex-wrap items-center gap-2 lg:justify-end">
          {canManage && <Button size="sm" onClick={() => setOpened({ id: item.id, tab: "setup", storage: "local" })} aria-label={t("Ouvrir {name}", { name: item.name })}>{t("Ouvrir")}</Button>}
          {canManage && <Button variant="outline" size="sm" disabled={!!error} onClick={()=>{setRenaming(item);setRenameName(item.name);setRenameError("");}} aria-label={t("Renommer {name}",{name:item.name})}><Pencil/>{t("Renommer")}</Button>}
        </div>
        <LibraryState lifecycle={item.lifecycle} />
      </li>)}
    </ul>}
    <Dialog open={creating} onOpenChange={open => { if (!savingRef.current) { setCreating(open); if (!open) { setName(""); setSaveError(""); } } }}>
      <DialogContent className="sm:max-w-lg" onInteractOutside={event => { if (saving) event.preventDefault(); }} onEscapeKeyDown={event => { if (saving) event.preventDefault(); }}>
        <DialogHeader icon={Videotape}><DialogTitle>{t("Créer une VTL")}</DialogTitle><DialogDescription>{t("Commencez par nommer la bibliothèque. Elle sera configurée avant d’être exposée au logiciel de sauvegarde.")}</DialogDescription></DialogHeader>
        <form onSubmit={event => void create(event)} className="space-y-5">
          <div className="space-y-2"><Label htmlFor="vtl-name">{t("Nom de la bibliothèque")}</Label><Input id="vtl-name" autoFocus required maxLength={100} placeholder={t("Ex. Sauvegardes production")} value={name} disabled={saving} onChange={event => setName(event.target.value)} /></div>
          <div className="rounded-lg border bg-muted/30 p-4 text-sm"><div className="flex items-center gap-2 font-medium"><Server className="size-4" />{t("1 VTL · 1 dossier local · S3 facultatif")}</div><p className="mt-2 text-muted-foreground">{t("Après la création : configurer le stockage local des bandes, enregistrer le client de sauvegarde, préparer la bibliothèque, puis activer son accès iSCSI.")}</p></div>
          {(error || saveError) && <Alert variant="destructive"><AlertDescription>{t(saveError || error)}</AlertDescription></Alert>}
          {!canManage && <p className="text-sm text-muted-foreground">{t("La création est réservée aux administrateurs.")}</p>}
          <div className="flex justify-end gap-2"><Button type="button" variant="outline" disabled={saving} onClick={() => { setCreating(false); setName(""); }}>{t("Annuler")}</Button><Button type="submit" disabled={saving || !name.trim() || !!error || !items || !canManage}>{saving ? t("Création…") : t("Créer la bibliothèque")}</Button></div>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={!!renaming} onOpenChange={open=>{if(!open&&!renamePending.current)setRenaming(null);}}>
      <DialogContent className="sm:max-w-lg" onInteractOutside={event=>{if(renameBusy)event.preventDefault();}} onEscapeKeyDown={event=>{if(renameBusy)event.preventDefault();}}>
        <DialogHeader icon={Pencil}><DialogTitle>{t("Renommer la VTL")}</DialogTitle><DialogDescription>{t("Choisissez le nom affiché pour cette bibliothèque.")}</DialogDescription></DialogHeader>
        <form onSubmit={event=>void rename(event)} className="space-y-5">
          <div className="space-y-2"><Label htmlFor="vtl-rename">{t("Nom de la bibliothèque")}</Label><Input id="vtl-rename" autoFocus required maxLength={100} value={renameName} disabled={renameBusy} onChange={event=>setRenameName(event.target.value)}/></div>
          {(renameError||error)&&<Alert variant="destructive"><AlertDescription>{t(renameError||error)}</AlertDescription></Alert>}
          <div className="flex justify-end gap-2"><Button type="button" variant="outline" disabled={renameBusy} onClick={()=>setRenaming(null)}>{t("Annuler")}</Button><Button type="submit" disabled={renameBusy||!renameName.trim()||renameName.trim()===renaming?.name||!!error||!canManage}>{renameBusy&&<LoaderCircle className="motion-safe:animate-spin"/>}{t(renameBusy?"Enregistrement…":"Enregistrer")}</Button></div>
        </form>
      </DialogContent>
    </Dialog>
    {opened && openedItem && <VtlWindow key={openedItem.id} vtl={openedItem} tab={opened.tab} storage={opened.storage} canManage={canManage} connected={!error}
      onTab={tab => setOpened(current => current && { ...current, tab })}
      onStorage={storage => setOpened(current => current && { ...current, storage })}
      onClose={() => setOpened(null)}
      onRefresh={async () => { await load(AbortSignal.timeout(15000)); window.dispatchEvent(new Event("cold-vtl-libraries-changed")); }} />}
  </>;
}
