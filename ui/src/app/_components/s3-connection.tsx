"use client";
import { useEffect, useRef, useState } from "react";
import { CircleAlert, CircleCheck, CircleX } from "lucide-react";
import { translate as t, useI18n } from "@/lib/i18n";
import { request } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectOption } from "@/components/ui/select";

type Provider = { id: string; label: string; default_endpoint: string; default_region: string };
type Fields = { provider: string; endpoint: string; region: string; prefix: string; access_key_id: string };
/** What OVH says about the saved keys, checked by the server at most every 5 minutes. */
export type CredentialStatus = "ok" | "list_denied" | "invalid" | "unreachable" | "unknown";
type Connection = { revision: number; configured: boolean; secret_configured: boolean; configuration: Fields | null; credentials?: { status: CredentialStatus; checked_at: string } | null };
const fallbackProviders: Provider[] = [
  { id: "ovh_cold_archive_v1", label: "OVHcloud Cold Archive v1", default_endpoint: "https://s3.rbx-archive.io.cloud.ovh.net", default_region: "rbx-archive" },
  { id: "s3_compatible", label: "S3 compatible", default_endpoint: "", default_region: "" },
];
/** Shows an access key without revealing it: first and last four characters. */
export function maskKey(key: string) {
  return key.length <= 8 ? key : `${key.slice(0, 4)}…${key.slice(-4)}`;
}

