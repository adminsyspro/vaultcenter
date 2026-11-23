"use client";
import { useEffect, useRef, useState } from "react";
import { translate as t, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectOption } from "@/components/ui/select";
import { useScopedRequest } from "./vtl-scope";

type Rotation = "manual" | "weekly" | "monthly";
export type Schedule = {
  revision: number; rotation: Rotation; weekday: number | null; month_day: number | null;
  archive_after_days: number | null; archive_terms_accepted: boolean; rotation_supported: boolean;
  rotation_due: boolean; period_start: string | null; active_bucket: string | null;
};
type Draft = { rotation: Rotation; weekday: number; month_day: number; archive: boolean; days: number };
const weekdays = ["Lundi", "Mardi", "Mercredi", "Jeudi", "Vendredi", "Samedi", "Dimanche"];
const draftOf = (value: Schedule): Draft => ({ rotation: value.rotation, weekday: value.weekday ?? 1, month_day: value.month_day ?? 1, archive: value.archive_after_days !== null, days: value.archive_after_days ?? 7 });

/** Bucket change and automatic archiving, saved as soon as a choice changes. */
export function S3Schedule() {
  useI18n();
  const request = useScopedRequest();
  const [saved, setSaved] = useState<Schedule | null>(null), [draft, setDraft] = useState<Draft | null>(null);
  const [terms, setTerms] = useState(false), [error, setError] = useState(""), [busy, setBusy] = useState(false), [done, setDone] = useState(false);
  const pending = useRef(false);
  useEffect(() => {
    const controller = new AbortController();
    void request<Schedule>("/v1/storage/schedule", { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) { setSaved(value); setDraft(draftOf(value)); } })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Planification indisponible.")); });
    return () => controller.abort();
  }, [request]);
  /** A rotation choice never carries an archiving choice that is still waiting for the OVH conditions. */
  function rotate(patch: Partial<Draft>) {
    if (!saved || !draft) return;
    setTerms(false);
    void save({ ...draft, archive: saved.archive_after_days !== null, days: saved.archive_after_days ?? draft.days, ...patch });
  }
  async function save(next: Draft, accept = false) {
    if (!saved || pending.current) return;
    setDraft(next);
    if (next.archive && !(Number.isInteger(next.days) && next.days >= 1 && next.days <= 365)) { setError(t("Choisissez un délai de 1 à 365 jours.")); return; }
    // Enabling automatic archiving shows the OVH conditions; accepting them enables it.
    if (next.archive && !saved.archive_terms_accepted && !accept) { setTerms(true); return; }
    pending.current = true; setBusy(true); setError(""); setDone(false);
    try {
      const value = await request<Schedule>("/v1/storage/schedule", {
        method: "PUT", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          revision: saved.revision, rotation: next.rotation,
          ...(next.rotation === "weekly" ? { weekday: next.weekday } : {}),
          ...(next.rotation === "monthly" ? { month_day: next.month_day } : {}),
          ...(next.archive ? { archive_after_days: next.days } : {}),
          accept_archive_terms: accept,
        }),
      });
      setSaved(value); setDraft(draftOf(value)); setTerms(false); setDone(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("Réglage non enregistré."));
      setDraft(draftOf(saved));
    } finally { pending.current = false; setBusy(false); }
  }
  if (!saved || !draft) return error ? <p role="alert" className="text-sm text-destructive">{t(error)}</p> : null;
  return <div className="space-y-4 border-t pt-4 text-sm">
    <fieldset className="space-y-2" disabled={busy}>
      <legend className="font-medium">{t("Changement de bucket")}</legend>
      <label className="flex items-center gap-2"><input type="radio" name="rotation" checked={draft.rotation === "manual"} onChange={() => void rotate({ rotation: "manual" })} />{t("Manuel uniquement")}</label>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-2"><input type="radio" name="rotation" disabled={!saved.rotation_supported} checked={draft.rotation === "weekly"} onChange={() => void rotate({ rotation: "weekly" })} />{t("Chaque semaine, le")}</label>
        <Select aria-label={t("Jour de la semaine")} className="w-36" value={String(draft.weekday)} disabled={busy || !saved.rotation_supported} onValueChange={value => void rotate({ rotation: "weekly", weekday: Number(value) })}>{weekdays.map((day, index) => <SelectOption key={day} value={String(index + 1)}>{t(day)}</SelectOption>)}</Select>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-2"><input type="radio" name="rotation" disabled={!saved.rotation_supported} checked={draft.rotation === "monthly"} onChange={() => void rotate({ rotation: "monthly" })} />{t("Chaque mois, le")}</label>
        <Select aria-label={t("Jour du mois")} className="w-24" value={String(draft.month_day)} disabled={busy || !saved.rotation_supported} onValueChange={value => void rotate({ rotation: "monthly", month_day: Number(value) })}>{Array.from({ length: 31 }, (_, index) => <SelectOption key={index + 1} value={String(index + 1)}>{String(index + 1)}</SelectOption>)}</Select>
      </div>
      {!saved.rotation_supported && <p className="text-muted-foreground">{t("Le changement planifié nécessite une connexion OVH Cold Archive.")}</p>}
    </fieldset>
    <fieldset className="space-y-2" disabled={busy}>
      <legend className="font-medium">{t("Archivage automatique")}</legend>
      <label className="flex items-center gap-2"><input type="radio" name="archive" checked={!draft.archive} onChange={() => void save({ ...draft, archive: false })} />{t("Jamais")}</label>
      <div className="flex flex-wrap items-center gap-2">
        <input type="radio" name="archive" aria-label={t("Archiver automatiquement")} disabled={!saved.rotation_supported} checked={draft.archive} onChange={() => void save({ ...draft, archive: true })} />
        <Input aria-label={t("Délai en jours")} disabled={!saved.rotation_supported} type="number" min={1} max={365} className="w-20" value={draft.days} onChange={event => setDraft({ ...draft, days: Number(event.target.value) })} onBlur={() => { if (draft.archive && draft.days !== saved.archive_after_days) void save(draft); }} />
        <span>{t("jours après qu’un bucket n’est plus actif")}</span>
      </div>
      {terms && <div role="alert" className="space-y-2 rounded-lg border border-amber-300 p-3 dark:border-amber-500/40">
        <p>{t("Un bucket archivé n’est plus lisible sans rappel. OVH facture au minimum 1 Tio pendant 180 jours, et chaque rappel est payant.")}</p>
        <p>{t("Seuls les buckets qui cesseront d’être actifs à partir de maintenant seront archivés automatiquement.")}</p>
        <div className="flex gap-2"><Button type="button" size="sm" onClick={() => void save(draft, true)}>{t("Accepter et activer")}</Button><Button type="button" size="sm" variant="outline" onClick={() => { setTerms(false); setDraft(draftOf(saved)); }}>{t("Annuler")}</Button></div>
      </div>}
      {!saved.rotation_supported && <p className="text-muted-foreground">{t("L’archivage automatique nécessite une connexion OVH Cold Archive.")}</p>}
    </fieldset>
    {done && <p role="status" className="text-muted-foreground">{t("Réglage enregistré.")}</p>}
    {error && <p role="alert" className="text-destructive">{t(error)}</p>}
  </div>;
}
