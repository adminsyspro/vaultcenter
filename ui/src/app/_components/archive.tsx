"use client";
import { useEffect, useId, useRef, useState } from "react";
import { Archive as ArchiveIcon, CircleAlert, CircleCheck, CloudDownload, LoaderCircle } from "lucide-react";
import { translate as t, useI18n } from "@/lib/i18n";
import { type Job, isActive, formatBytes, formatDate, kindLabels, jobSummary } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { bucketLink, bucketPath } from "./bucket-selector";
import { useScopedRequest } from "./vtl-scope";

type Operation = {
  job_id: string; prepare_job_id: string; phase: string; operation: "archive" | "recall"; bucket: string;
  observed_status: string | null; observed_at: string | null; restored_until: string | null;
};
export type StorageContext = {
  destination_id?: string; active?: boolean; bucket: string; raw_status: string | null; managed?: boolean; next_bucket?: string | null;
  inventory?: { bytes: number; cartridges: { barcode: string }[] } | null; catalogue_generations?: unknown[];
};
const phases: Record<string, string> = {
  queued: "Demande en attente", submitting: "La demande est en cours d’envoi à OVH. Ne la renouvelez pas.",
  uncertain: "Le résultat de la demande est incertain. Vérifiez-le chez OVH.", accepted: "Demande acceptée, traitement OVH en cours",
  archived: "Archivage terminé chez OVH", restored: "Données accessibles en lecture seule",
  failed: "Demande arrêtée avant l’envoi",
};

