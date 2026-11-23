"use client";
import { Cloud, CloudDownload } from "lucide-react";
import { useI18n } from "@/lib/i18n";
import { formatBytes } from "@/lib/api";
import { copyStatus } from "./storage-copy-status";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import type { RecoveryCopy } from "./storage-recovery-dialog";

/** One S3 copy: state line, main action « Réintégrer… », technical details folded. */
export function StorageCopyDialog({ copy, canRestore, canDelete, onDelete, onClose, onRestore, onBucket }: {
  copy: RecoveryCopy; canRestore: boolean; canDelete: boolean; onDelete?: () => void; onClose: () => void; onRestore: () => void; onBucket: () => void;
}) {
  const { t, locale } = useI18n();
  const readable = copy.readable && !!copy.manifest_sha256;
  const state = [t(copyStatus(copy).label), copy.logical_size_bytes == null ? t("Volume non confirmé") : formatBytes(copy.logical_size_bytes), copy.bucket].join(" · ");
  const deleteBlocked = !copy.writable ? "La suppression est indisponible, car le bucket est en lecture seule ou son accès n’est pas confirmé." : !readable ? "La suppression sera possible quand le manifeste aura été lu." : !canDelete ? "La suppression est indisponible pour le moment." : "";
  const fields: [string, string | undefined | null][] = [
    ["Génération", copy.generation.toLocaleString(locale)],
    ["Fichiers de la cartouche virtuelle", copy.file_count == null ? t("Non confirmé") : copy.file_count.toLocaleString(locale)],
    ["Segments S3", copy.segment_count == null ? t("Non confirmé") : copy.segment_count.toLocaleString(locale)],
    ["Chemin du manifeste", copy.manifest_key],
    ["Empreinte SHA-256 du manifeste", copy.manifest_sha256],
    ["Format du manifeste", copy.format_version?.toLocaleString(locale)],
  ];
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent className="sm:max-w-xl">
      <DialogHeader icon={Cloud}><DialogTitle>{t("Copie S3 · {barcode}", { barcode: copy.barcode })}</DialogTitle><DialogDescription className="break-words">{state}</DialogDescription></DialogHeader>
      {copy.raw_status === "Archived" && <p className="text-sm text-muted-foreground">{t("Les données sont archivées à froid. Rappelez le bucket entier pour les rendre lisibles sur S3.")}</p>}
      {copy.deletion_status === "failed" && <p className="text-sm text-destructive">{t("Des objets peuvent déjà avoir été supprimés. Relancez la suppression de cette copie pour la terminer.")}</p>}
      {readable && <p className="text-xs text-muted-foreground">{t("Manifeste lu")}</p>}
      <details className="border-t pt-3">
        <summary className="cursor-pointer text-sm text-muted-foreground">{t("Informations techniques")}</summary>
        <dl className="mt-4 space-y-3 text-sm">
          {fields.map(([label, value]) => <div key={label}><dt className="text-muted-foreground">{t(label)}</dt><dd className="mt-1 break-all font-mono text-xs">{value ?? t("Non confirmé")}</dd></div>)}
        </dl>
      </details>
      <DialogFooter className="sm:items-center">
        {onDelete && <span className="sm:mr-auto"><Button variant="link" className="text-destructive" disabled={!!deleteBlocked} onClick={onDelete}>{t("Supprimer cette copie…")}</Button></span>}
        {onDelete && deleteBlocked && <p className="text-xs text-muted-foreground sm:mr-auto">{t(deleteBlocked)}</p>}
        <Button variant="outline" onClick={onClose}>{t("Fermer")}</Button>
        {readable
          ? <Button disabled={!canRestore} onClick={onRestore}>{t("Réintégrer…")}</Button>
          : <Button variant="outline" onClick={onBucket}>{copy.raw_status === "Archived" && <CloudDownload />}{t(copy.raw_status === "Archived" ? "Rappeler le bucket" : "Voir le bucket")}</Button>}
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
