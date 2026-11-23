"use client";
import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { Download, Search } from "lucide-react";
import { translate as t, getLocale, useI18n } from "@/lib/i18n";
import { type Job, formatBytes } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectOption } from "@/components/ui/select";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { StorageAccessStatus } from "./storage-copy-status";
import { VtlScope, scopedPath, useScopedRequest } from "./vtl-scope";

type BucketState = "available" | "archiving" | "archived" | "recalling" | "recalled" | "unknown";
export type HistoryRow = {
  barcode: string; generation: number; bytes: number | null; exported_at: string; destination_id: string; bucket: string | null;
  removed_at: string | null; removal: "s3_deleted" | "catalogue" | null; bucket_state: BucketState;
  archived_at: string | null; recalled_at: string | null; readable_until: string | null; local_deleted_at: string | null;
};
type Filter = "all" | "cold" | "available" | "removed";
const cold: BucketState[] = ["archiving", "archived", "recalling", "recalled"];
// The bucket badge reuses the S3 access labels: the catalogue state maps onto OVH's.
const access: Record<BucketState, { raw_status: string | null; readable: boolean; writable: boolean }> = {
  available: { raw_status: null, readable: true, writable: true }, archiving: { raw_status: "Archiving", readable: false, writable: false },
  archived: { raw_status: "Archived", readable: false, writable: false }, recalling: { raw_status: "Restoring", readable: false, writable: false },
  recalled: { raw_status: "Restored", readable: true, writable: false }, unknown: { raw_status: null, readable: false, writable: false },
};
/** A register keeps the year: SQLite dates are UTC without a zone suffix. */
function day(value: string) {
  const date = new Date(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value) ? value.replace(" ", "T") + "Z" : value);
  return Number.isNaN(date.getTime()) ? value
    : new Intl.DateTimeFormat(getLocale(), { day: "2-digit", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
}
function matches(row: HistoryRow, filter: Filter) {
  if (filter === "removed") return !!row.removed_at;
  if (row.removed_at) return filter === "all";
  return filter === "all" || (filter === "cold" ? cold.includes(row.bucket_state) : row.bucket_state === "available");
}

/** Dated register of the VTL's S3 copies, read from the catalogue only. */
export function StorageHistory({ jobs, onBucket }: { jobs: Job[]; onBucket: (id: string, name: string) => void }) {
  useI18n();
  const request = useScopedRequest();
  const vtl = useContext(VtlScope);
  const [rows, setRows] = useState<HistoryRow[]>([]);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const sequence = useRef(0);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    const current = ++sequence.current;
    setBusy(true); setError("");
    try {
      const value = await request<HistoryRow[]>("/v1/storage/history", { signal });
      if (!Array.isArray(value)) throw new Error(t("Historique invalide."));
      if (current === sequence.current) setRows(value);
    } catch (cause) {
      if (!signal?.aborted && current === sequence.current) { setRows([]); setError(cause instanceof Error ? cause.message : t("Historique indisponible.")); }
    } finally { if (!signal?.aborted && current === sequence.current) setBusy(false); }
  }, [request]);
  const revision = jobs.filter(job => /^(export|archive_|recall_|s3_delete)/.test(job.kind)).map(job => `${job.id}:${job.status}`).join(",");
  useEffect(() => { const controller = new AbortController(); void refresh(controller.signal); return () => controller.abort(); }, [refresh, revision]);
  const query = search.trim().toLowerCase();
  const visible = rows.filter(row => matches(row, filter) && (!query || row.barcode.toLowerCase().includes(query) || (row.bucket ?? "").toLowerCase().includes(query)));
  return <section aria-label={t("Historique des copies")} className="space-y-4">
    <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
      <div className="min-w-0 flex-1 space-y-2"><Label htmlFor="history-search">{t("Rechercher une cartouche ou un bucket")}</Label><div className="relative"><Search className="pointer-events-none absolute top-2.5 left-3 size-4 text-muted-foreground" aria-hidden="true" /><Input id="history-search" className="bg-card dark:bg-card pl-9" value={search} onChange={event => setSearch(event.target.value)} placeholder={t("Code-barres ou bucket")} /></div></div>
      <div className="space-y-2 sm:w-56"><Label htmlFor="history-filter">{t("Copies")}</Label><Select id="history-filter" value={filter} onValueChange={value => setFilter(value as Filter)}>
        <SelectOption value="all">{t("Toutes")}</SelectOption><SelectOption value="cold">{t("En Cold Archive")}</SelectOption>
        <SelectOption value="available">{t("Disponibles sur S3")}</SelectOption><SelectOption value="removed">{t("Retirées")}</SelectOption>
      </Select></div>
      {vtl && <Button asChild variant="outline"><a href={scopedPath("/v1/storage/history?format=csv", vtl)} download><Download aria-hidden="true" />{t("Exporter CSV")}</a></Button>}
    </div>
    {error && <Alert variant="destructive"><AlertDescription className="flex flex-wrap items-center gap-3">{t(error)}<Button size="sm" variant="outline" onClick={() => void refresh()}>{t("Réessayer")}</Button></AlertDescription></Alert>}
    <div className="vc-surface" aria-busy={busy}><div className="overflow-x-auto"><table className="w-full min-w-[760px] text-left text-sm"><caption className="sr-only">{t("Historique des copies S3 par cartouche")}</caption>
      <thead className="border-b bg-muted/40"><tr>{["Code-barres", "Génération", "Bucket", "Exportée le", "État", "Suppression locale"].map(label => <th key={label} scope="col" className="whitespace-nowrap px-4 py-3 font-medium">{t(label)}</th>)}</tr></thead>
      <tbody>{visible.map(row => <tr key={`${row.destination_id}:${row.barcode}:${row.generation}`} className={`border-b last:border-0 ${row.removed_at ? "text-muted-foreground" : ""}`}>
        <th scope="row" className="px-4 py-3 font-mono font-medium">{row.barcode}</th>
        <td className="px-4 py-3 tabular-nums">{row.generation}</td>
        <td className="max-w-64 px-4 py-3">{row.bucket ? <Button variant="link" className="h-auto break-all p-0 text-left font-mono font-normal whitespace-normal" onClick={() => onBucket(row.destination_id, row.bucket!)}>{row.bucket}</Button> : "—"}</td>
        <td className="whitespace-nowrap px-4 py-3 tabular-nums">{day(row.exported_at)}{row.bytes != null && <p className="text-xs text-muted-foreground">{formatBytes(row.bytes)}</p>}</td>
        <td className="px-4 py-3">{row.removed_at
          ? t(row.removal === "s3_deleted" ? "Supprimée de S3 le {date}" : "Retirée du catalogue le {date}", { date: day(row.removed_at) })
          : <div className="space-y-1"><StorageAccessStatus access={access[row.bucket_state]} />
            {row.bucket_state === "archived" && row.archived_at && <p className="text-xs text-muted-foreground">{t("archivé le {date}", { date: day(row.archived_at) })}</p>}
            {row.bucket_state === "recalled" && row.readable_until && <p className="text-xs text-muted-foreground">{t("rappelé, lisible jusqu’au {date}", { date: day(row.readable_until) })}</p>}
          </div>}</td>
        <td className="whitespace-nowrap px-4 py-3 tabular-nums">{row.local_deleted_at ? day(row.local_deleted_at) : "—"}</td>
      </tr>)}
      {!visible.length && !busy && !error && <tr><td colSpan={6} className="px-5 py-12 text-center text-muted-foreground">{t(rows.length ? "Aucune copie ne correspond aux filtres." : "Aucune copie enregistrée pour cette VTL.")}</td></tr>}</tbody></table></div></div>
    <p className="text-xs text-muted-foreground">{t("Dernier état connu. L’état en direct est dans la fenêtre du bucket.")}</p>
  </section>;
}
