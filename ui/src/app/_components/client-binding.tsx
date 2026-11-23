"use client";
import { translate as t, useI18n } from "@/lib/i18n";
import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { request } from "@/lib/api";
import { clientBinding, type ClientBindingInfo } from "@/lib/client-binding";

export function ClientBinding({ vtlId, onChange, onCancel }: { vtlId: string; onChange?: (info: ClientBindingInfo, saved: boolean) => void; onCancel?: () => void }) {
  useI18n();
  const fieldId = useId(), alive = useRef(true), locked = useRef(false), generation = useRef(0);
  const [info, setInfo] = useState<ClientBindingInfo | null>(null);
  const [value, setValue] = useState(""), [error, setError] = useState(""), [busy, setBusy] = useState(false);
  const endpoint = `/v1/vtls/${encodeURIComponent(vtlId)}/client`;
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => {
    const controller = new AbortController(), token = ++generation.current; setInfo(null); setValue(""); setError("");
    void request<unknown>(endpoint, {signal: controller.signal}).then(result => {
      const fresh = clientBinding(result, vtlId);
      if (!controller.signal.aborted && generation.current === token) { setInfo(fresh); setValue(fresh.initiator ?? ""); onChange?.(fresh, false); }
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Association du client indisponible.")); });
    return () => { controller.abort(); ++generation.current; };
  // eslint-disable-next-line react-hooks/exhaustive-deps -- notify once per load, not on parent re-render
  }, [endpoint, vtlId]);
  async function save(event: React.FormEvent) {
    event.preventDefault(); if (!info || !info.editable || locked.current || !value.trim()) return;
    locked.current = true; const token = ++generation.current; setBusy(true); setError("");
    try {
      const fresh = clientBinding(await request<unknown>(endpoint, {method: "PUT", headers: {"Content-Type": "application/json"}, body: JSON.stringify({revision: info.revision, client: "veeam", initiator: value.trim()})}), vtlId);
      if (alive.current && generation.current === token) { setInfo(fresh); setValue(fresh.initiator ?? ""); }
      window.dispatchEvent(new Event("cold-vtl-libraries-changed"));
      onChange?.(fresh, true);
    } catch (cause) { if (alive.current) setError(cause instanceof Error ? cause.message : t("Enregistrement non confirmé. Rouvrez cette fenêtre.")); }
    finally { locked.current = false; if (alive.current) setBusy(false); }
  }
  const changed = !!info && value.trim() !== (info.initiator ?? "");
  return <section className="space-y-2" aria-label={t("Client de sauvegarde")}>
    {info && !info.editable ? <p className="font-mono text-xs">{info.initiator ?? "—"}</p>
      : info && <form onSubmit={event => void save(event)} className="space-y-2">
        <Label htmlFor={fieldId} className="sr-only">{t("Adresse IP du serveur Veeam")}</Label>
        <div className="relative"><Input id={fieldId} required disabled={busy} className="pr-28" placeholder={t("Ex. 192.168.1.50")} value={value} onChange={event => setValue(event.target.value)}/>
        <Button type="submit" size="sm" className="absolute right-0.5 top-0.5 h-7" disabled={busy || !value.trim() || !changed}>{busy ? t("Enregistrement…") : t("Enregistrer")}</Button></div>
        {onCancel && <Button type="button" variant="link" size="sm" className="h-auto p-0" disabled={busy} onClick={onCancel}>{t("Annuler")}</Button>}
      </form>}
    {error && <Alert variant="destructive"><AlertDescription>{t(error)}</AlertDescription></Alert>}
  </section>;
}
