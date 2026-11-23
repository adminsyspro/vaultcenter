"use client";
import { translate as t, isPlural, useI18n } from "@/lib/i18n";

import { useScopedRequest } from "./vtl-scope";
import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, CheckCircle2, CircleHelp, CirclePause, CircleX, LoaderCircle, ShieldCheck } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import {
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
} from "@/components/ui/card";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { type Job, isActive, formatDate } from "@/lib/api";
import { Reintegration } from "./reintegration";
import { localizedToast as toast } from "./providers";

export interface MaintenanceStatus {
  configured: boolean;
  phase: string;
  ready: boolean;
  blocked?: boolean;
  pbs_locked?: boolean;
  services?: Record<string, string>;
  message?: string | null;
  actor?: string;
  since?: number;
  reintegration_pending?: boolean;
  client?: "pbs" | "veeam";
}
interface Event {
  id: number;
  action: string;
  success: boolean;
  created_at: string;
  actor: string | null;
  message: string;
}
const phases: Record<string, string> = {
  online: "En service",
  maintenance: "En maintenance",
  entering: "Mise en maintenance",
  leaving: "Reprise des services",
  failed: "Intervention à reprendre",
  unconfigured: "Non configurée",
};
const serviceNames: Record<string, string> = {
  "tgt.service": "Accès iSCSI",
  "vtltape@11.service": "Lecteur virtuel",
  "vtllibrary@10.service": "Changeur de cartouches",
  "mhvtl.target": "Ensemble VTL",
};
const serviceStates: Record<string, string> = {
  active: "Actif",
  inactive: "Arrêté",
  failed: "En échec",
  activating: "Démarrage",
  deactivating: "Arrêt en cours",
  reloading: "Rechargement",
};
function ServiceState({ value }: { value: string }) {
  useI18n();
  const transitioning = ["activating", "deactivating", "reloading"].includes(value);
  const Icon = value === "active" ? CheckCircle2
    : value === "inactive" ? CirclePause
    : value === "failed" ? CircleX
    : transitioning ? LoaderCircle : CircleHelp;
  const color = value === "active" ? "text-emerald-700 dark:text-emerald-400"
    : value === "failed" ? "text-destructive"
    : transitioning ? "text-primary" : "text-muted-foreground";
  return (
    <span className={`flex shrink-0 items-center gap-2 text-sm font-medium ${color}`}>
      <Icon aria-hidden="true" className={`size-4 ${transitioning ? "motion-safe:animate-spin" : ""}`} />
      {t(serviceStates[value] ?? value)}
    </span>
  );
}
function ServiceDiagnostics({ services }: { services?: Record<string, string> }) {
  useI18n();
  const entries = Object.entries(services ?? {});
  const activeCount = entries.filter(([, state]) => state === "active").length;
  const failedCount = entries.filter(([, state]) => state === "failed").length;
  return (
    <section aria-label={t("Diagnostic technique")}>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-3">
              <Activity aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-primary" />
              <div className="space-y-1">
                <p className="font-medium">{t("Services de la bibliothèque")}</p>
                <CardDescription>{t("État du lecteur, du changeur et de l’accès iSCSI.")}</CardDescription>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline" className="tabular-nums">
                {entries.length ? t("{active} / {total} actifs", { active: activeCount, total: entries.length }) : t("État indisponible")}
              </Badge>
              {failedCount > 0 && <Badge variant="destructive">{t("{count} en échec", { count: failedCount })}</Badge>}
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {entries.length ? (
              <ul className="divide-y border-t text-sm">
                {entries.map(([name, value]) => (
                  <li key={name} className="flex items-center justify-between gap-3 py-3">
                    <div className="min-w-0">
                      {serviceNames[name] && <p className="font-medium">{t(serviceNames[name])}</p>}
                      <p className="break-all font-mono text-xs text-muted-foreground">{name}</p>
                    </div>
                    <ServiceState value={value} />
                  </li>
                ))}
              </ul>
          ) : <p className="text-sm text-muted-foreground">{t("L’état des services n’a pas pu être confirmé.")}</p>}
        </CardContent>
    </section>
  );
}
export function Maintenance({
  open,
  canManage,
  connected,
  jobs,
  onRefresh,
  onClose,
  onStatusConfirmed,
  onTransitionPending,
}: {
  open: boolean;
  canManage: boolean;
  connected: boolean;
  jobs: Job[];
  onRefresh: () => Promise<void>;
  onClose: () => void;
  onStatusConfirmed: (status: MaintenanceStatus) => void;
  onTransitionPending: (pending: boolean) => void;
}) {
  useI18n();
  const request = useScopedRequest();
  const [status, setStatus] = useState<MaintenanceStatus | null>(null);
  const [events, setEvents] = useState<Event[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<"enter" | "leave" | null>(null);
  const [idleConfirmed, setIdleConfirmed] = useState(false);
  const [live, setLive] = useState<MaintenanceStatus | null>(null);
  const [showReintegration, setShowReintegration] = useState(false);
  const transitioning = useRef(false);
  const readGeneration = useRef(0);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (transitioning.current) return;
    const generation = ++readGeneration.current;
    const [status, events] = await Promise.all([
      request<MaintenanceStatus>("/v1/maintenance", { signal }),
      historyOpen ? request<Event[]>("/v1/maintenance/events", { signal }) : Promise.resolve(null),
    ]);
    if (generation !== readGeneration.current || transitioning.current || signal?.aborted) return;
    setStatus(status);
    if (events) setEvents(events);
  }, [request, historyOpen]);
  useEffect(() => {
    const controller = new AbortController();
    let inFlight = false;
    const update = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        await refresh(controller.signal);
      } catch (cause) {
        if (!controller.signal.aborted) {
          setStatus(null);
          setError(
            cause instanceof Error
              ? cause.message
              : t("État de maintenance inaccessible."),
          );
        }
      } finally {
        inFlight = false;
      }
    };
    void update();
    const timer = setInterval(() => {
      if (!document.hidden) void update();
    }, 5000);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [refresh]);
  useEffect(() => { if (!open) setIdleConfirmed(false); }, [open]);
  // A transition started here, or one the server reports (e.g. after a reload).
  const moving = pending ?? (status?.phase === "entering" ? "enter" : status?.phase === "leaving" ? "leave" : null);
  // During a transition the window stays open: read the services to show progress.
  useEffect(() => {
    if (!moving) { setLive(null); return; }
    const controller = new AbortController();
    let reported = false;
    const tick = () => void request<MaintenanceStatus>("/v1/maintenance", { signal: AbortSignal.any([controller.signal, AbortSignal.timeout(5000)]) })
      .then(value => { if (!controller.signal.aborted) setLive(value); })
      .catch(cause => {
        // One log per transition: the read is retried every 2 seconds.
        if (!controller.signal.aborted && !reported) { reported = true; console.error("Maintenance progress read failed", cause); }
      });
    tick(); const timer = setInterval(tick, 2000);
    return () => { controller.abort(); clearInterval(timer); };
  }, [moving, request]);
  const active = jobs.filter(isActive);
  const veeam = status?.client === "veeam";
  async function transition(action: "enter" | "leave") {
    if (transitioning.current) return;
    transitioning.current = true;
    readGeneration.current++;
    setPending(action);
    onTransitionPending(true);
    setError("");
    try {
      const confirmIdle = action === "enter" && veeam && idleConfirmed;
      setIdleConfirmed(false);
      const confirmed = await request<MaintenanceStatus>(`/v1/maintenance/${action}`, {
        method: "POST", signal: AbortSignal.timeout(610000),
        ...(confirmIdle ? { headers: { "Content-Type": "application/json" }, body: JSON.stringify({ confirm_idle: true }) } : {}),
      });
      setStatus(confirmed);
      onStatusConfirmed(confirmed);
      if (confirmed.phase === (action === "enter" ? "maintenance" : "online")) {
        toast.success(action === "enter" ? t("Bibliothèque arrêtée") : t("Bibliothèque remise en service"));
      }
    } catch (cause) {
      const message = cause instanceof Error
        ? cause.message
        : t("Transition non confirmée. Consultez l’état avant de réessayer.");
      setError(message);
      toast.error(message);
    } finally {
      transitioning.current = false;
      setPending(null);
      onTransitionPending(false);
      void Promise.allSettled([refresh(), onRefresh()]);
    }
  }
  const online = status?.phase === "online";
  const unavailable = !connected || !canManage || ["entering", "leaving"].includes(status?.phase ?? "") || !!pending || !!active.length || !!status?.reintegration_pending || !status?.configured;
  const action = online ? "enter" : "leave";
  const needsIdleConfirmation = online && veeam && !idleConfirmed;
  const current = live ?? status;
  const services = Object.values(current?.services ?? {});
  const activeServices = services.filter(value => value === "active").length;
  const stateLine = current?.phase
    ? `${t(phases[current.phase] ?? current.phase)} · ${t("{active}/{total} services actifs", { active: activeServices, total: services.length })}`
    : t("État indisponible");
  const progress = services.length ? Math.round(100 * (moving === "enter" ? services.length - activeServices : activeServices) / services.length) : 0;
  const prerequisite = online
    ? t(veeam ? "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis Veeam."
      : status?.client === "pbs" ? "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis PBS."
      : "Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis le logiciel de sauvegarde.")
    : status?.ready ? t("Remettez la bibliothèque en service quand les cartouches sont prêtes.") : null;
  return (
    <Dialog open={open} onOpenChange={value => { if (!value) onClose(); }}>
    <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-md">
      <DialogHeader icon={ShieldCheck}><DialogTitle>{t("Maintenance")}</DialogTitle><DialogDescription>{stateLine}</DialogDescription></DialogHeader>
    <section aria-label={t("Maintenance")}>
      <div className="space-y-3">
        {moving
          ? <div className="space-y-2">
              <p role="status" className="text-sm">{t(moving === "enter" ? "Les services de la bibliothèque s’arrêtent." : "Les services de la bibliothèque redémarrent.")}</p>
              <div role="progressbar" aria-label={t("Progression de la maintenance")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress} className="h-2 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-primary transition-all" style={{ width: `${Math.max(progress, 5)}%` }} /></div>
              <p className="text-xs text-muted-foreground">{t("Vous pouvez fermer cette fenêtre. L’opération continue.")}</p>
            </div>
          : prerequisite && <p className="text-sm">{prerequisite}</p>}
        {online && veeam && !moving && <label className="flex items-start gap-2 text-sm"><Checkbox className="mt-0.5" checked={idleConfirmed} disabled={unavailable} onCheckedChange={value => setIdleConfirmed(value === true)} />{t("Aucun job Veeam n’utilise cette bibliothèque")}</label>}
        <Button variant={online ? "outline" : "default"} disabled={unavailable || needsIdleConfirmation} onClick={() => void transition(action)}>
          {moving && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
          {moving === "enter" ? t("Mise en maintenance…") : moving === "leave" ? t("Reprise des services…") : online ? t("Passer en maintenance") : t("Reprendre le service")}
        </Button>
      </div>
      {!connected && <p role="status" className="mt-3 text-sm text-destructive">{t("Connexion à la bibliothèque interrompue.")}</p>}
      {!!active.length && <p role="status" className="mt-3 text-sm text-muted-foreground">{t("Attendez la fin des opérations VTL pour changer le mode de maintenance.")}</p>}
      {!canManage && <p className="mt-3 text-sm text-muted-foreground">{t("La gestion de la maintenance est réservée aux administrateurs.")}</p>}
      {status && !status.configured && <p className="mt-3 text-sm text-muted-foreground">{t("L’adaptateur de maintenance PBS/VTL n’est pas configuré sur cette appliance.")}</p>}
      {error && <Alert variant="destructive" className="mt-3"><AlertTitle>{t("Opération non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
      {status?.message && <Alert className="mt-3"><AlertTitle>{t("État à vérifier")}</AlertTitle><AlertDescription>{t(status.message)}</AlertDescription></Alert>}
      {status?.reintegration_pending && (showReintegration
        ? <div className="mt-4"><Reintegration jobs={jobs} canManage={canManage} onRefresh={onRefresh} /></div>
        : <p className="mt-3 flex flex-wrap items-center gap-x-2 text-sm"><span>{t("Une réintégration est à terminer.")}</span><Button variant="link" size="sm" className="h-auto p-0" onClick={() => setShowReintegration(true)}>{t("Voir la réintégration")}</Button></p>)}
      <div className="mt-4 space-y-3 border-t pt-3">
        <details onToggle={event => setHistoryOpen(event.currentTarget.open)}>
          <summary className="cursor-pointer text-sm font-medium text-muted-foreground">{t("Détails")}</summary>
          <div className="mt-4 space-y-4">
            <dl className="flex flex-wrap gap-x-8 gap-y-3 text-sm">
              {!veeam && <div><dt className="text-muted-foreground">{t("Lecteur PBS")}</dt><dd>{status?.pbs_locked ? t("Réservé pour la maintenance") : online ? t("Non réservé par la VTL") : t("Réservation non confirmée")}</dd></div>}
              <div><dt className="text-muted-foreground">{t("Protection des services")}</dt><dd>{status?.blocked ? t("Redémarrage automatique bloqué") : t("Verrou de maintenance inactif")}</dd></div>
              <div><dt className="text-muted-foreground">{t("Opérations VTL en cours")}</dt><dd>{active.length}</dd></div>
            </dl>
            <ServiceDiagnostics services={status?.services} />
          </div>
          <div className="mt-4">
            <h3 className="text-sm font-medium">{t("Dernières interventions")}</h3>
            <p className="mt-1 text-sm text-muted-foreground">{t(isPlural(events.length) ? "{count} interventions affichées, de la plus récente à la plus ancienne." : "{count} intervention affichée, de la plus récente à la plus ancienne.", { count: events.length })}</p>
          </div>
          <ul className="mt-3 divide-y">
            {events.map(event => <li key={event.id} className="space-y-2 py-4 first:pt-0 last:pb-0 text-sm">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div><p className="font-medium">{event.action === "enter" ? t("Mise en maintenance") : t("Reprise des services")}</p><p className="mt-1 text-xs text-muted-foreground">{event.actor || t("Compte supprimé")} · {formatDate(event.created_at)}</p></div>
                <Badge variant={event.success ? "outline" : "destructive"}>{event.success ? t("Terminée") : t("Échec")}</Badge>
              </div>
              {!event.success && <p className="break-words text-destructive">{t(event.message)}</p>}
            </li>)}
            {!events.length && <li className="py-4 text-sm text-muted-foreground">{t("Aucune intervention enregistrée.")}</li>}
          </ul>
        </details>
      </div>
    </section>
    </DialogContent>
    </Dialog>
  );
}
