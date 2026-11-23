"use client";
import { translate as t, useI18n, getLocale, isPlural } from "@/lib/i18n";

import { useCallback, useEffect, useState } from 'react';
import { Mail, RefreshCw, Send } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Select, SelectOption } from '@/components/ui/select';
import { emailConfiguration, emailHistory, emailPreview, saveEmailConfiguration, sendTestEmail, type EmailConfiguration, type EmailHistory, type EmailPreview, type EmailSettings } from '@/lib/notifications';

const families: Record<string, string> = { export: 'Export S3', restore: 'Restauration S3', reintegrate: 'Réintégration PBS', archive: 'Archivage', recall: 'Rappel' };
const statuses: Record<string, string> = { queued: 'En attente', sending: 'Envoi en cours', sent: 'Accepté par le relais', failed: 'Échec de l’envoi', uncertain: 'Livraison incertaine' };
const reasons: Record<string, string> = { certificate: 'Certificat SMTP non valide', authentication: 'Authentification refusée', connection: 'Connexion interrompue', recipient_rejected: 'Destinataire refusé', sender_rejected: 'Expéditeur refusé', message_rejected: 'Message refusé', smtp_rejected: 'Requête SMTP refusée', configuration: 'Configuration à vérifier', helper_unavailable: 'Service d’envoi indisponible', interrupted: 'Envoi interrompu', invalid_helper_result: 'Résultat non confirmé' };
const message = (error: unknown) => error instanceof Error ? error.message : t("Impossible de contacter le service.");

