"use client";
import type {ReactNode} from "react";
import {useI18n,translate as t} from '@/lib/i18n';
import { CircleCheck, CircleAlert, Clock3, LoaderCircle } from "lucide-react";
import { type Job, formatBytes, formatDate, kindLabels } from "@/lib/api";

const labels: Record<string, string> = {
  status: "État", error: "Erreur", message: "Message", barcode: "Cartouche",
  generation: "Version", bucket: "Bucket", prefix: "Préfixe", provider: "Fournisseur",
  provider_label: "Fournisseur", endpoint: "Adresse S3", region: "Région",
  bytes: "Volume", size: "Taille", logical_size_bytes: "Volume logique",
  bytes_written: "Volume écrit", size_bytes: "Taille", segment_count: "Nombre de segments",
  segments: "Segments", files: "Fichiers", objects: "Objets", cartridges: "Cartouches",
  manifest_key: "Chemin du manifeste", manifest_sha256: "Empreinte SHA-256 du manifeste",
  sha256: "Empreinte SHA-256", key: "Chemin S3", path: "Chemin", name: "Nom",
  destination: "Dossier de destination", scope: "Périmètre", inventory: "Inventaire",
  digest: "Empreinte de contrôle", lease: "Identifiant de maintenance",
  expires_at: "Expiration", created_at: "Création", updated_at: "Dernière mise à jour",
  verified: "Vérifié", complete: "Complet", readable: "Lecture autorisée",
  writable: "Écriture autorisée", supported: "Pris en charge", read_tested: "Lecture testée",
  write_tested: "Écriture testée", raw_status: "État du fournisseur",
  outside_prefix: "Objets hors préfixe", unrecognized_objects: "Objets non reconnus",
  manifests: "Manifestes", configuration_id: "Configuration", multipart_uploads: "Transferts multiparties",
  archive_lock_until: "Fin du verrou d’archive", restored: "Restauré", uploaded: "Transféré",
};
const states: Record<string, string> = {
  ok: "Réussi", restored: "Restauré", verified: "Vérifié", completed: "Terminé",
  failed: "Échec", running: "En cours", queued: "En attente", request_accepted: "Demande acceptée",
  Archived: "Archivé", Archiving: "Archivage en cours", Restoring: "Rappel en cours", Restored: "Rappel terminé",
};
function label(key: string) {
  return labels[key] ? t(labels[key]) : key.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/[_-]/g, " ").replace(/^./, c => c.toUpperCase());
}
function Value({ value, field = "" }: { value: unknown; field?: string }) {
 const {t,locale}=useI18n();
  if (value === null) return <span className="text-muted-foreground">{t("Non renseigné")}</span>;
  if (Array.isArray(value)) return value.length ? <ol className="space-y-3">{value.map((item, index) => <li key={index} className="min-w-0 rounded-md border p-3"><Value value={item} /></li>)}</ol> : <span className="text-muted-foreground">{t("Aucun élément")}</span>;
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    return entries.length ? <dl className="min-w-0 divide-y rounded-lg border px-3">{entries.map(([key, item]) => <div key={key} className="min-w-0 space-y-1 py-3"><dt className="text-xs font-medium text-muted-foreground">{label(key)}</dt><dd className="min-w-0 break-words text-sm [overflow-wrap:anywhere]"><Value value={item} field={key} /></dd></div>)}</dl> : <span className="text-muted-foreground">{t("Aucun détail")}</span>;
  }
  if (typeof value === "boolean") return <span>{value ? t("Oui") : t("Non")}</span>;
  if (typeof value === "number") {
    if ((field === "bytes" || field.endsWith("_bytes") || field === "bytes_written") && value >= 0) return <span>{formatBytes(value)}</span>;
    if (field.endsWith("_at") && value > 0) {
      const date = new Date(value * 1000);
      if (!Number.isNaN(date.getTime())) return <span>{formatDate(date.toISOString())}</span>;
    }
    return <span>{value.toLocaleString(locale)}</span>;
  }
  const text = String(value);
  return <span className="whitespace-pre-wrap [overflow-wrap:anywhere]">{["status", "raw_status"].includes(field) ? t(states[text] ?? text) : field.endsWith("_at") ? formatDate(text) : text || "—"}</span>;
}
export function WorkerResult({ job, compact = false, hideMessages = false, metadata }: { job: Job; compact?: boolean; hideMessages?: boolean; metadata?: ReactNode }) {
 const {t,locale}=useI18n();
  let value: unknown = job.message;
  try { value = job.message ? JSON.parse(job.message) : null; } catch { /* Preserve actual error output. */ }
  const result = value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
  const text = typeof value === "string" ? value : null;
  const messages: string[] = [];
  if (result) {
    for (const key of ["error", "message"]) if (typeof result[key] === "string") messages.push(result[key] as string);
    if (typeof result.logical_size_bytes === "number") messages.push(t("Volume traité : {size}.", {size:formatBytes(result.logical_size_bytes)}));
    if (typeof result.segment_count === "number") messages.push(t("{count} segment(s) dans la copie.", {count:result.segment_count.toLocaleString(locale)}));
    if (Array.isArray(result.files)) messages.push(t("{count} fichier(s) référencé(s) dans le résultat.", {count:result.files.length}));
    if (typeof result.bucket === "string") messages.push(t("Stockage concerné : {bucket}.", {bucket:result.bucket}));
    if (result.verified === true || result.status === "verified") messages.push(t("Le worker confirme la vérification de l’intégrité."));
    if (result.read_tested === true) messages.push(t("Test de lecture effectué."));
    if (result.write_tested === false) messages.push(t("L’écriture n’a pas été testée."));
    if (result.status === "request_accepted") messages.push(t("La demande a été acceptée par le fournisseur ; son traitement peut se poursuivre."));
  }
  if (text) messages.push(text);
  const state = ({ completed: t("Opération terminée"), failed: t("Opération en échec"), running: t("Opération en cours"), queued: t("En attente de traitement") } as Record<string, string>)[job.status] ?? t("État mis à jour");
  const Icon = job.status === "completed" ? CircleCheck : job.status === "failed" ? CircleAlert : job.status === "running" ? LoaderCircle : Clock3;
  const color = job.status === "failed" ? "text-destructive" : job.status === "completed" ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground";
  if (compact) return <div className="min-w-0 space-y-5 text-sm leading-relaxed">
    {!hideMessages && (messages.length ? <ul className={job.status === 'failed' ? 'space-y-2 text-destructive' : 'space-y-2'}>{messages.map((message,index)=><li key={index} className="whitespace-pre-wrap [overflow-wrap:anywhere]">{message}</li>)}</ul> : <p className="text-muted-foreground">{t("Aucun compte rendu détaillé n’a été enregistré.")}</p>)}
    <details className="border-t pt-3"><summary className="cursor-pointer text-sm text-muted-foreground">{t("Informations techniques")}</summary><div className="mt-4 space-y-4">{metadata}{value !== null && <Value value={value}/>}</div></details>
  </div>;
  return <div className="min-w-0 space-y-4 text-sm leading-relaxed">
    <ol aria-label={t("Journal de l’opération")} className="space-y-5 border-l-2 border-border pl-4">
      <li className="space-y-1">
        <time dateTime={job.created_at} className="text-xs text-muted-foreground">{formatDate(job.created_at)}</time>
        <p className="font-medium">{t("Opération enregistrée")}</p>
        <p className="break-words text-muted-foreground">{t(kindLabels[job.kind] ?? job.kind)} · {job.subject}</p>
      </li>
      <li className="space-y-2">
        <time dateTime={job.updated_at} className="text-xs text-muted-foreground">{t("Dernière mise à jour : {date}", {date:formatDate(job.updated_at)})}</time>
        <p className={`flex items-center gap-2 font-medium ${color}`}><Icon className="size-4 shrink-0" />{state}</p>
        {messages.length ? <ul className="space-y-2">{messages.map((message, index) => <li key={index} className="whitespace-pre-wrap [overflow-wrap:anywhere]">{message}</li>)}</ul> : <p className="text-muted-foreground">{job.status === "running" || job.status === "queued" ? t("Le compte rendu sera disponible à la fin du traitement.") : t("Aucun compte rendu détaillé n’a été enregistré.")}</p>}
      </li>
    </ol>
    <p className="text-xs text-muted-foreground">{t("Ce journal présente les informations enregistrées, sans le détail des étapes intermédiaires.")}</p>
    {result || Array.isArray(value) ? <details className="rounded-lg border p-3"><summary className="cursor-pointer text-sm font-medium">{t("Informations techniques")}</summary><div className="mt-3"><Value value={value} /></div></details> : null}
  </div>;
}
