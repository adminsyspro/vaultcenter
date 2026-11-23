"use client";
import { useRef, useState } from "react";
import { Trash2, LoaderCircle, TriangleAlert } from "lucide-react";
import { useI18n } from "@/lib/i18n";
import { formatBytes, type Job } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { useScopedRequest } from "./vtl-scope";
import type { RecoveryCopy } from "./storage-recovery-dialog";

/** Deleting one copy shows what remains of this cartridge elsewhere; nothing is retyped. */
export function StorageDeleteDialog({ copy, others, available, onClose, onStarted }: { copy: RecoveryCopy; others: RecoveryCopy[]; available: boolean; onClose: () => void; onStarted: (job: Job) => void }) {
  const { t } = useI18n();
  const request = useScopedRequest();
  const pending = useRef(false);
  const [busy, setBusy] = useState(false), [error, setError] = useState("");
  const enabled = available && !!copy.writable && copy.readable && !!copy.manifest_sha256 && !busy;
  async function submit() {
    if (!enabled || pending.current) return;
    pending.current = true; setBusy(true); setError("");
    try {
      onStarted(await request<Job>("/v1/storage/copies/delete", { method: "POST", headers: { "Content-Type": "application/json" }, signal: AbortSignal.timeout(45000),
        body: JSON.stringify({ destination_id: copy.destination_id, configuration_revision: copy.configuration_revision ?? null, barcode: copy.barcode, generation: copy.generation, manifest_sha256: copy.manifest_sha256, confirm_barcode: copy.barcode, confirm_bucket: copy.bucket }) }));
    } catch (cause) { setError(cause instanceof Error ? cause.message : t("Suppression non confirmée. Consultez les opérations avant de réessayer.")); }
    finally { pending.current = false; setBusy(false); }
  }
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}><DialogContent className="sm:max-w-lg">
    <DialogHeader icon={Trash2}><DialogTitle>{t("Supprimer la copie S3 de {barcode}", { barcode: copy.barcode })}</DialogTitle>
      <DialogDescription className="break-words">{t("Dans {bucket} · génération {generation} · {volume}", { bucket: copy.bucket, generation: copy.generation, volume: copy.logical_size_bytes == null ? t("Volume non confirmé") : formatBytes(copy.logical_size_bytes) })}</DialogDescription></DialogHeader>
    <div className="space-y-1 text-sm">
      <p className="font-medium">{t("Autres copies de {barcode}", { barcode: copy.barcode })}</p>
      {others.length
        ? <ul className="space-y-1">{others.map(other => <li key={`${other.destination_id}:${other.generation}`} className="break-all">{other.bucket} · {t("génération {generation}", { generation: other.generation })}</li>)}</ul>
        : <p className="text-destructive">⚠ {t("aucune autre copie utilisable")}</p>}
    </div>
    {error && <Alert variant="destructive"><TriangleAlert /><AlertTitle>{t("Suppression non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t("Annuler")}</Button><Button variant="destructive" disabled={!enabled} onClick={() => void submit()}>{busy ? <LoaderCircle className="motion-safe:animate-spin" /> : <Trash2 />}{t(busy ? "Contrôle et demande en cours…" : "Supprimer définitivement")}</Button></DialogFooter>
  </DialogContent></Dialog>;
}