export function Notifications() {
  const {language}=useI18n();
  const [config, setConfig] = useState<EmailConfiguration | null>(null);
  const [settings, setSettings] = useState<EmailSettings | null>(null);
  const [recipients, setRecipients] = useState('');
  const [password, setPassword] = useState('');
  const [clearPassword, setClearPassword] = useState(false);
  const [preview, setPreview] = useState<EmailPreview | null>(null);
  const [history, setHistory] = useState<EmailHistory | null>(null);
  const [offset, setOffset] = useState(0);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const load = useCallback(async () => {
    setBusy(true); setError(null);
    try {
      const [next, example] = await Promise.all([emailConfiguration(), emailPreview()]);
      setConfig(next); setSettings(next.settings); setRecipients(next.settings.recipients.join('\n'));
      setPassword(''); setClearPassword(false); setDirty(false); setPreview(example);
    } catch (cause) { setError(message(cause)); } finally { setBusy(false); }
  }, []);
  const refreshHistory = useCallback(async () => {
    try { setHistory(await emailHistory(offset)); setHistoryError(null); }
    catch (cause) { setHistoryError(message(cause)); }
  }, [offset]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => { void refreshHistory(); const timer = setInterval(() => { if (!document.hidden) void refreshHistory(); }, 5000); return () => clearInterval(timer); }, [refreshHistory]);
  function change<K extends keyof EmailSettings>(key: K, value: EmailSettings[K]) { setSettings(current => current && ({ ...current, [key]: value })); setDirty(true); setNotice(null); }
  async function save(event: React.FormEvent) {
    event.preventDefault(); if (!config || !settings) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      const next = await saveEmailConfiguration(config.revision, { ...settings, recipients: recipients.split(/[\n,;]+/).map(s => s.trim()).filter(Boolean) }, password || null, clearPassword);
      setConfig(next); setSettings(next.settings); setRecipients(next.settings.recipients.join('\n')); setPassword(''); setClearPassword(false); setDirty(false);
      setNotice(t("Configuration enregistrée."));
      setPreview(await emailPreview());
    } catch (cause) { setError(message(cause)); } finally { setBusy(false); }
  }
  async function test() {
    setBusy(true); setError(null); setNotice(null);
    try { await sendTestEmail(); setNotice(t("Email de test mis en file d’envoi.")); await refreshHistory(); }
    catch (cause) { setError(message(cause)); } finally { setBusy(false); }
  }
  if (!settings || !config) return <div className="space-y-4">{error ? <Alert variant="destructive"><AlertTitle>{t("Notifications indisponibles")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert> : <p role="status">{t("Chargement des notifications…")}</p>}<Button variant="outline" disabled={busy} onClick={() => void load()}>{t("Réessayer")}</Button></div>;
  return <div className="min-w-0 space-y-8">
    {error && <Alert variant="destructive"><AlertTitle>{t("Action non effectuée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
    {notice && <p role="status" className="text-sm text-primary">{t(notice)}</p>}
    <div className="grid min-w-0 gap-8 xl:grid-cols-[minmax(0,1fr)_minmax(340px,0.9fr)]">
      <form onSubmit={save} className="min-w-0 space-y-7">
        <fieldset disabled={busy} className="space-y-7 disabled:opacity-60">
          <div className="flex items-start gap-3 rounded-xl border bg-card p-5">
            <input id="email-enabled" type="checkbox" checked={settings.enabled} onChange={e => change('enabled', e.target.checked)} className="mt-1 size-4 shrink-0 accent-primary" />
            <div><Label htmlFor="email-enabled">{t("Activer les notifications")}</Label><p className="mt-1 text-sm text-muted-foreground">{t("Configuration commune à toutes les bibliothèques. Chaque email identifie la bibliothèque concernée.")}</p></div>
          </div>
          {settings.language !== undefined && <div className="space-y-2">
            <Label htmlFor="email-language">{t("Langue des emails")}</Label>
            <Select id="email-language" value={settings.language} onValueChange={(selectedValue) => change('language', selectedValue as 'fr'|'en')}>
              <SelectOption value="fr">Français</SelectOption><SelectOption value="en">English</SelectOption>
            </Select>
            <p className="text-sm text-muted-foreground">{t("Cette langue s’applique à tous les emails lors de leur envoi, indépendamment de la langue de l’interface.")}</p>
          </div>}
          <section className="space-y-4" aria-labelledby="smtp-title">
            <h2 id="smtp-title" className="text-lg font-semibold">{t("Connexion SMTP")}</h2>
            <div className="grid gap-4 sm:grid-cols-[1fr_110px]">
              <div className="space-y-2"><Label htmlFor="email-host">{t("Serveur SMTP")}</Label><Input id="email-host" value={settings.host} required={settings.enabled} placeholder="smtp.exemple.fr" onChange={e => change('host', e.target.value.trim())} /></div>
              <div className="space-y-2"><Label htmlFor="email-port">{t("Port")}</Label><Input id="email-port" type="number" min={1} max={65535} required value={settings.port || ''} onChange={e => change('port', Number(e.target.value))} /></div>
            </div>
            <div className="space-y-2"><Label htmlFor="email-tls">{t("Sécurité de la connexion")}</Label><Select id="email-tls" value={settings.tls} onValueChange={(selectedValue) => change('tls', selectedValue as EmailSettings['tls'])}><SelectOption value="starttls">{t("STARTTLS obligatoire")}</SelectOption><SelectOption value="implicit">{t("TLS implicite")}</SelectOption></Select></div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2"><Label htmlFor="email-username">{t("Identifiant SMTP (facultatif)")}</Label><Input id="email-username" autoComplete="off" value={settings.username} onChange={e => change('username', e.target.value)} /></div>
              <div className="space-y-2"><Label htmlFor="email-password">{t("Mot de passe SMTP")}</Label><Input id="email-password" type="password" autoComplete="new-password" disabled={clearPassword} value={password} placeholder={config.has_password ? t("Enregistré · laisser vide pour conserver") : t("Mot de passe du relais")} onChange={e => { setPassword(e.target.value); setDirty(true); }} /></div>
            </div>
            {config.has_password && <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={clearPassword} onChange={e => { setClearPassword(e.target.checked); setPassword(''); setDirty(true); }} className="size-4 accent-primary" />{t("Supprimer le mot de passe enregistré")}</label>}
          </section>
          <section className="space-y-4 border-t pt-6" aria-labelledby="email-addresses">
            <h2 id="email-addresses" className="text-lg font-semibold">{t("Expéditeur et destinataires")}</h2>
            <div className="space-y-2"><Label htmlFor="email-installation">{t("Nom de l’installation")}</Label><Input id="email-installation" required maxLength={100} value={settings.installation} onChange={e => change('installation', e.target.value)} /></div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2"><Label htmlFor="email-sender-name">{t("Nom de l’expéditeur")}</Label><Input id="email-sender-name" required value={settings.sender_name} onChange={e => change('sender_name', e.target.value)} /></div>
              <div className="space-y-2"><Label htmlFor="email-sender">{t("Adresse de l’expéditeur")}</Label><Input id="email-sender" type="email" required={settings.enabled} value={settings.sender} onChange={e => change('sender', e.target.value)} /></div>
            </div>
            <div className="space-y-2"><Label htmlFor="email-recipients">{t("Destinataires")}</Label><textarea id="email-recipients" required={settings.enabled} rows={3} value={recipients} onChange={e => { setRecipients(e.target.value); setDirty(true); }} className="w-full rounded-md border border-input bg-transparent p-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-describedby="email-recipients-help" /><p id="email-recipients-help" className="text-sm text-muted-foreground">{t("Une adresse par ligne, jusqu’à 20 destinataires. Les emails déjà en attente conservent leurs destinataires.")}</p></div>
          </section>
          <section className="space-y-3 border-t pt-6" aria-labelledby="email-events"><h2 id="email-events" className="text-lg font-semibold">{t("Événements à recevoir")}</h2>
            <table className="w-full text-sm"><thead><tr className="border-b"><th className="py-3 text-left font-medium">{t("Opération")}</th><th className="px-2 font-medium">{t("Succès")}</th><th className="px-2 font-medium">{t("Échec")}</th></tr></thead><tbody>{Object.entries(families).map(([key, label]) => <tr key={key} className="border-b"><th scope="row" className="py-3 text-left font-normal">{t(label)}</th>{(['success', 'failure'] as const).map(outcome => <td key={outcome} className="text-center"><input type="checkbox" className="size-4 accent-primary" aria-label={`${t(label)} · ${outcome === 'success' ? t("Succès") : t("Échec")}`} checked={settings.events[key]?.[outcome] ?? false} onChange={e => change('events', { ...settings.events, [key]: { ...(settings.events[key] ?? { success: false, failure: false }), [outcome]: e.target.checked } })} /></td>)}</tr>)}</tbody></table>
            <p className="text-sm text-muted-foreground">{t("Archivage et rappel : le succès attend la confirmation du stockage. Un résultat incertain déclenche une alerte si les échecs sont activés.")}</p>
          </section>
        </fieldset>
        <div className="flex flex-wrap items-center gap-3"><Button type="submit" disabled={busy || !dirty}>{t("Enregistrer")}</Button><Button type="button" variant="outline" disabled={busy} onClick={() => void load()}><RefreshCw />{t("Recharger")}</Button>{dirty && <span className="text-sm text-muted-foreground">{t("Modifications non enregistrées")}</span>}</div>
      </form>
      <section className="min-w-0 space-y-4" aria-labelledby="email-preview-title">
        <div><h2 id="email-preview-title" className="flex items-center gap-2 text-lg font-semibold"><Mail className="size-5" />{t("Aperçu de l’email")}</h2><p className="mt-1 text-sm text-muted-foreground">{t("Exemple basé sur la configuration enregistrée.")}</p></div>
        {preview && <><p className="break-words text-sm text-muted-foreground">{t("Objet : {subject}", {subject: preview.subject})}</p><iframe title={t("Aperçu de l’email")} sandbox="" srcDoc={preview.html} className="h-[640px] w-full rounded-xl border bg-white" /><details className="text-sm"><summary className="cursor-pointer text-muted-foreground">{t("Version texte")}</summary><pre className="mt-3 whitespace-pre-wrap break-words rounded-lg border p-4 font-sans">{preview.text}</pre></details></>}
        <Button type="button" variant="outline" className="max-w-full whitespace-normal" disabled={busy || dirty || !config.settings.enabled} onClick={() => void test()}><Send />{t("Envoyer un email de test")}</Button>
        <p className="text-sm text-muted-foreground">{t("Enregistrez et activez la configuration avant le test. L’envoi utilise les destinataires enregistrés.")}</p>
      </section>
    </div>
    <section className="space-y-4 border-t pt-6" aria-labelledby="email-history-title">
      <div className="flex flex-wrap items-center justify-between gap-3"><h2 id="email-history-title" className="text-lg font-semibold">{t("Journal des envois")}</h2><Button variant="ghost" size="sm" onClick={() => void refreshHistory()}><RefreshCw />{t("Actualiser")}</Button></div>
      <p className="text-sm text-muted-foreground">{t("« Accepté par le relais » confirme la remise au serveur SMTP. Vérifiez votre boîte mail pour confirmer la réception. Les livraisons incertaines ne sont pas renvoyées automatiquement.")}</p>
      {historyError && <p role="alert" className="text-destructive">{t(historyError)}</p>}
      {!history ? <p role="status">{t("Chargement du journal…")}</p> : !history.items.length ? <p className="rounded-xl border p-6 text-sm text-muted-foreground">{t("Aucun email enregistré pour le moment.")}</p> : <ul className="divide-y rounded-xl border">{history.items.map(item => <li key={item.id} className="flex flex-wrap justify-between gap-3 p-4 text-sm"><div className="min-w-0"><p className="break-words font-medium">{item.event.subject}</p><p className="break-all text-muted-foreground">{item.recipient}</p><p className="text-muted-foreground">{new Date(item.created_at * 1000).toLocaleString(getLocale())} · {t(language==='fr'?"{count} tentative(s)":isPlural(item.attempts)?"{count} tentatives":"{count} tentative", {count: item.attempts})}</p></div><div><p className={item.status === 'failed' || item.status === 'uncertain' ? 'font-medium text-destructive' : 'font-medium'}>{t(statuses[item.status] ?? item.status)}</p>{item.category && reasons[item.category] && <p className="text-muted-foreground">{t(reasons[item.category])}</p>}</div></li>)}</ul>}
      <div className="flex justify-end gap-2"><Button variant="outline" size="sm" disabled={offset === 0} onClick={() => setOffset(value => Math.max(0, value - 25))}>{t("Précédent")}</Button><Button variant="outline" size="sm" disabled={!history?.has_more} onClick={() => setOffset(value => value + 25)}>{t("Suivant")}</Button></div>
    </section>
  </div>;
}
