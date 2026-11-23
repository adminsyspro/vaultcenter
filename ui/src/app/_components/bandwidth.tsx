"use client";
import { translate as t, useI18n } from "@/lib/i18n";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { request } from "@/lib/api";
import { bandwidthStatus, megabytes, type BandwidthStatus } from "@/lib/bandwidth";

export function BandwidthSettings({ vtlId, source: knownSource, collapsible = false }: { vtlId: string; source?: string; collapsible?: boolean }) {
  useI18n();
  // Neutral wording until the client type of the VTL is known.
  const source = knownSource ?? t("Sauvegarde");
  const prefix = useId(), alive = useRef(true), locked = useRef(false), current = useRef<BandwidthStatus | null>(null), editRevision = useRef<number | null>(null);
  const [status, setStatus] = useState<BandwidthStatus | null>(null), [error, setError] = useState(""), [busy, setBusy] = useState(false);
  const [tapeOn, setTapeOn] = useState(false), [s3On, setS3On] = useState(false);
  const [tape, setTape] = useState("100"), [s3, setS3] = useState("100"), [saved, setSaved] = useState(false);
  const endpoint = `/v1/vtls/${encodeURIComponent(vtlId)}/bandwidth`;
  const accept = useCallback((value: unknown, replaceFields: boolean) => {
    const fresh = bandwidthStatus(value, vtlId);
    if (!alive.current || current.current && fresh.revision < current.current.revision) return;
    current.current = fresh; setStatus(fresh);
    if (replaceFields) {
      editRevision.current = fresh.revision;
      setTapeOn(fresh.tape_write_bps !== null); setS3On(fresh.s3_bps !== null);
      setTape(String((fresh.tape_write_bps ?? 100_000_000) / 1_000_000));
      setS3(String((fresh.s3_bps ?? 100_000_000) / 1_000_000));
    }
  }, [vtlId]);
  const reload = useCallback(async (replaceFields: boolean) => {
    try { accept(await request<unknown>(endpoint), replaceFields); if (alive.current && replaceFields) setError(""); }
    catch (cause) { if (alive.current) setError(cause instanceof Error ? cause.message : t("Lecture des limites indisponible.")); }
  }, [endpoint, accept]);
  useEffect(() => { alive.current = true; void reload(true); return () => { alive.current = false; }; }, [reload]);
  useEffect(() => {
    if (status?.tape.state !== "pending" || busy) return;
    const timer = setTimeout(() => void reload(false), 2000);
    return () => clearTimeout(timer);
  }, [status, busy, reload]);
  async function save(event: React.FormEvent) {
    event.preventDefault(); if (locked.current || !current.current) return;
    locked.current = true; setBusy(true); setError(""); setSaved(false);
    try {
      const body = {revision: editRevision.current, tape_write_bps: tapeOn ? megabytes(tape) : null, s3_bps: s3On ? megabytes(s3) : null};
      const value = await request<unknown>(endpoint, {method: "PUT", headers: {"Content-Type": "application/json"}, body: JSON.stringify(body)});
      accept(value, true); if (alive.current) setSaved(true);
    } catch (cause) { if (alive.current) setError(cause instanceof Error ? cause.message : t("Enregistrement non confirmé. Rechargez les réglages.")); }
    finally { locked.current = false; if (alive.current) setBusy(false); }
  }
  const title = <h3 id={`${prefix}-title`} className="font-medium">{t("Limites de débit")}</h3>;
  const body = <>
    <form onSubmit={event => void save(event)} className="space-y-4">
      <fieldset disabled={busy || !status} className="grid min-w-0 gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <label className="flex items-center gap-2 text-sm font-medium"><input type="checkbox" aria-label={t("Limiter {source} → bandes", {source})} checked={tapeOn} onChange={e => { setTapeOn(e.target.checked); setSaved(false); }} className="size-4 accent-primary"/>{t("{source} → bandes", {source})}</label>
          {tapeOn ? <><Label htmlFor={`${prefix}-tape`}>{t("Limite {source} → bandes (Mo/s)", {source})}</Label><Input id={`${prefix}-tape`} type="number" min="1" max="100000" step="1" required value={tape} onChange={e => { setTape(e.target.value); setSaved(false); }}/></> : <p className="text-sm text-muted-foreground">{t("Sans limite")}</p>}
          <p className="text-xs text-muted-foreground">{t("Écritures uniquement ; les restaurations gardent leur débit libre.")}</p>
        </div>
        <div className="space-y-2">
          <label className="flex items-center gap-2 text-sm font-medium"><input type="checkbox" aria-label={t("Limiter S3")} checked={s3On} onChange={e => { setS3On(e.target.checked); setSaved(false); }} className="size-4 accent-primary"/>{t("S3")}</label>
          {s3On ? <><Label htmlFor={`${prefix}-s3`}>{t("Limite S3 (Mo/s)")}</Label><Input id={`${prefix}-s3`} type="number" min="1" max="100000" step="1" required value={s3} onChange={e => { setS3(e.target.value); setSaved(false); }}/></> : <p className="text-sm text-muted-foreground">{t("Sans limite")}</p>}
          <p className="text-xs text-muted-foreground">{t("Budget partagé entre envois, téléchargements et vérifications.")}</p>
        </div>
      </fieldset>
      {error && <Alert variant="destructive"><AlertDescription>{t(error)}</AlertDescription></Alert>}
      {status && (saved || status.tape.state !== "unavailable" || status.s3.state === "degraded") && <p role="status" className="text-sm text-muted-foreground">{saved ? t("Réglages enregistrés. ") : ""}{status.tape.state === "confirmed" ? t("Limite confirmée par le lecteur.") : status.tape.state === "stopped" ? t("Limite appliquée au prochain démarrage du lecteur.") : status.tape.state === "pending" ? t("En attente de confirmation du lecteur.") : (saved ? t("Confirmation du lecteur indisponible.") : "")}{status.s3.state === "degraded" ? t(" Le réglage S3 nécessite une vérification.") : ""}</p>}
      <div className="flex flex-wrap justify-end gap-2"><Button type="button" variant="outline" disabled={busy} onClick={() => void reload(true)}>{t("Recharger les réglages")}</Button><Button type="submit" disabled={busy || !status}>{busy ? t("Enregistrement…") : t("Enregistrer les limites")}</Button></div>
    </form>
  </>;
  return collapsible
    ? <details className="group border-t pt-4" aria-labelledby={`${prefix}-title`}><summary className="cursor-pointer list-none [&::-webkit-details-marker]:hidden"><span className="inline-flex items-center gap-2">{title}<span aria-hidden="true" className="text-muted-foreground transition-transform group-open:rotate-90">›</span></span></summary><div className="mt-4 space-y-4">{body}</div></details>
    : <section className="space-y-4 border-t pt-4" aria-labelledby={`${prefix}-title`}>{title}{body}</section>;
}
