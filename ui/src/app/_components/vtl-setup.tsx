"use client";
import { translate as t, useI18n } from "@/lib/i18n";
import { useCallback, useEffect, useState } from "react";
import { Check, CircleCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { request } from "@/lib/api";
import { storageStatus, type TapeStorageStatus } from "@/lib/tape-storage";
import { clientBinding, type ClientBindingInfo } from "@/lib/client-binding";
import { ClientBinding } from "./client-binding";
import { Provisioning } from "./provisioning";

type Library = { id: string; lifecycle: string; provisioning_revision: number };

/** A finished step: one green line, with « Modifier » when it can change. */
function DoneLine({ children, onEdit }: { children: React.ReactNode; onEdit?: () => void }) {
  return <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-emerald-700 dark:text-emerald-300">
    <CircleCheck aria-hidden="true" className="size-4 shrink-0" />
    <span className="min-w-0 break-all">{children}</span>
    {onEdit && <Button type="button" variant="link" size="sm" className="h-auto p-0" onClick={onEdit}>{t("Modifier")}</Button>}
  </div>;
}

function Step({ index, title, done, children }: { index: number; title?: string; done: boolean; children: React.ReactNode }) {
  return <li className="grid grid-cols-[1.75rem_minmax(0,1fr)] gap-x-3 gap-y-2">
    <span role="img" aria-label={done ? t("Étape {index} terminée", { index }) : t("Étape {index}", { index })} className={`flex size-7 items-center justify-center rounded-full border text-sm font-medium tabular-nums ${done ? "border-primary bg-primary text-primary-foreground" : "text-muted-foreground"}`}>{done ? <Check aria-hidden="true" className="size-4" /> : index}</span>
    {title ? <><h3 className="self-center font-medium">{title}</h3><div className="col-start-2 min-w-0 space-y-2 text-sm">{children}</div></>
      : <div className="min-w-0 space-y-2 self-center text-sm">{children}</div>}
  </li>;
}

/** « Mise en service »: numbered steps while the VTL is a draft, a summary once active. */
export function VtlSetup({ vtl, onEditStorage }: { vtl: Library; onEditStorage: () => void }) {
  useI18n();
  const [storage, setStorage] = useState<TapeStorageStatus | null>(null), [storageError, setStorageError] = useState(false);
  const [client, setClient] = useState<ClientBindingInfo | null>(null), [phase, setPhase] = useState<string | null>(null);
  const [editingClient, setEditingClient] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    void request<unknown>(`/v1/vtls/${encodeURIComponent(vtl.id)}/settings/tape-storage`, { signal: controller.signal })
      .then(value => setStorage(storageStatus(value, vtl.id)))
      .catch(() => { if (!controller.signal.aborted) setStorageError(true); });
    void request<unknown>(`/v1/vtls/${encodeURIComponent(vtl.id)}/client`, { signal: controller.signal })
      .then(value => setClient(clientBinding(value, vtl.id)))
      .catch(cause => { if (!controller.signal.aborted) console.error("VTL client read failed", cause); });
    return () => controller.abort();
  }, [vtl.id]);
  const onStatus = useCallback((value: { phase: string }) => setPhase(value.phase), []);
  const active = vtl.lifecycle === "ready" || phase === "active";
  // The wording follows the client type; while it is unknown (unbound, loading or unreadable) it stays neutral.
  const kind = client?.client === "pbs" ? "PBS" : client?.client === "veeam" ? "Veeam" : null;
  const server = kind === "PBS" ? t("Serveur PBS") : kind === "Veeam" ? t("Serveur Veeam") : t("Serveur de sauvegarde");
  const storageReady = storage?.phase === "ready", clientReady = !!client?.client && !!client.initiator;
  const blocked = !storageReady ? t("Configurez d’abord le stockage local.")
    : !clientReady ? (kind === "PBS" ? t("Enregistrez d’abord l’adresse du serveur PBS.") : kind === "Veeam" ? t("Enregistrez d’abord l’adresse du serveur Veeam.") : t("Enregistrez d’abord l’adresse du serveur de sauvegarde."))
    : null;
  const storageText = storageError ? t("État du stockage indisponible.") : storage ? t("À configurer") : t("Lecture…");
  if (active) return <section aria-label={t("Bibliothèque en service")} className="space-y-3">
    <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_minmax(0,1fr)]">
      <dt className="text-muted-foreground">{server}</dt><dd className="font-mono text-xs sm:self-center">{client?.initiator ?? "—"}</dd>
      <dt className="text-muted-foreground">{t("Stockage local")}</dt><dd className="sm:self-center">{storage?.path ? <span className="break-all font-mono text-xs">{storage.path}</span> : <span className="text-muted-foreground">{storageText}</span>}</dd>
    </dl>
  </section>;
  return <ol aria-label={t("Mise en service")} className="space-y-6">
    <Step index={1} title={t("Stockage local")} done={storageReady}>
      {storageReady
        ? <DoneLine onEdit={onEditStorage}><span className="font-mono text-xs">{storage?.path}</span></DoneLine>
        : <div className="flex flex-wrap items-center justify-between gap-3"><span className="text-muted-foreground">{storageText}</span><Button type="button" size="sm" variant="outline" onClick={onEditStorage}>{t("Configurer")}</Button></div>}
    </Step>
    <Step index={2} title={server} done={clientReady}>
      {clientReady && !editingClient
        ? <section aria-label={t("Client de sauvegarde")}><DoneLine onEdit={client?.editable ? () => setEditingClient(true) : undefined}><span className="font-mono text-xs">{client?.initiator}</span></DoneLine></section>
        : <ClientBinding vtlId={vtl.id} onCancel={clientReady ? () => setEditingClient(false) : undefined} onChange={(info, saved) => { setClient(info); if (saved && info.initiator) setEditingClient(false); }} />}
    </Step>
    <Step index={3} done={false}>
      <Provisioning vtlId={vtl.id} revision={vtl.provisioning_revision} blocked={blocked} onStatus={onStatus} />
    </Step>
  </ol>;
}
