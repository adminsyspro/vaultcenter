import {translate as t,getLocale,getLanguage} from "./i18n";
export interface AuthUser {
  id: string;
  username: string;
  role: "admin" | "viewer";
  issuer: string | null;
  disabled: boolean;
}
export interface Session {
  user: AuthUser;
  csrf_token: string | null;
}
let csrfToken: string | null = null;
export function setCsrfToken(value: string | null) {
  csrfToken = value;
}

/** State of a copy's bucket from its latest OVH archive or recall request (history.rs BucketState). */
export type CopyBucketState = 'available' | 'archiving' | 'archived' | 'recalling' | 'recalled' | 'unknown';

export interface Cartridge {
  /** Synchronisation with the active bucket still to run or running. */
  reconcile_pending?: boolean;
  /** Most recent verified S3 copy, all buckets included. */
  last_copy?: { bucket: string | null; generation: number; checked_at: string; bucket_state?: CopyBucketState;
    /** Logical size of the copy: a larger observed local volume means writes after it. */
    bytes?: number | null } | null;
  local_deleted?: boolean;
  media_type?: string | null;
  segment_size_mib?: number;
  multipart_part_size_mib?: number;
  export_resumable?: boolean;
  barcode: string;
  capacity_bytes: number;
  bytes_written: number;
  generation: number;
  state: string;
}

export interface RemoteGeneration {
  barcode: string;
  generation: number;
  format_version: number;
  logical_size_bytes: number;
  file_count: number;
  segment_count: number;
  local_match: boolean;
  manifest_sha256: string;
}

export interface Job {
  destination_id?: string | null;
  configuration_revision?: number | null;
  id: string;
  kind: string;
  generation?: number | null;
  destination?: string | null;
  subject: string;
  status: string;
  progress: number;
  message: string | null;
  created_at: string;
  updated_at: string;
}

export interface ServiceStatus {
  status: string;
  cartridges: number;
  jobs: number;
  media_root: string;
  version?: string;
  revision?: string;
}

export interface Snapshot {
  status: ServiceStatus;
  cartridges: Cartridge[];
  jobs: Job[];
}

export class ApiError extends Error {
  constructor(message:string, public readonly status:number) { super(message); this.name='ApiError'; }
}

export async function request<T>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const response = await fetch(path, {
    cache: "no-store",
    ...options,
    signal: options.signal ?? AbortSignal.timeout(15000),
    headers: {
      Accept: "application/json",
      ...(csrfToken &&
      options.method &&
      !["GET", "HEAD"].includes(options.method.toUpperCase())
        ? { "X-CSRF-Token": csrfToken }
        : {}),
      ...options.headers,
    },
  });
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    if (
      response.status === 401 &&
      path !== "/auth/login" &&
      path !== "/auth/password"
    )
      window.dispatchEvent(new Event("cold-vtl-unauthorized"));
    throw new ApiError(
      typeof body?.error === "string"
        ? t(body.error)
        : t('Erreur HTTP {status}',{status:response.status}),
      response.status,
    );
  }
  if (body === null) throw new Error(t("Réponse du serveur invalide."));
  return body as T;
}

export async function readSnapshot(signal: AbortSignal, vtlId: string): Promise<Snapshot> {
  const [status, cartridges, jobs] = await Promise.all([
    request<ServiceStatus>(`/v1/vtls/${encodeURIComponent(vtlId)}/status`, { signal }),
    request<Cartridge[]>(`/v1/vtls/${encodeURIComponent(vtlId)}/cartridges`, { signal }),
    request<Job[]>(`/v1/vtls/${encodeURIComponent(vtlId)}/jobs`, { signal }),
  ]);
  if (
    !status ||
    typeof status.status !== "string" ||
    !Array.isArray(cartridges) ||
    !Array.isArray(jobs)
  ) {
    throw new Error(t("Le serveur a renvoyé un inventaire invalide."));
  }
  return { status, cartridges, jobs };
}