/** Step ①: keys are checked with the provider before anything is saved. */
export function S3Connection({ vtlId, connected, onStatus, onSaved }: {
  vtlId: string; connected: boolean; onStatus: (configured: boolean, credentials: CredentialStatus | null) => void; onSaved: () => Promise<void>;
}) {
  useI18n();
  const endpoint = `/v1/vtls/${encodeURIComponent(vtlId)}/storage/connection`;
  const [saved, setSaved] = useState<Connection | null>(null);
  const [providers, setProviders] = useState<Provider[]>(fallbackProviders);
  const [fields, setFields] = useState<Fields>({ provider: fallbackProviders[0].id, endpoint: fallbackProviders[0].default_endpoint, region: fallbackProviders[0].default_region, prefix: "cold-vtl", access_key_id: "" });
  const [secret, setSecret] = useState(""), [editing, setEditing] = useState(false), [endpointOpen, setEndpointOpen] = useState(false);
  const [busy, setBusy] = useState(true), [error, setError] = useState(""), [warning, setWarning] = useState(""), [notice, setNotice] = useState("");
  const pending = useRef(false), status = useRef(onStatus);
  status.current = onStatus;
  useEffect(() => {
    const controller = new AbortController();
    void Promise.all([
      request<Connection>(endpoint, { signal: controller.signal }),
      request<{ providers?: Provider[] }>(`/v1/vtls/${encodeURIComponent(vtlId)}/settings/s3`, { signal: controller.signal })
        .catch(() => { if (!controller.signal.aborted) setNotice(t("La liste des fournisseurs est indisponible. Les fournisseurs habituels sont proposés.")); return { providers: undefined }; }),
    ]).then(([value, settings]) => {
      if (controller.signal.aborted) return;
      const list = settings.providers?.length ? settings.providers : fallbackProviders;
      setProviders(list); setSaved(value);
      if (value.configuration) setFields(value.configuration);
      else setFields(current => ({ ...current, provider: list[0].id, endpoint: list[0].default_endpoint, region: list[0].default_region }));
      status.current(value.configured, value.credentials?.status ?? null);
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Connexion S3 indisponible.")); })
      .finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [endpoint, vtlId]);
  const keepSecret = !!saved?.secret_configured && saved.configuration?.access_key_id === fields.access_key_id;
  function chooseProvider(id: string) {
    const provider = providers.find(item => item.id === id);
    if (provider) setFields({ ...fields, provider: id, endpoint: provider.default_endpoint, region: provider.default_region });
  }
  async function save(event: React.FormEvent) {
    event.preventDefault();
    if (pending.current || !connected || !saved) return;
    pending.current = true; setBusy(true); setError(""); setWarning("");
    try {
      const result = await request<{ warning?: string | null }>(endpoint, {
        method: "PUT", headers: { "Content-Type": "application/json" }, signal: AbortSignal.timeout(45000),
        body: JSON.stringify({ ...fields, revision: saved.revision, secret_access_key: secret || null }),
      });
      setSecret(""); setEditing(false); setEndpointOpen(false);
      const value = await request<Connection>(endpoint);
      setSaved(value); if (value.configuration) setFields(value.configuration);
      // The connection line already states a listing refusal reported by the check.
      status.current(value.configured, value.credentials?.status ?? null); setWarning(value.credentials ? "" : result?.warning ?? "");
      await onSaved();
    } catch (cause) { setError(cause instanceof Error ? cause.message : t("Connexion non confirmée.")); }
    finally { pending.current = false; setBusy(false); }
  }
  const summary = !!saved?.configured && !editing;
  const keys = saved?.credentials?.status ?? "ok";
  const tone = keys === "invalid" ? "text-destructive" : keys === "unreachable" || keys === "unknown" ? "text-amber-700 dark:text-amber-300" : "text-emerald-700 dark:text-emerald-300";
  const Icon = keys === "invalid" ? CircleX : keys === "unreachable" || keys === "unknown" ? CircleAlert : CircleCheck;
  return <section aria-label={t("Connexion S3")} className="space-y-3 rounded-xl border p-5">
    <h3 className="flex items-center gap-2 font-semibold"><span aria-hidden="true" className="inline-flex size-6 items-center justify-center rounded-full border text-xs tabular-nums">1</span>{t("Connexion S3")}</h3>
    {summary
      ? <>
        <p className={`flex flex-wrap items-center gap-x-2 text-sm ${tone}`}><Icon aria-hidden="true" className="size-4" />{t("Connexion S3")} · <span className="font-mono">{maskKey(saved?.configuration?.access_key_id ?? "")}</span> · <Button type="button" variant="link" size="sm" className="h-auto p-0" onClick={() => setEditing(true)}>{t("Modifier")}</Button></p>
        {keys === "list_denied" && <p role="status" className="text-sm text-muted-foreground">{t("OVH accepte ces clés, mais interdit de lister les buckets.")}</p>}
        {keys === "invalid" && <p role="alert" className="text-sm text-destructive">{t("OVH refuse ces clés. Corrigez-les avec « Modifier ».")}</p>}
        {(keys === "unreachable" || keys === "unknown") && <p role="status" className="text-sm text-amber-700 dark:text-amber-300">{t("OVH ne répond pas pour le moment. Les clés n’ont pas pu être vérifiées.")}</p>}
      </>
      : <form className="space-y-4" onSubmit={event => void save(event)} autoComplete="off">
        <fieldset disabled={busy || !connected || !saved} className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2"><Label htmlFor="connection-provider">{t("Fournisseur")}</Label><Select id="connection-provider" value={fields.provider} onValueChange={chooseProvider}>{providers.map(item => <SelectOption key={item.id} value={item.id}>{t(item.label)}</SelectOption>)}</Select></div>
          <div className="space-y-2"><Label htmlFor="connection-prefix">{t("Préfixe des objets")}</Label><Input id="connection-prefix" value={fields.prefix} onChange={event => setFields({ ...fields, prefix: event.target.value })} /></div>
          <div className="space-y-2"><Label htmlFor="connection-key">Access key</Label><Input id="connection-key" required value={fields.access_key_id} onChange={event => setFields({ ...fields, access_key_id: event.target.value })} /></div>
          <div className="space-y-2"><Label htmlFor="connection-secret">Secret key</Label><Input id="connection-secret" type="password" autoComplete="new-password" required={!keepSecret} value={secret} onChange={event => setSecret(event.target.value)} placeholder={keepSecret ? t("Secret enregistré — laisser vide pour le conserver") : ""} /></div>
          {endpointOpen || !providers.find(item => item.id === fields.provider)?.default_endpoint ? <>
            <div className="space-y-2"><Label htmlFor="connection-endpoint">{t("Endpoint HTTPS")}</Label><Input id="connection-endpoint" type="url" required value={fields.endpoint} onChange={event => setFields({ ...fields, endpoint: event.target.value })} /></div>
            <div className="space-y-2"><Label htmlFor="connection-region">{t("Région S3")}</Label><Input id="connection-region" required value={fields.region} onChange={event => setFields({ ...fields, region: event.target.value })} /></div>
          </> : <p className="text-xs text-muted-foreground sm:col-span-2">{fields.endpoint} · {fields.region} <Button type="button" variant="link" size="sm" className="h-auto p-0 text-xs" onClick={() => setEndpointOpen(true)}>{t("Modifier l’endpoint")}</Button></p>}
        </fieldset>
        <div className="flex gap-2"><Button type="submit" disabled={busy || !connected || !saved || !fields.endpoint || !fields.region || !fields.access_key_id || (!secret && !keepSecret)}>{busy && saved ? t("Vérification…") : t("Vérifier et enregistrer")}</Button>
        {saved?.configured && <Button type="button" variant="outline" disabled={busy} onClick={() => { setEditing(false); setSecret(""); setError(""); if (saved.configuration) setFields(saved.configuration); }}>{t("Annuler")}</Button>}</div>
      </form>}
    {error && <p role="alert" className="text-sm text-destructive">{t(error)}</p>}
    {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
    {warning && <p role="status" className="text-sm text-amber-700 dark:text-amber-300">{t(warning)}</p>}
  </section>;
}
