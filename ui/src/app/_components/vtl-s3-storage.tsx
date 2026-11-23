"use client";
import { useCallback, useEffect, useRef, useState } from "react";
import { CircleCheck } from "lucide-react";
import { translate as t, useI18n } from "@/lib/i18n";
import { type Job } from "@/lib/api";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { S3Connection, type CredentialStatus } from "./s3-connection";
import { S3BucketStep, type Discovery } from "./s3-bucket-step";
import { S3Advanced } from "./s3-advanced";
import { VtlScope, useScopedRequest } from "./vtl-scope";

/** Stockage › S3: ① connection, ② export bucket and schedule, ③ ready; rare operations folded below. */
export function VtlS3Storage({ vtlId, activeBucket, canManage, connected, onRefresh }: { vtlId: string; activeBucket: string | null; canManage: boolean; connected: boolean; onRefresh: () => Promise<void> }) {
  useI18n();
  if (!canManage) return <Alert><AlertTitle>{t("Accès administrateur requis")}</AlertTitle><AlertDescription>{t("La configuration des identifiants S3 est réservée aux administrateurs.")}</AlertDescription></Alert>;
  return <VtlScope.Provider value={vtlId}><S3Steps vtlId={vtlId} activeBucket={activeBucket} connected={connected} onRefresh={onRefresh} /></VtlScope.Provider>;
}

function S3Steps({ vtlId, activeBucket, connected, onRefresh }: { vtlId: string; activeBucket: string | null; connected: boolean; onRefresh: () => Promise<void> }) {
  useI18n();
  const request = useScopedRequest();
  const [configured, setConfigured] = useState<boolean | null>(null), [keys, setKeys] = useState<CredentialStatus | null>(null);
  const status = useCallback((value: boolean, credentials: CredentialStatus | null) => { setConfigured(value); setKeys(credentials); }, []);
  const refused = keys === "invalid";
  const [jobs, setJobs] = useState<Job[]>([]);
  const [discovery, setDiscovery] = useState<Discovery | null>(null), [error, setError] = useState(""), [jobsError, setJobsError] = useState("");
  const refreshLibraries = useRef(onRefresh);
  refreshLibraries.current = onRefresh;
  // Creation jobs drive the step ② lines (creating, checking, occupied, active).
  const signature = jobs.filter(job => /^(bucket_create|archive_|recall_)/.test(job.kind)).map(job => `${job.id}:${job.status}:${job.message ?? ""}`).join(",");
  useEffect(() => {
    if (!configured || !connected) return;
    const controller = new AbortController();
    const tick = () => { if (!document.hidden) void request<Job[]>("/v1/jobs", { signal: AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]) }).then(value => { if (Array.isArray(value)) { setJobs(value); setJobsError(""); } }).catch(cause => { if (!controller.signal.aborted) setJobsError(cause instanceof Error && !["TimeoutError", "AbortError"].includes(cause.name) ? cause.message : t("Suivi des créations indisponible.")); }); };
    tick(); const timer = setInterval(tick, 5000);
    return () => { controller.abort(); clearInterval(timer); };
  }, [configured, connected, request]);
  const discover = useCallback(async () => {
    try {
      const value = await request<Discovery>("/v1/storage/buckets/discover", { signal: AbortSignal.timeout(45000) });
      if (!Array.isArray(value.buckets)) throw new Error(t("Liste des buckets invalide."));
      setDiscovery(value); setError("");
    } catch (cause) { setError(cause instanceof Error ? cause.message : t("Découverte impossible.")); }
  }, [request]);
  const seen = useRef<string | null>(null);
  useEffect(() => {
    if (!configured || !connected) return;
    if (seen.current !== null && seen.current !== signature) refreshLibraries.current().catch(cause => setError(cause instanceof Error ? cause.message : t("Actualisation impossible.")));
    seen.current = signature;
    void discover();
  }, [configured, connected, signature, discover]);
  const changed = useCallback(async () => {
    const [, refreshed] = await Promise.allSettled([discover(), refreshLibraries.current()]);
    if (refreshed.status === "rejected") setError(refreshed.reason instanceof Error ? refreshed.reason.message : t("Actualisation impossible."));
  }, [discover]);
  // Copies already in the active bucket: step ② offers no change while it is empty.
  // undefined: not read yet for this bucket; null: unknown (refresh failed or bucket not listed).
  const [copies, setCopies] = useState<{ bucket: string; count: number | null } | null>(null), [copiesError, setCopiesError] = useState("");
  const activeCopies = !activeBucket ? null : copies?.bucket === activeBucket ? copies.count : undefined;
  const exported = jobs.filter(job => job.kind === "export" && job.status === "completed").map(job => job.id).join(",");
  useEffect(() => {
    if (!configured || !connected || !activeBucket) return;
    const controller = new AbortController();
    request<{ bucket: string; active: boolean; copy_count: number }[]>("/v1/storage/buckets", { signal: controller.signal })
      .then(values => { if (!Array.isArray(values)) throw new Error(t("Liste des buckets indisponible.")); setCopies({ bucket: activeBucket, count: values.find(value => value.active && value.bucket === activeBucket)?.copy_count ?? null }); setCopiesError(""); })
      .catch(cause => {
        if (controller.signal.aborted) return;
        setCopies({ bucket: activeBucket, count: null });
        setCopiesError(cause instanceof Error ? cause.message : t("Liste des buckets indisponible."));
      });
    return () => controller.abort();
  }, [configured, connected, activeBucket, exported, request]);
  const ready = configured === true && !!activeBucket && !refused;
  return <div className="space-y-5">
    <ol aria-label={t("Configuration S3")} className="space-y-5">
      <li><S3Connection vtlId={vtlId} connected={connected} onStatus={status} onSaved={changed} /></li>
      <li><S3BucketStep ready={configured === true} keysRefused={refused} connected={connected} activeBucket={activeBucket} activeCopies={activeCopies} discovery={discovery} error={error || jobsError || copiesError} jobs={jobs} onChanged={changed} /></li>
      <li><section aria-label={t("Prêt à exporter")} className={`flex items-center gap-2 rounded-xl border p-5 text-sm ${ready ? "border-emerald-600/30 text-emerald-700 dark:text-emerald-300" : "text-muted-foreground"}`}>
        <span aria-hidden="true" className="inline-flex size-6 items-center justify-center rounded-full border text-xs tabular-nums">3</span>
        {ready ? <><CircleCheck aria-hidden="true" className="size-4" />{t("Prêt à exporter vers {bucket}", { bucket: activeBucket ?? "" })}</> : t("Prêt à exporter")}
      </section></li>
    </ol>
    <S3Advanced ready={configured === true} connected={connected} discovery={discovery} onChanged={changed} />
  </div>;
}
