"use client";
import { translate as t, useI18n } from "@/lib/i18n";


import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Trash2 } from "lucide-react";
import { localizedToast as toast } from "./providers";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { useScopedRequest } from "./vtl-scope";
import { bucketLink } from "./bucket-selector";
import { formatDate } from "@/lib/api";

type Deletion = { request_id: string; barcodes: string[]; confirmation: "delete-local" };
type Options = { ready: boolean; reason: string | null; pending: Deletion | null };
type Preview = Options & { cartridges: { barcode: string; exports: { destination_id: string; bucket: string | null; generation: number; checked_at: string }[] }[] };

export function DeleteCartridges({ barcodes, connected, onDeleted }: {
  barcodes: string[]; connected: boolean; onDeleted: (barcodes: string[]) => Promise<void>;
}) {
  useI18n();
  const request = useScopedRequest();
  const [pending, setPending] = useState<Deletion | null>(null);
  const [deletion, setDeletion] = useState<Deletion | null>(null);
  const [optionsError, setOptionsError] = useState("");
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!connected) return;
    const abort = new AbortController();
    void request<Options>("/v1/cartridges/deletion", { signal: abort.signal })
      .then(value => { if (!abort.signal.aborted) { setPending(value.pending ?? null); setOptionsError(""); } })
      .catch(error => { if (!abort.signal.aborted) setOptionsError(error.message); });
    return () => abort.abort();
  }, [request, connected, revision]);
  return <>
    {(barcodes.length > 0 || pending || optionsError) && <div className="flex flex-wrap items-center gap-3">
      <Button variant="outline" size="sm" title={pending ? t("Reprendre la suppression") : t("Supprimer ({count})", { count: barcodes.length })} disabled={!connected || (!pending && (barcodes.length === 0 || barcodes.length > 64))}
        onClick={() => setDeletion(pending ?? { request_id: Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join(""), barcodes: [...barcodes], confirmation: "delete-local" })}>
        <Trash2 data-icon="inline-start" />{pending ? t("Reprendre la suppression") : <><span className="lto-tool-label lto-action-label">{t("Supprimer")} </span>({barcodes.length})</>}
      </Button>
      {barcodes.length > 64 && <p className="text-sm text-muted-foreground">{t("Sélectionnez au plus 64 cartouches.")}</p>}
      {optionsError && <p role="alert" className="text-sm text-destructive">{t("Suivi des suppressions indisponible.")} <Button size="sm" variant="ghost" onClick={() => setRevision(value => value + 1)}>{t("Réessayer")}</Button></p>}
    </div>}
    {deletion && <DeletionDialog key={deletion.request_id} deletion={deletion} connected={connected}
      onClose={() => { setDeletion(null); setRevision(value => value + 1); }}
      onDeleted={async () => { setPending(null); setDeletion(null); setRevision(value => value + 1); await onDeleted(deletion.barcodes); }} />}
  </>;
}

