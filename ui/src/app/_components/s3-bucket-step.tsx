"use client";
import { useEffect, useRef, useState } from "react";
import { CircleCheck, LoaderCircle } from "lucide-react";
import { translate as t, useI18n } from "@/lib/i18n";
import type { Job } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { useScopedRequest } from "./vtl-scope";
import { MANAGED_BUCKET_PREFIX, ManagedBucketInput, managedBucketSuffix, validManagedBucketSuffix } from "./managed-bucket-input";
import { S3Schedule } from "./s3-schedule";

export type Creation = { job_id: string; bucket: string; phase: string; message: string | null };
export type Discovery = {
  buckets: { name: string; attached: boolean; active: boolean; destination_id: string | null }[];
  creations?: Creation[]; complete: boolean; error: string | null; revision: number; suggested_bucket: string; creation_supported: boolean;
};

/**
 * Step ②: the export bucket; « Créer » or « Changer » sits inside its field. A name the
 * user typed is never replaced: the suggestion fills the field only while it is untouched,
 * and after a creation succeeds it is refilled only if nobody typed since.
 */
export function S3BucketStep({ ready, keysRefused, connected, activeBucket, activeCopies, discovery, error, jobs, onChanged }: {
  ready: boolean; keysRefused: boolean; connected: boolean; activeBucket: string | null; activeCopies: number | null | undefined; discovery: Discovery | null; error: string; jobs: Job[]; onChanged: () => Promise<void>;
}) {
  useI18n();
  const request = useScopedRequest();
  const [next, setNext] = useState(""), [busy, setBusy] = useState(false), [failure, setFailure] = useState("");
  const [dismissed, setDismissed] = useState<string[]>([]), [created, setCreated] = useState<string | null>(null);
  // edited: the user typed since the field was last filled; submitted: the bucket this step asked for.
  const pending = useRef(false), edited = useRef(false), submitted = useRef<string | null>(null);
  useEffect(() => {
    if (!discovery) return;
    const current = discovery.creations?.[0];
    // The name being created stays in the field until the provider settles it.
    if (current && ["reserved", "creating", "uncertain"].includes(current.phase)) return;
    if (current?.phase === "switched" && current.bucket === submitted.current) { submitted.current = null; edited.current = false; setCreated(current.bucket); }
    if (!edited.current) setNext(managedBucketSuffix(discovery.suggested_bucket));
  }, [discovery]);
  const latest = discovery?.creations?.[0];
  const job = latest ? jobs.find(item => item.id === latest.job_id) : undefined;
  const creating = !!latest && ["reserved", "creating"].includes(latest.phase);
  const checking = latest?.phase === "uncertain";
  const occupied = latest?.phase === "occupied" && !dismissed.includes(latest.job_id) ? latest : null;
  const refused = latest?.phase === "abandoned" && !dismissed.includes(latest.job_id) ? job?.message ?? latest.message : null;
  // An active bucket without any copy yet is where the next exports go: nothing to change.
  // Until they are counted (undefined), the bucket might be empty: no change is offered either.
  const activeEmpty = !!activeBucket && activeCopies === 0 && !creating && !checking;
  const counting = !!activeBucket && activeCopies === undefined && !creating && !checking;
  const blocked = !ready || keysRefused || !connected || busy || creating || checking;
  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!discovery || pending.current || blocked || activeEmpty || counting || !validManagedBucketSuffix(next)) return;
    pending.current = true; setBusy(true); setFailure("");
    try {
      await request("/v1/storage/buckets/create", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ revision: discovery.revision, bucket: MANAGED_BUCKET_PREFIX + next }) });
      submitted.current = MANAGED_BUCKET_PREFIX + next; setCreated(null);
      if (latest) setDismissed(current => [...current, latest.job_id]);
      await onChanged();
    } catch (cause) { setFailure(cause instanceof Error ? cause.message : t("Création non confirmée.")); }
    finally { pending.current = false; setBusy(false); }
  }
  function otherName() {
    if (occupied) setDismissed(current => [...current, occupied.job_id]);
    edited.current = false; submitted.current = null;
    void onChanged();
  }
  return <section aria-label={t("Bucket des exports")} className={`space-y-4 rounded-xl border p-5 ${ready && !keysRefused ? "" : "bg-muted/30"}`}>
    <h3 className="flex items-center gap-2 font-semibold"><span aria-hidden="true" className="inline-flex size-6 items-center justify-center rounded-full border text-xs tabular-nums">2</span>{t("Bucket des exports")}</h3>
    {!ready ? <p className="text-sm text-muted-foreground">{t("Enregistrez d’abord la connexion S3.")}</p> : <>
      {activeBucket && <p className="flex flex-wrap items-center gap-2 text-sm text-emerald-700 dark:text-emerald-300"><CircleCheck aria-hidden="true" className="size-4" />{t("Bucket actif : {bucket}", { bucket: activeBucket })}</p>}
      {created && created === activeBucket && <p role="status" className="text-sm text-emerald-700 dark:text-emerald-300">{t("Le bucket {bucket} a été créé et activé.", { bucket: created })}</p>}
      {discovery && !discovery.creation_supported
        ? <p className="text-sm text-muted-foreground">{t("Ce fournisseur ne permet pas de créer un bucket depuis VaultCenter. Utilisez un bucket existant dans les options avancées.")}</p>
        : <form className="max-w-xl space-y-2" onSubmit={event => void submit(event)}>
          {counting ? <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle aria-hidden="true" className="size-4 motion-safe:animate-spin" />{t("Lecture des copies du bucket…")}</p>
            : activeEmpty ? <p className="text-sm text-muted-foreground">{t("Le bucket actif est encore vide : les prochains exports iront dedans.")}</p> : <>
            <Label htmlFor="export-bucket">{t(activeBucket ? "Prochain bucket" : "Nom du bucket")}</Label>
            <ManagedBucketInput id="export-bucket" value={next} disabled={blocked || !discovery} required aria-invalid={!!next && !validManagedBucketSuffix(next)} onChange={event => { edited.current = true; setCreated(null); setNext(event.target.value); setFailure(""); }}
              action={<Button type="submit" size="sm" className="absolute right-0.5 top-0.5 h-7" disabled={blocked || !discovery || !validManagedBucketSuffix(next)}>{busy && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}{t(activeBucket ? "Changer" : "Créer")}</Button>} />
          </>}
          {creating && <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle aria-hidden="true" className="size-4 motion-safe:animate-spin" />{t("Création de {bucket} en cours.", { bucket: latest?.bucket ?? "" })}</p>}
          {keysRefused && <p role="status" className="text-sm text-muted-foreground">{t("Corrigez d’abord les clés de la connexion.")}</p>}
          {checking && !keysRefused && <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle aria-hidden="true" className="size-4 motion-safe:animate-spin" />{t("Vérification de la création en cours.")}</p>}
          {occupied && <p role="alert" className="flex flex-wrap items-center gap-x-3 text-sm text-destructive">{t("Le bucket {bucket} existe déjà et contient des données.", { bucket: occupied.bucket })}<Button type="button" variant="link" size="sm" className="h-auto p-0" onClick={otherName}>{t("Utiliser un autre nom")}</Button></p>}
          {refused && <p role="alert" className="text-sm text-destructive">{t(refused)}</p>}
          {(failure || error) && <p role="alert" className="text-sm text-destructive">{t(failure || error)}</p>}
        </form>}
      <S3Schedule />
    </>}
  </section>;
}
