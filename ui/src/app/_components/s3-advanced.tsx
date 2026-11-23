"use client";
import { useRef, useState } from "react";
import { translate as t, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { SuggestInput } from "@/components/ui/suggest-input";
import { useScopedRequest } from "./vtl-scope";
import type { Discovery } from "./s3-bucket-step";

/** Rare operations, on the saved connection ①: no second credential form. */
export function S3Advanced({ ready, connected, discovery, onChanged }: { ready: boolean; connected: boolean; discovery: Discovery | null; onChanged: () => Promise<void> }) {
  useI18n();
  const request = useScopedRequest();
  const [bucket, setBucket] = useState(""), [prefix, setPrefix] = useState(""), [busy, setBusy] = useState(false);
  const [message, setMessage] = useState(""), [error, setError] = useState("");
  const pending = useRef(false);
  async function attach(activate: boolean) {
    if (!discovery || pending.current || !bucket.trim()) return;
    pending.current = true; setBusy(true); setError(""); setMessage("");
    try {
      await request("/v1/storage/buckets/attach", { method: "POST", headers: { "Content-Type": "application/json" }, signal: AbortSignal.timeout(45000),
        body: JSON.stringify({ revision: discovery.revision, bucket: bucket.trim(), ...(prefix ? { prefix } : {}), ...(activate ? { activate: true } : {}) }) });
      setMessage(t(activate ? "Bucket activé. Les cartouches se synchronisent avec lui en arrière-plan." : "Bucket rattaché. Ses copies apparaissent dans Copies S3."));
      setBucket(""); setPrefix("");
      await onChanged();
    } catch (cause) { setError(cause instanceof Error ? cause.message : t("Opération non confirmée.")); }
    finally { pending.current = false; setBusy(false); }
  }
  const available = ready && connected && !!discovery && !busy;
  return <details className="rounded-xl border p-5">
    <summary className="cursor-pointer font-medium">{t("Options avancées")}</summary>
    <section aria-label={t("Utiliser un bucket existant")} className="mt-4 space-y-3">
      {discovery && !discovery.complete && <p className="text-sm text-muted-foreground">{t("La liste des buckets est incomplète. Vous pouvez saisir le nom d’un bucket connu.")}{discovery.error ? ` ${t(discovery.error)}` : ""}</p>}
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-2"><Label htmlFor="existing-bucket">{t("Bucket existant")}</Label><SuggestInput id="existing-bucket" value={bucket} onValueChange={setBucket} suggestions={discovery?.buckets.filter(item => !item.attached).map(item => item.name) ?? []} disabled={!available} /></div>
        <div className="space-y-2"><Label htmlFor="existing-prefix">{t("Préfixe des objets")}</Label><Input id="existing-prefix" value={prefix} placeholder={t("Préfixe de la connexion par défaut")} disabled={!available} onChange={event => setPrefix(event.target.value)} /></div>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="outline" disabled={!available || !bucket.trim()} onClick={() => void attach(true)}>{t("Utiliser comme bucket actif")}</Button>
        <Button type="button" variant="outline" disabled={!available || !bucket.trim()} onClick={() => void attach(false)}>{t("Rattacher un ancien bucket")}</Button>
      </div>
      {message && <p role="status" className="text-sm">{message}</p>}
      {error && <p role="alert" className="text-sm text-destructive">{t(error)}</p>}
    </section>
  </details>;
}
