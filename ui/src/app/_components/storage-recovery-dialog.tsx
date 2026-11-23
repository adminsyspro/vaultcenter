"use client";
import { CloudDownload, CircleCheck, CircleAlert, LoaderCircle } from "lucide-react";
import { useContext, useEffect, useRef, useState } from "react";
import { translate as t, useI18n } from "@/lib/i18n";
import { request as apiRequest, formatBytes, type Job } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { Select, SelectOption } from "@/components/ui/select";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { useScopedRequest, VtlScope } from "./vtl-scope";
import type { Library } from "./use-vtl-selection";
export type RecoveryCopy = {writable?: boolean; raw_status?: string | null; supported?: boolean; configuration_revision?: number | null; deletion_status?: string | null; destination_id: string; bucket: string; barcode: string; generation: number; manifest_sha256: string | null; logical_size_bytes: number | null; readable: boolean; manifest_key?: string | null; file_count?: number | null; segment_count?: number | null; format_version?: number | null};
type Check = {can_restore: boolean; conflict: boolean; local_identity: "absent" | "conflict"; space_ok: boolean; free_slots: number; required_bytes: number; available_bytes: number; installation_required_bytes?: number; download_required_bytes?: number; download_available_bytes?: number | null; shared_volume?: boolean; reason: string | null};
export function StorageRecoveryDialog({copy, connected, canManage, onClose, onStarted}: {copy: RecoveryCopy; connected: boolean; canManage: boolean; onClose: () => void; onStarted: (job: Job) => void}) {
  useI18n();
  const request = useScopedRequest();
  const source = useContext(VtlScope);
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [target, setTarget] = useState(source ?? "");
  const [check, setCheck] = useState<Check | null>(null), [checking, setChecking] = useState(false);
  const [error, setError] = useState(""), [busy, setBusy] = useState(false), [accepted, setAccepted] = useState(false);
  const pending = useRef(false);
  useEffect(() => {
    const controller = new AbortController();
    void apiRequest<Library[]>("/v1/vtls", {signal: controller.signal}).then(values => { if (!controller.signal.aborted) setLibraries(values.filter(value => value.lifecycle === "ready")); }).catch(cause => { if (!controller.signal.aborted) setError(cause.message); });
    return () => controller.abort();
  }, []);
  const allowed = canManage && connected && copy.readable && !!copy.manifest_sha256 && libraries.some(value => value.id === target);
  const payload = (replace: boolean) => ({destination_id: copy.destination_id, barcode: copy.barcode, generation: copy.generation, manifest_sha256: copy.manifest_sha256, target_vtl_id: target, replace});
  // The target library is checked on opening and at every change.
  useEffect(() => {
    if (!allowed) { setCheck(null); setChecking(false); setError(""); return; }
    const controller = new AbortController();
    setChecking(true); setCheck(null); setError(""); setAccepted(false);
    void request<Check>("/v1/storage/recovery/check", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(payload(false)), signal: AbortSignal.any([controller.signal, AbortSignal.timeout(45000)])})
      .then(value => { if (!controller.signal.aborted) setCheck(value); })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Contrôle impossible.")); })
      .finally(() => { if (!controller.signal.aborted) setChecking(false); });
    return () => controller.abort();
  // eslint-disable-next-line react-hooks/exhaustive-deps -- payload derives from target and copy
  }, [allowed, target, copy, request]);
  const conflict = !!check?.conflict;
  const ready = conflict ? !!check?.space_ok && accepted : !!check?.can_restore;
  async function submit() {
    if (pending.current || busy || !allowed || !check || !ready) return;
    pending.current = true; setBusy(true); setError("");
    try { onStarted(await request<Job>("/v1/storage/recovery", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(payload(conflict)), signal: AbortSignal.timeout(45000)})); }
    catch (cause) { setError(cause instanceof Error ? cause.message : t("Réintégration non confirmée.")); }
    finally { pending.current = false; setBusy(false); }
  }
  const targetName = libraries.find(value => value.id === target)?.name ?? "";
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}><DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
    <DialogHeader icon={CloudDownload}><DialogTitle>{t("Réintégrer {barcode}", {barcode: copy.barcode})}</DialogTitle><DialogDescription>{t("Dans {bucket} · génération {generation} · {volume}", {bucket: copy.bucket, generation: copy.generation, volume: copy.logical_size_bytes == null ? t("Volume non confirmé") : formatBytes(copy.logical_size_bytes)})}</DialogDescription></DialogHeader>
    <div className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-4"><Label htmlFor="recovery-target">{t("Bibliothèque cible")}</Label><Select className="w-full min-w-0" id="recovery-target" value={target} disabled={busy || !connected} onValueChange={value => setTarget(value)}><SelectOption value="">{t("Choisir une bibliothèque")}</SelectOption>{libraries.map(library => <SelectOption key={library.id} value={library.id}>{library.name}</SelectOption>)}</Select></div>
    {checking && <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle aria-hidden="true" className="size-4 motion-safe:animate-spin" />{t("Vérification de la bibliothèque cible…")}</p>}
    {error && <Alert variant="destructive" className="border-destructive/40 bg-destructive/5"><CircleAlert aria-hidden="true" /><AlertTitle>{t(error)}</AlertTitle></Alert>}
    {check && (conflict
      ? <div role="alert" className="space-y-3 rounded-lg border border-amber-300 bg-amber-50 p-4 text-sm text-amber-900 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200">
          <p>{t("{code} existe déjà dans {vtl}. La cartouche actuelle sera sauvegardée, puis remplacée. Vous pourrez revenir en arrière depuis la maintenance.", {code: copy.barcode, vtl: targetName})}</p>
          <label className="flex items-start gap-2"><Checkbox className="mt-0.5" checked={accepted} disabled={busy || !check.space_ok} onCheckedChange={value => setAccepted(value === true)} />{t("Je confirme le remplacement de {code}.", {code: copy.barcode})}</label>
        </div>
      : <Alert role={check.can_restore ? "status" : "alert"} className={check.can_restore ? "border-emerald-300 bg-emerald-50 text-emerald-800 dark:border-emerald-500/40 dark:bg-emerald-500/10 dark:text-emerald-300" : "border-destructive/40 bg-destructive/5 text-destructive"}>{check.can_restore ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}<AlertTitle>{t(check.can_restore ? "Contrôles réussis" : "Réintégration indisponible")}</AlertTitle><AlertDescription className="text-current">{check.reason ? t(check.reason) : t("Le code-barres est libre dans la bibliothèque cible.")}</AlertDescription></Alert>)}
    {check && <div className="space-y-2 text-sm">
      {check.download_available_bytes != null && !check.shared_volume && <div><p className="font-medium">{t("Téléchargement temporaire")}</p><p className={check.download_available_bytes < (check.download_required_bytes ?? check.required_bytes) ? "text-destructive" : "text-muted-foreground"}>{t("Espace disponible : {available} · nécessaire : {required}", {available: formatBytes(check.download_available_bytes), required: formatBytes(check.download_required_bytes ?? check.required_bytes)})}</p></div>}
      <div><p className="font-medium">{t(check.shared_volume ? "Téléchargement et installation sur le même volume" : "Installation dans la bibliothèque")}</p><p className={check.available_bytes < (check.installation_required_bytes ?? check.required_bytes) ? "text-destructive" : "text-muted-foreground"}>{t("Espace disponible : {available} · nécessaire : {required}", {available: formatBytes(check.available_bytes), required: formatBytes(check.installation_required_bytes ?? check.required_bytes)})}</p></div>
    </div>}
    <DialogFooter>
      <Button variant="outline" disabled={busy} onClick={onClose}>{t("Fermer")}</Button>
      <Button variant={conflict ? "destructive" : "default"} disabled={busy || checking || !ready} onClick={() => void submit()}>{busy ? <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" /> : <CloudDownload aria-hidden="true" />}{t(conflict ? "Remplacer" : "Réintégrer")}</Button>
    </DialogFooter>
  </DialogContent></Dialog>;
}