export const mediaLabels: Record<string, string> = {
  writable: "Inscriptible",
  sealed: "Prête à envoyer",
  uploading: "Envoi vers S3",
  verified: "Vérifiée",
  archiving: "Archivage",
  archived: "Archivée",
  restoring: "Restauration",
  hydrating: "Hydratation",
  online: "En ligne",
  failed: "Échec",
};
export const jobLabels: Record<string, string> = {
  queued: "En attente",
  running: "En cours",
  completed: "Terminé",
  failed: "Échec",
};
export const kindLabels: Record<string, string> = {
  archive_submit: "Demande d’archivage OVH",
  archive_refresh: "Observation OVH",
  recall_submit: "Demande de rappel OVH",
  reintegrate: "Réintégration PBS",
  rollback: "Retour à la cartouche précédente",
  export: "Envoi vers S3",
  restore: "Téléchargement depuis S3",
  s3_delete: "Suppression de copie S3",
  recovery: "Récupération depuis S3",
  recovery_install: "Installation dans la bibliothèque",
  reconcile: "Recherche de copies S3",
  bucket_create: "Création du bucket",
};
export const isActive = (job: Job) =>
  ["queued", "running"].includes(job.status);

export function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) return getLanguage()==='en'?'0 B':'0 o';
  const rank = Math.min(Math.floor(Math.log(value) / Math.log(1024)), 4);
  return `${new Intl.NumberFormat(getLocale(), { maximumFractionDigits: rank > 1 ? 2 : 0 }).format(value / 1024 ** rank)} ${(getLanguage()==='en'?["B", "KiB", "MiB", "GiB", "TiB"]:["o", "Kio", "Mio", "Gio", "Tio"])[rank]}`;
}

export function formatDate(value: string) {
  // SQLite CURRENT_TIMESTAMP is UTC, without an explicit timezone suffix.
  const date = new Date(
    /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value)
      ? value.replace(" ", "T") + "Z"
      : value,
  );
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(getLocale(), {
        day: "2-digit",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
      }).format(date);
}

export function jobSummary(job: Job) {
  if(job.kind==='s3_delete'){
    if(job.status==='completed')return t('Copie S3 supprimée. Les données locales sont conservées.');
    if(job.status==='running')return t('Suppression des objets de la copie S3 en cours.');
  }
  if (['recovery','recovery_install'].includes(job.kind)) {
    if(job.status==='completed')return t('Cartouche récupérée depuis S3 et réintégrée dans la bibliothèque.');
    if(job.status==='running')return t(job.kind==='recovery_install'||job.message?.includes('installing')?'Installation dans la bibliothèque':'Téléchargement et vérification depuis S3 en cours.');
  }

  if(job.status==='running') {
    const stages:Record<string,string> = {
      reconcile:'Recherche et vérification des copies S3 en cours.',
      restore:'Téléchargement et vérification depuis S3 en cours.',
      reintegrate:'Installation dans la bibliothèque',
      rollback:'Rétablissement de la cartouche précédente en cours.',
      bucket_create:'Création et vérification du nouveau bucket en cours.',
    };
    if(stages[job.kind])return t(stages[job.kind]);
  }
  if (/^(archive|recall)_/.test(job.kind)) {
    if (job.status === "completed") {
      if (job.kind.endsWith("_submit")) return t("Demande acceptée par OVH. Le suivi continue dans la fenêtre du bucket.");
      return t("État OVH observé et suivi mis à jour.");
    }
    if (job.status === "running") return t("Vérification des copies puis demande à OVH en cours.");
  }
  if (job.status === "completed" && job.kind === "reintegrate")
    return t("Cartouche réintégrée, services repris et présence vérifiée dans PBS. La cartouche précédente est conservée.");
  if (job.status === "completed" && job.kind === "rollback")
    return t("Cartouche précédente rétablie et accès PBS vérifié.");
  if (job.status === "completed" && job.kind === "restore")
    return t("Copie restaurée et intégrité vérifiée. Le dossier est prêt pour une réintégration contrôlée dans la VTL.");
  if (job.status === "completed" && job.kind === "reconcile") {
    try {
      const result = JSON.parse(job.message ?? "null");
      if (Array.isArray(result?.generations) && result.generations.length === 0)
        return t("Aucune copie S3 trouvée. Aucun fichier n’a été envoyé.");
    } catch { /* Keep the operation's purpose clear even for older results. */ }
    return t("Recherche des copies S3 terminée. Aucun fichier n’a été envoyé.");
  }
  if (job.status === "completed" && job.kind === "export") return t("Copie S3 vérifiée.");
  if (job.status === "completed") return t("Opération terminée avec succès.");
  if (job.status === "running") return t("Transfert et vérification en cours.");
  if (job.status === "queued") return t("En attente de traitement.");
  if (job.message) {
    try {
      const result: unknown = JSON.parse(job.message);
      if (result && typeof result === "object") return t("Consultez le résultat de l’opération ci-dessous.");
    } catch { /* The worker can also return a plain-text error. */ }
  }
  return (job.message ? t(job.message) : "") || t("L’opération a échoué. Consultez les détails.");
}