function DeletionDialog({ deletion, connected, onClose, onDeleted }: {
  deletion: Deletion; connected: boolean; onClose: () => void; onDeleted: () => Promise<void>;
}) {
  useI18n();
  const request = useScopedRequest();
  const [preview, setPreview] = useState<Preview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [revision, setRevision] = useState(0);
  const submitting = useRef(false);
  const [client, setClient] = useState<string | null>(null);
  const [idleConfirmed, setIdleConfirmed] = useState(false);
  const [entering, setEntering] = useState(false);
  const [maintenanceError, setMaintenanceError] = useState("");
  const veeam = client === "veeam";
  useEffect(() => {
    const abort = new AbortController();
    void request<{ client?: string | null }>("/v1/maintenance", { signal: abort.signal })
      .then(value => { if (!abort.signal.aborted) setClient(value.client ?? null); })
      .catch(cause => { if (!abort.signal.aborted) console.error(cause); });
    return () => abort.abort();
  }, [request]);
  async function enterMaintenance() {
    if (entering || (veeam && !idleConfirmed)) return;
    setEntering(true); setMaintenanceError("");
    try {
      // For Veeam, the checkbox above the button is the confirmation that no job uses the library.
      await request("/v1/maintenance/enter", { method: "POST", signal: AbortSignal.timeout(610000),
        ...(veeam ? { headers: { "Content-Type": "application/json" }, body: JSON.stringify({ confirm_idle: true }) } : {}) });
      setRevision(value => value + 1);
    } catch (cause) { setMaintenanceError(cause instanceof Error ? cause.message : t("Passage en maintenance non confirmé.")); }
    finally { setEntering(false); }
  }
  useEffect(() => {
    const abort = new AbortController();
    setLoading(true); setError(""); setPreview(null);
    void request<Preview>("/v1/cartridges/deletion", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ barcodes: deletion.barcodes }), signal: abort.signal })
      .then(value => {
        if (abort.signal.aborted) return;
        if (!Array.isArray(value.cartridges) || value.cartridges.length !== deletion.barcodes.length
            || value.cartridges.some((item, i) => item.barcode !== deletion.barcodes[i] || !Array.isArray(item.exports)))
          throw new Error(t("Aperçu incomplet. Actualisez avant de confirmer."));
        if (value.pending && (value.pending.request_id !== deletion.request_id || JSON.stringify(value.pending.barcodes) !== JSON.stringify(deletion.barcodes)))
          throw new Error(t("Une autre suppression est à reprendre. Fermez ce dialogue puis utilisez « Reprendre la suppression »."));
        setPreview(value);
      })
      .catch(cause => { if (!abort.signal.aborted) setError(cause.message); })
      .finally(() => { if (!abort.signal.aborted) setLoading(false); });
    return () => abort.abort();
  }, [request, deletion, revision]);
  async function submit() {
    if (!preview?.ready || !acknowledged || !connected || submitting.current) return;
    submitting.current = true; setBusy(true); setError("");
    try {
      const result = await request<{ phase: string; request_id: string; barcodes: string[] }>("/v1/cartridges/delete-local", {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(deletion), signal: AbortSignal.timeout(310000),
      });
      if (result.phase !== "completed" || result.request_id !== deletion.request_id || JSON.stringify(result.barcodes) !== JSON.stringify(deletion.barcodes))
        throw new Error(t("Suppression non confirmée. Réessayez pour reprendre la même demande."));
      toast.success(t("Cartouches supprimées localement. Exports S3 conservés."));
      await onDeleted();
    } catch (cause) { setError(cause instanceof Error ? cause.message : t("Suppression non confirmée. Réessayez.")); }
    finally { submitting.current = false; setBusy(false); }
  }
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}>
    <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-xl" showCloseButton={false}>
      <DialogHeader icon={Trash2}>
        <DialogTitle>{t("Supprimer localement")}</DialogTitle>
        <DialogDescription>{t("Les fichiers locaux sont effacés définitivement. Les copies S3 sont conservées.")}</DialogDescription>
      </DialogHeader>
      {loading ? <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle className="size-4 animate-spin" />{t("Chargement des liens S3…")}</p> : preview && <div className="space-y-4">
        <ul className="divide-y divide-border text-sm">
          {preview.cartridges.map(tape => {
            const latest = [...tape.exports].sort((a, b) => b.checked_at.localeCompare(a.checked_at))[0];
            return <li key={tape.barcode} className="break-words py-2 first:pt-0">{latest
              ? <span>{tape.barcode} · ✓ {t("copie S3")} · <a className="underline underline-offset-4" href={bucketLink("storage", latest.destination_id)} target="_blank" rel="noopener noreferrer">{latest.bucket ?? t("Destination S3")}</a> v{latest.generation}</span>
              : <span className="text-destructive">{tape.barcode} · ⚠ {t("aucune copie S3 : données perdues")}</span>}</li>;
          })}
        </ul>
        {!preview.ready && <div className="space-y-3 rounded-lg border p-3 text-sm">
          <p>{t(veeam ? "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis Veeam."
            : client === "pbs" ? "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis PBS."
            : "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis le logiciel de sauvegarde.")}</p>
          {veeam && <label className="flex items-start gap-2"><Checkbox className="mt-0.5" checked={idleConfirmed} disabled={entering || !connected} onCheckedChange={value => setIdleConfirmed(value === true)} />{t("Aucun job Veeam n’utilise cette bibliothèque")}</label>}
          <Button size="sm" variant="outline" disabled={entering || !connected || (veeam && !idleConfirmed)} onClick={() => void enterMaintenance()}>
            {entering && <LoaderCircle data-icon="inline-start" className="animate-spin" />}{t(entering ? "Mise en maintenance…" : "Passer en maintenance")}</Button>
          {maintenanceError && <p role="alert" className="text-destructive">{t(maintenanceError)}</p>}
        </div>}
      </div>}
      {error && <Alert variant="destructive"><AlertTitle>{t("Suppression non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
      {preview?.ready && <label className="flex items-start gap-3 text-sm leading-relaxed">
        <Checkbox className="mt-1" checked={acknowledged} disabled={busy || loading} onCheckedChange={v => setAcknowledged(v === true)} />{t("J’ai compris que les données locales des cartouches sélectionnées seront définitivement supprimées.")}</label>}
      <DialogFooter>
        <Button variant="outline" disabled={busy} onClick={onClose}>{t("Annuler")}</Button>
        <Button variant="ghost" disabled={busy || loading} onClick={() => setRevision(value => value + 1)}>{t("Actualiser")}</Button>
        <Button variant="destructive" disabled={busy || loading || !preview?.ready || !acknowledged || !connected} onClick={() => void submit()}>
          {busy && <LoaderCircle data-icon="inline-start" className="animate-spin" />}{t("Supprimer")}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
