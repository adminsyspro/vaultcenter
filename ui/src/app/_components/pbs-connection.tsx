"use client";
import { translate as t, useI18n } from "@/lib/i18n";
import { useEffect, useId, useRef, useState } from "react";
import { Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { request } from "@/lib/api";
import { connectionInfo, type PbsConnectionInfo } from "@/lib/pbs-connection";

export function PbsConnectionPanel({ vtlId }: { vtlId: string }) {
  useI18n();
  const prefix = useId(), alive = useRef(true), locked = useRef(false), generation = useRef(0);
  const [info, setInfo] = useState<PbsConnectionInfo | null>(null);
  const [host, setHost] = useState(""), [port, setPort] = useState("3260"), [error, setError] = useState(""), [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(""), [copyError, setCopyError] = useState("");
  const endpoint = `/v1/vtls/${encodeURIComponent(vtlId)}/pbs-connection`;
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => {
    const controller = new AbortController(), token = ++generation.current; setInfo(null); setHost(""); setPort("3260"); setError(""); setCopied(""); setCopyError("");
    void request<unknown>(endpoint, {signal: controller.signal}).then(value => {
      const fresh = connectionInfo(value, vtlId);
      if (!controller.signal.aborted && generation.current === token) { setInfo(fresh); setHost(fresh.host ?? window.location.hostname); setPort(String(fresh.port)); }
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Procédure indisponible.")); });
    return () => { controller.abort(); ++generation.current; };
  }, [endpoint, vtlId]);
  async function save(event: React.FormEvent) {
    event.preventDefault(); if (!info || locked.current) return;
    locked.current = true; const token = ++generation.current; setBusy(true); setError("");
    try {
      const fresh = connectionInfo(await request<unknown>(endpoint, {method: "PUT", headers: {"Content-Type": "application/json"}, body: JSON.stringify({revision: info.revision, host: host.trim(), port: Number(port)})}), vtlId);
      if (alive.current && generation.current === token) { setInfo(fresh); setHost(fresh.host ?? ""); setPort(String(fresh.port)); setCopied(""); }
    } catch (cause) { if (alive.current) setError(cause instanceof Error ? cause.message : t("Enregistrement non confirmé. Rouvrez cette fenêtre.")); }
    finally { locked.current = false; if (alive.current) setBusy(false); }
  }
  async function copy(id: string, command: string, origin: HTMLElement) {
    setCopied(""); setCopyError("");
    try { await navigator.clipboard.writeText(command); if (alive.current) setCopied(id); return; } catch {}
    // Plain-HTTP appliances have no Clipboard API. Copy from a temporary field
    // inside the dialog, so its focus trap keeps the selection.
    const field = document.createElement("textarea");
    field.value = command; field.setAttribute("readonly", ""); field.style.position = "fixed"; field.style.opacity = "0";
    (origin.closest("[role=dialog]") ?? document.body).appendChild(field);
    field.select();
    let done = false;
    try { done = document.execCommand("copy"); } catch {}
    field.remove(); origin.focus();
    if (!alive.current) return;
    if (done) setCopied(id);
    else setCopyError(t("Copie automatique indisponible. Sélectionnez les commandes ci-dessous et copiez-les manuellement."));
  }
  const changed = info && (host.trim() !== (info.host ?? "") || Number(port) !== info.port);
  return <section aria-label={t("Connexion iSCSI")} className="space-y-4">
    <p className="text-sm text-muted-foreground">{!info ? t("Adresse iSCSI et procédure de connexion de la bibliothèque.") : info.client === "pbs" ? t("Exécutez ces commandes en root sur le PBS associé à cette VTL.") : t("Exécutez ces commandes dans PowerShell en administrateur sur le serveur Veeam.")}</p>
      {info?.pbs && <p className="break-all text-sm">{t("PBS associé :")} <span className="font-mono">{info.pbs}</span></p>}
      <form onSubmit={event => void save(event)} className="space-y-3">
        <div className="grid min-w-0 gap-3 sm:grid-cols-[minmax(0,1fr)_7rem]"><div className="space-y-2"><Label htmlFor={`${prefix}-host`}>{t("Adresse iSCSI de VaultCenter")}</Label><Input id={`${prefix}-host`} required disabled={busy || !info} placeholder={t("Ex. 192.168.1.10")} value={host} onChange={e => setHost(e.target.value)}/></div><div className="space-y-2"><Label htmlFor={`${prefix}-port`}>{t("Port iSCSI")}</Label><Input id={`${prefix}-port`} type="number" min="1" max="65535" step="1" required disabled={busy || !info} value={port} onChange={e => setPort(e.target.value)}/></div></div>
        <p className="text-xs text-muted-foreground">{info?.client === "veeam" ? t("Choisissez l’adresse joignable depuis le serveur Veeam. Le port iSCSI est indépendant du port de cette interface.") : t("Choisissez l’adresse joignable depuis PBS. Le port iSCSI est indépendant du port de cette interface.")}</p>
        <div className="flex justify-end"><Button type="submit" disabled={busy || !info || !host.trim()}>{busy ? t("Enregistrement…") : t("Enregistrer le portail")}</Button></div>
      </form>
      {error && <Alert variant="destructive"><AlertDescription>{t(error)}</AlertDescription></Alert>}
      {info?.message && <p className="text-sm text-muted-foreground">{t(info.message)}</p>}
      {info?.phase === "missing_prerequisite" && info.client === "pbs" && !info.pbs && <p className="text-sm">{t("Prérequis : associer ce PBS à la bibliothèque, configurer son stockage et préparer son matériel. Sur PBS, installer open-iscsi et sg3-utils.")}</p>}
      {changed && <p role="status" className="text-sm text-muted-foreground">{t("Enregistrez le portail pour actualiser les commandes.")}</p>}
      {copyError && <p role="status" className="text-sm text-muted-foreground">{t(copyError)}</p>}
      {!changed && info?.steps.map((step, index) => <section key={step.id} className="min-w-0 space-y-2 border-t pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="font-medium">{index + 1}. {t(step.title)}</h3><Button type="button" size="sm" variant="outline" aria-label={t("Copier {title}", {title: t(step.title)})} onClick={event => void copy(step.id, step.command, event.currentTarget)}><Copy aria-hidden="true"/>{copied === step.id ? t("Copié") : t("Copier")}</Button></div>
        <p className="text-sm text-muted-foreground">{t(step.explanation)}</p><pre tabIndex={0} aria-label={t("Commandes : {title}", {title: t(step.title)})} className="max-h-72 overflow-auto rounded-md bg-muted p-3 font-mono text-xs leading-relaxed selection:bg-primary/25"><code>{step.command}</code></pre>
      </section>)}
  </section>;
}