/** The state of one bucket in one line, with its single action. */
export function BucketState({ destinationId, bucketName, createdAt, canManage, jobs, onRefresh, onStorageRefresh, storage, connected }: {
  destinationId: string; bucketName: string; createdAt?: string; canManage: boolean; jobs: Job[];
  onRefresh: () => Promise<void>; onStorageRefresh: () => Promise<void>; storage: StorageContext; connected: boolean;
}) {
  const { locale } = useI18n();
  const request = useScopedRequest();
  const [overview, setOverview] = useState<{ configured: boolean; operations: Operation[] } | null>(null);
  const [confirming, setConfirming] = useState<"archive" | "recall" | null>(null);
  const [billing, setBilling] = useState(false), [exclusive, setExclusive] = useState(false), [cost, setCost] = useState(false);
  const [error, setError] = useState(""), [overviewError, setOverviewError] = useState(""), [pending, setPending] = useState(false);
  const submitting = useRef(false);
  const confirmationTitle = useId();
  useEffect(() => {
    const abort = new AbortController();
    void request<{ configured: boolean; operations: Operation[] }>(bucketPath("/v1/storage/archive", destinationId), { signal: abort.signal })
      .then(value => { if (!abort.signal.aborted) { setOverview(value); setOverviewError(""); } })
      .catch(cause => { if (!abort.signal.aborted) { setOverview(null); setOverviewError(cause instanceof Error ? cause.message : t("Actualisation impossible.")); } });
    return () => abort.abort();
  }, [jobs, request, destinationId]);
  const operations = overview?.operations ?? [];
  const inProgress = operations.filter(o => ["queued", "submitting", "uncertain", "accepted"].includes(o.phase));
  const verifying = jobs.some(job => /^(archive|recall)_submit$/.test(job.kind) && job.destination_id === destinationId && isActive(job));
  const raw = storage.raw_status;
  const kind = verifying || inProgress.length || raw === "Archiving" || raw === "Restoring" ? "progress"
    : raw === "None" ? "available" : raw === "Archived" ? "archived" : raw === "Restored" ? "recalled" : "unknown";
  const recalling = raw === "Restoring" || inProgress.some(o => o.operation === "recall");
  const restoredUntil = operations.find(o => o.operation === "recall" && o.restored_until)?.restored_until ?? null;
  const busyVtl = jobs.some(isActive);
  const available = canManage && connected && !!overview?.configured && !pending && !busyVtl;
  // A refused or settled archive (e.g. next bucket already taken) is one line; the next attempt proposes a new name.
  const lastArchive = jobs.find(job => job.kind === "archive_submit" && job.destination_id === destinationId);
  const lastFailure = lastArchive?.status === "failed" ? lastArchive : null;
  const summary = t("Cartouches : {cartridges} · volume : {bytes} · copies enregistrées : {copies}", {
    cartridges: storage.inventory?.cartridges.length ?? 0, bytes: formatBytes(storage.inventory?.bytes ?? 0), copies: storage.catalogue_generations?.length ?? 0,
  });
  async function action(path: string, body: object = {}) {
    if (submitting.current || !(canManage && connected)) return;
    submitting.current = true; setPending(true); setError("");
    try {
      await request<Job>(bucketPath(path, destinationId), { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      setConfirming(null); setBilling(false); setExclusive(false); setCost(false);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("Demande non confirmée. Consultez les dernières opérations avant de réessayer."));
    } finally {
      await onRefresh().catch(cause => console.error("Bucket refresh failed", cause));
      submitting.current = false; setPending(false);
    }
  }
  const line = (icon: React.ReactNode, title: string, detail: string | null) => <div className="flex items-start gap-3">
    {icon}<div className="min-w-0 space-y-1"><p className="font-semibold">{title}</p>{detail && <p className="text-sm text-muted-foreground">{detail}</p>}</div>
  </div>;
  return <div className="space-y-5">
    {error && <Alert variant="destructive"><AlertTitle>{t("Action non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
    {overviewError && <Alert variant="destructive"><AlertDescription>{t(overviewError)}</AlertDescription></Alert>}
    {kind === "available" && line(<CircleCheck aria-hidden="true" className="mt-0.5 size-5 text-emerald-600 dark:text-emerald-400" />,
      t(storage.active ? "Disponible · bucket actif" : "Disponible"),
      summary + (createdAt ? ` · ${t("créé le {date}", { date: formatDate(createdAt) })}` : ""))}
    {kind === "progress" && <div className="space-y-3">
      {line(<LoaderCircle aria-hidden="true" className="mt-0.5 size-5 text-primary motion-safe:animate-spin" />,
        t(recalling ? "Rappel en cours chez OVH" : verifying && !inProgress.some(o => o.phase !== "queued") ? "Vérification des copies en cours" : "Archivage en cours chez OVH"),
        t("Le suivi continue même si vous fermez cette fenêtre."))}
      <div role="progressbar" aria-label={t("Progression chez OVH")} className="h-2 overflow-hidden rounded-full bg-muted"><div className="h-full w-1/3 rounded-full bg-primary motion-safe:animate-pulse" /></div>
    </div>}
    {kind === "archived" && line(<ArchiveIcon aria-hidden="true" className="mt-0.5 size-5 text-primary" />, t("Archivé sur bande"), t("Rappelez le bucket pour lire ses copies. Le rappel est facturé par OVH."))}
    {kind === "recalled" && line(<CircleCheck aria-hidden="true" className="mt-0.5 size-5 text-emerald-600 dark:text-emerald-400" />,
      restoredUntil ? t("Rappelé · lecture seule jusqu’au {date}", { date: new Date(restoredUntil).toLocaleDateString(locale, { day: "2-digit", month: "2-digit" }) }) : t("Rappelé · lecture seule"), null)}
    {kind === "unknown" && line(<CircleAlert aria-hidden="true" className="mt-0.5 size-5 text-muted-foreground" />, t("État du bucket à vérifier"), t("Lisez l’état actuel dans les dernières opérations."))}
    {kind === "available" && lastFailure?.message && <p role="alert" className="text-sm text-destructive">{t(lastFailure.message)}</p>}

    {canManage && kind === "available" && confirming !== "archive" && <Button disabled={!available} onClick={() => setConfirming("archive")}><ArchiveIcon />{t("Archiver sur bande…")}</Button>}
    {canManage && kind === "available" && busyVtl && <p className="text-sm text-muted-foreground">{t("Attendez la fin des opérations en cours pour archiver ce bucket.")}</p>}
    {canManage && kind === "archived" && confirming !== "recall" && <Button disabled={!available} onClick={() => setConfirming("recall")}><CloudDownload />{t("Rappeler le bucket…")}</Button>}
    {kind === "recalled" && <Button asChild variant="outline"><a href={bucketLink("media", destinationId)}>{t("Restaurer une cartouche…")}</a></Button>}

    {confirming === "archive" && kind === "available" && <section aria-labelledby={confirmationTitle} className="space-y-4 border-t pt-4">
      <h3 id={confirmationTitle} className="font-semibold">{t("Archiver sur bande")}</h3>
      <dl className="space-y-3 text-sm sm:grid sm:grid-cols-[max-content_1fr] sm:gap-x-6 sm:gap-y-2 sm:space-y-0">
        {storage.active && storage.next_bucket && <div className="space-y-0.5 sm:contents">
          <dt className="text-muted-foreground">{t("Prochains exports")}</dt><dd className="break-all font-mono text-xs leading-5">{storage.next_bucket}</dd>
        </div>}
        <div className="space-y-0.5 sm:contents"><dt className="text-muted-foreground">{t("Lecture")}</dt><dd>{t("après un rappel payant, pendant 30 jours")}</dd></div>
        <div className="space-y-0.5 sm:contents"><dt className="text-muted-foreground">{t("Facturation OVH")}</dt><dd>{t("1 Tio minimum pendant 180 jours")}</dd></div>
      </dl>
      <div className="space-y-2">
        <label className="flex items-start gap-2 text-sm"><Checkbox className="mt-0.5" checked={billing} onCheckedChange={value => setBilling(value === true)} />{t("J’ai compris les conditions OVH")}</label>
        {!storage.managed && <label className="flex items-start gap-2 text-sm"><Checkbox className="mt-0.5" checked={exclusive} onCheckedChange={value => setExclusive(value === true)} />{t("Ce bucket est réservé à cette VTL.")}</label>}
      </div>
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="outline" disabled={pending} onClick={() => setConfirming(null)}>{t("Annuler")}</Button>
        <Button variant="destructive" disabled={!available || !billing || (!storage.managed && !exclusive)}
          onClick={() => void action("/v1/storage/archive", { confirm_bucket: storage.bucket, acknowledge_billing: true, exclusive_bucket: !!storage.managed || exclusive, next_bucket: storage.active ? storage.next_bucket ?? null : null })}>
          {pending && <LoaderCircle className="motion-safe:animate-spin" />}{t("Archiver")}</Button>
      </div>
    </section>}
    {confirming === "recall" && kind === "archived" && <section aria-labelledby={confirmationTitle} className="space-y-4 border-t pt-4">
      <h3 id={confirmationTitle} className="font-semibold">{t("Rappeler le bucket")}</h3>
      <p className="text-sm">{t("OVH rend le bucket entier lisible pendant 30 jours, en lecture seule.")}</p>
      <label className="flex items-start gap-2 text-sm"><Checkbox className="mt-0.5" checked={cost} onCheckedChange={value => setCost(value === true)} />{t("J’accepte les frais de rappel du bucket entier.")}</label>
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="outline" disabled={pending} onClick={() => setConfirming(null)}>{t("Annuler")}</Button>
        <Button disabled={!available || !cost} onClick={() => void action("/v1/storage/recall", { confirm_bucket: storage.bucket, acknowledge_retrieval_cost: true })}>{pending && <LoaderCircle className="motion-safe:animate-spin" />}{t("Rappeler")}</Button>
      </div>
    </section>}

    {!confirming && <details className="border-t pt-3">
      <summary className="cursor-pointer text-sm font-medium text-muted-foreground">{t("Dernières opérations")}</summary>
      <div className="mt-3 space-y-2">
        {!operations.length && <p className="text-sm text-muted-foreground">{t("Aucune opération OVH enregistrée.")}</p>}
        {operations.slice(0, 5).map(o => <div key={o.job_id} className="rounded-lg border p-3 text-sm">
          <strong>{o.operation === "recall" ? t("Rappel") : t("Archivage")} · {o.bucket}</strong>
          <p>{t(phases[o.phase] ?? "État inconnu")}</p>
          {o.observed_at && <p className="text-xs text-muted-foreground">{t("Dernière observation :")} {o.observed_status ?? t("inconnu")} · {formatDate(o.observed_at)}</p>}
        </div>)}
        {jobs.filter(job => /^(archive|recall)_/.test(job.kind) && job.destination_id === destinationId).slice(0, 5).map(job => <div key={job.id} className="rounded-lg border p-3 text-sm">
          <strong>{t(kindLabels[job.kind] ?? job.kind)}</strong><p>{jobSummary(job)}</p>
        </div>)}
        <div className="flex flex-wrap gap-2 pt-1">
          <Button variant="outline" size="sm" onClick={() => void Promise.allSettled([onStorageRefresh(), onRefresh()])}>{t("Actualiser")}</Button>
          {canManage && inProgress.length > 0 && <Button variant="outline" size="sm" disabled={!available} onClick={() => void action("/v1/storage/archive/refresh")}>{t("Vérifier la demande chez OVH")}</Button>}
        </div>
      </div>
    </details>}
  </div>;
}
