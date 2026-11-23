"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useEffect, useRef, useState } from "react";
import { Select, SelectOption } from "@/components/ui/select";
import { Label } from "@/components/ui/label";
import { useScopedRequest } from "./vtl-scope";

export interface StorageBucket {
  id: string;
  bucket: string;
  active: boolean;
  created_at: string;
  configuration_revision: number;
  copy_count: number;
  observed_status: string | null;
  rotation_phase: string | null;
}
export function bucketPath(path: string, id: string) {
  return `${path}${path.includes("?") ? "&" : "?"}destination_id=${encodeURIComponent(id)}`;
}
export function bucketLink(page: string, id: string) {
  const query = new URLSearchParams(location.hash.split("?")[1] ?? "");
  query.set("bucket", id);
  return `#${page}?${query}`;
}
export function useBucketSelection(revision: string, followActive = false) {
  const request = useScopedRequest();
  const [buckets, setBuckets] = useState<StorageBucket[]>([]);
  const [selectedId, select] = useState(() => new URLSearchParams(typeof location === "undefined" ? "" : location.hash.split("?")[1] ?? "").get("bucket") ?? "");
  const followingActive = useRef(followActive && !selectedId);
  const [error, setError] = useState("");
  useEffect(() => {
    const abort = new AbortController();
    const refresh = () => request<StorageBucket[]>("/v1/storage/buckets", {signal: abort.signal}).then(values => {
      if (abort.signal.aborted) return;
      if (!Array.isArray(values)) throw new Error(t("Liste des buckets invalide."));
      setBuckets(values);
      const fallback = values.find(b => b.active)?.id ?? values.find(b => !["reserved", "creating", "uncertain", "failed"].includes(b.rotation_phase ?? ""))?.id ?? "";
      select(current => followingActive.current ? fallback : current || fallback);
      setError("");
    }).catch(e => { if (!abort.signal.aborted) setError(e.message); });
    void refresh();
    const timer = setInterval(() => { if (!document.hidden) void refresh(); }, 30000);
    return () => { abort.abort(); clearInterval(timer); };
  }, [request, revision]);
  const bucket = buckets.find(b => b.id === selectedId);
  return { buckets, bucket, selectedId, select: (id: string) => { followingActive.current = followActive && !!buckets.find(b => b.id === id)?.active; select(id); }, error: error || (buckets.length && !bucket ? t("Ce bucket n’appartient pas à la VTL sélectionnée.") : "") };
}
const states: Record<string, string> = { Archiving: "Archivage en cours", Archived: "Archivé", Restoring: "Récupération en cours", Restored: "Prêt à restaurer" };
export function BucketSelector({id, buckets, value, onChange, disabled = false}: {
  id: string; buckets: StorageBucket[]; value: string; onChange: (id: string) => void; disabled?: boolean;
}) {
  useI18n();
  return <div className="flex flex-wrap items-center gap-3">
    <Label htmlFor={id}>Bucket</Label>
    <Select id={id} value={value} onValueChange={(selectedValue) => onChange(selectedValue)} disabled={disabled || !buckets.length} className="w-full sm:min-w-72">
      {!buckets.length && <SelectOption value="">{t("Chargement des buckets…")}</SelectOption>}
      {buckets.map(b => <SelectOption key={b.id} value={b.id} disabled={!b.active && ["reserved", "creating", "uncertain", "failed"].includes(b.rotation_phase ?? "")}>
        {b.bucket} · {b.active ? t("Actif — nouveaux exports") : ["reserved", "creating", "uncertain", "failed"].includes(b.rotation_phase ?? "") ? t("Création non confirmée") : t(states[b.observed_status ?? ""] ?? "Ancien bucket")}
      </SelectOption>)}
    </Select>
  </div>;
}
