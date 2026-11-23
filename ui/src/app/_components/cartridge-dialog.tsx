"use client";

import type { ReactNode } from "react";
import { CloudUpload, HardDrive, X } from "lucide-react";
import { useI18n } from "@/lib/i18n";
import { formatBytes, formatDate, isActive, jobLabels, mediaLabels, type Cartridge, type Job } from "@/lib/api";
import { driveLabels, occupancy, type DriveObservation, type MediaObservation } from "@/lib/telemetry";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  Dialog, DialogClose, DialogContent, DialogDescription,
  DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import { CartridgeActivity } from "./cartridge-activity";

/** One cartridge: state line, its details, send action; history folded. */
export function CartridgeDialog({ barcode, media, location, observation, drive, jobs, onExport, exportDisabledReason, onClose }: {
  barcode: string;
  media?: Cartridge;
  location: string | null;
  /** Live measures of the tape's volume, when the telemetry has them. */
  observation?: MediaObservation;
  /** The drive holding the tape, when it is loaded. */
  drive?: DriveObservation;
  jobs: Job[];
  onExport: () => void;
  exportDisabledReason: string | null;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const active = jobs.some(job => job.subject === barcode && isActive(job));
  const state = media
    ? [t(mediaLabels[media.state] ?? media.state), formatBytes(occupancy(media, observation).used), media.local_deleted ? t("Supprimée localement") : location ?? t("Dans la bibliothèque")].join(" · ")
    : t("État inconnu");
  const copy = media?.last_copy;
  return (
    <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
      <DialogContent className="sm:max-w-2xl" showCloseButton={false}>
        <DialogHeader icon={HardDrive}>
          <DialogTitle>{t("Cartouche {barcode}", { barcode })}</DialogTitle>
          <DialogDescription>{state}</DialogDescription>
        </DialogHeader>
        <DialogClose asChild>
          <Button className="absolute top-2 right-2" variant="ghost" size="icon-sm"
            aria-label={t("Fermer les détails de la cartouche")}>
            <X />
          </Button>
        </DialogClose>
        {media ? <CartridgeDetails media={media} location={location} observation={observation} drive={drive}
          running={jobs.filter(job => job.kind === "export" && job.subject === barcode && isActive(job))} />
          : <p className="text-sm">{t("Aucune copie S3 pour l’instant.")}</p>}
        <div className="flex flex-col items-start gap-2 border-b pb-4 sm:flex-row sm:items-center sm:justify-between">
          {exportDisabledReason && <p id="cartridge-export-help" className="text-sm text-muted-foreground">{t(exportDisabledReason)}</p>}
          <Button className="w-full shrink-0 sm:ml-auto sm:w-auto" disabled={!!exportDisabledReason || active}
            aria-describedby={exportDisabledReason ? "cartridge-export-help" : undefined} onClick={onExport}>
            <CloudUpload data-icon="inline-start" />{t("Envoyer vers S3")}
          </Button>
        </div>
        <details>
          <summary className="cursor-pointer text-sm font-medium text-muted-foreground">{t("Historique des envois")}</summary>
          <div className="mt-3"><CartridgeActivity barcode={barcode} jobs={jobs} /></div>
        </details>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t("Fermer")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** What the appliance knows about the tape, one line per fact. */
function CartridgeDetails({ media, location, observation, drive, running }: {
  media: Cartridge;
  location: string | null;
  observation?: MediaObservation;
  drive?: DriveObservation;
  running: Job[];
}) {
  const { t, locale } = useI18n();
  const fill = occupancy(media, observation);
  const percent = fill.percent === null ? null : new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(fill.percent);
  const copy = media.last_copy;
  const local = observation?.barcode === media.barcode ? observation.local_size_bytes : null;
  // Veeam or PBS wrote to the tape after its copy: the local volume outgrew what was copied.
  const stale = copy?.bytes != null && local != null && local > copy.bytes;
  const position = media.local_deleted
    ? t("Supprimée localement")
    : drive
      ? `${drive.label} · ${t(driveLabels[drive.state])}`
      : location ?? t("Dans la bibliothèque");
  const rows: [string, ReactNode][] = [
    [t("État"), t(mediaLabels[media.state] ?? media.state)],
    [t("Position"), position],
    ...(media.media_type ? [[t("Type de média"), media.media_type] as [string, ReactNode]] : []),
    [t("Occupation"), <div key="fill" className="space-y-1">
      <p>{percent === null
        ? formatBytes(fill.used)
        : t("{used} sur {capacity} · {percent} %", { used: formatBytes(fill.used), capacity: formatBytes(fill.capacity), percent })}</p>
      {fill.percent !== null && <Progress className="max-w-xs" value={fill.percent} aria-label={t("Occupation de la cartouche")} />}
      <p className="text-xs text-muted-foreground">{t(fill.label)}</p>
    </div>],
    [t("Version"), String(media.generation)],
    [t("Copie S3"), copy
      ? <div key="copy" className="space-y-1">
        <p>{[copy.bucket ?? t("Destination S3"), t("version {generation}", { generation: copy.generation }),
          copy.bytes != null ? formatBytes(copy.bytes) : null].filter(Boolean).join(" · ")}</p>
        <p className="text-xs text-muted-foreground">{t("Vérifiée le {date}", { date: formatDate(copy.checked_at) })}</p>
        {stale && <p className="text-xs text-[var(--dashboard-amber)]">{t("Des écritures ont eu lieu après cette copie.")}</p>}
      </div>
      : t("Aucune copie S3 pour l’instant.")],
    ...(running.length ? [[t("Envoi vers S3"), running.map(job => t(jobLabels[job.status] ?? job.status)).join(" · ")] as [string, ReactNode]] : []),
  ];
  return (
    <dl className="lto-cartridge-details" aria-label={t("Détails de la cartouche")}>
      {rows.map(([term, value]) => <div key={term}><dt>{term}</dt><dd>{value}</dd></div>)}
    </dl>
  );
}
