"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useEffect, useId, useRef, useState } from "react";
import { Popover } from "radix-ui";
import { Bell, CircleCheck, CircleHelp, CircleX, LoaderCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { formatDate, request } from "@/lib/api";
import { type ActivityFeed, type ActivityItem, activityHref, activityLabel, isActivityFeed } from "@/lib/activity";
import { vtlHash } from "./use-vtl-selection";

const REFRESH_MS = 30000;

const outcomes = {
  success: { icon: CircleCheck, label: "Réussi", className: "text-emerald-600 dark:text-emerald-400" },
  failure: { icon: CircleX, label: "Échec", className: "text-destructive" },
  uncertain: { icon: CircleHelp, label: "Incertain", className: "text-amber-700 dark:text-amber-300" },
};

function Row({ item, unread, onNavigate }: { item: ActivityItem; unread: boolean; onNavigate: () => void }) {
  const outcome = item.outcome ? outcomes[item.outcome] : null;
  const label = t(activityLabel(item));
  return (
    <li>
      <a href={activityHref(item)} onClick={onNavigate} className="lto-activity-row">
        {outcome ? (
          <outcome.icon aria-hidden="true" className={`mt-0.5 size-4 shrink-0 ${outcome.className}`} />
        ) : (
          <LoaderCircle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-primary motion-safe:animate-spin" />
        )}
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">
            {label}
            {outcome && <span className="sr-only"> · {t(outcome.label)}</span>}
          </span>
          <span className="block truncate font-mono text-xs">{item.subject}</span>
          <span className="block text-xs text-muted-foreground">
            {outcome
              ? `${item.vtl_name} · ${formatDate(item.finished_at ?? "")}`
              : t("{vtl} · depuis {time}", { vtl: item.vtl_name, time: formatDate(item.started_at ?? "") })}
          </span>
          {item.progress != null && <Progress value={item.progress} aria-label={label} aria-valuenow={item.progress} className="mt-2" />}
        </span>
        {unread && <span className="lto-activity-dot"><span className="sr-only">{t("Non lu")}</span></span>}
      </a>
    </li>
  );
}

export function ActivityBell({ selectedVtl }: { selectedVtl: string | null }) {
  useI18n();
  const titleId = useId();
  const runningId = useId();
  const recentId = useId();
  const [feed, setFeed] = useState<ActivityFeed | null>(null);
  const [failed, setFailed] = useState(false);
  const [open, setOpen] = useState(false);
  // Unread when the panel opened: the dots stay until it closes.
  const [shown, setShown] = useState<Set<string>>(new Set());
  const reload = useRef<() => Promise<void>>(async () => {});
  useEffect(() => {
    let disposed = false;
    let pending: AbortController | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      if (disposed || document.hidden || pending) return;
      const controller = new AbortController();
      pending = controller;
      try {
        const value = await request<unknown>("/v1/activity", { signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8000)]) });
        if (!disposed && !controller.signal.aborted) {
          if (!isActivityFeed(value)) throw Error("Réponse d’activité invalide.");
          setFeed(value);
          setFailed(false);
        }
      } catch {
        if (!disposed && !controller.signal.aborted) setFailed(true);
      } finally {
        if (pending === controller) {
          pending = null;
          if (!disposed) {
            clearTimeout(timer);
            timer = setTimeout(poll, REFRESH_MS);
          }
        }
      }
    }
    reload.current = async () => {
      clearTimeout(timer);
      pending?.abort();
      pending = null;
      await poll();
    };
    function visibility() {
      clearTimeout(timer);
      if (document.hidden) pending?.abort();
      else void poll();
    }
    document.addEventListener("visibilitychange", visibility);
    void poll();
    return () => {
      disposed = true;
      reload.current = async () => {};
      clearTimeout(timer);
      pending?.abort();
      document.removeEventListener("visibilitychange", visibility);
    };
  }, []);

  async function markSeen() {
    try {
      await request("/v1/activity/seen", { method: "POST" });
    } catch {
      // The badge comes back with the next refresh.
    }
    await reload.current();
  }
  function toggle(next: boolean) {
    setOpen(next);
    if (!next) {
      setShown(new Set());
      return;
    }
    setShown(new Set(feed?.recent.filter(item => item.unread).map(item => item.id)));
    setFeed(current => current && { ...current, unread: 0, recent: current.recent.map(item => ({ ...item, unread: false })) });
    void markSeen();
  }
  const close = () => toggle(false);
  const active = feed?.active.length ?? 0;
  const unread = feed?.unread ?? 0;
  return (
    <Popover.Root open={open} onOpenChange={toggle}>
      <Popover.Trigger asChild>
        <Button variant="ghost" size="icon" className="lto-activity-bell" aria-label={t("Notifications : {active} en cours, {unread} non lues", { active, unread })}>
          {active > 0 && <LoaderCircle aria-hidden="true" className="lto-activity-ring" />}
          <Bell aria-hidden="true" />
          {unread > 0 && <span aria-hidden="true" className="lto-activity-badge">{unread > 9 ? "9+" : unread}</span>}
        </Button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          align="end"
          sideOffset={8}
          collisionPadding={16}
          aria-labelledby={titleId}
          className="lto-activity-panel z-50 w-[24rem] max-w-[calc(100vw-2rem)] rounded-xl bg-popover text-popover-foreground shadow-md"
        >
          <h2 id={titleId} className="px-4 pt-3 pb-2 text-sm font-semibold">{t("Notifications")}</h2>
          <div className="max-h-[min(70vh,32rem)] overflow-y-auto px-1 pb-1">
            {failed ? (
              <div role="alert" className="flex items-center justify-between gap-3 px-3 py-3 text-sm">
                <span>{t("Activité indisponible.")}</span>
                <Button variant="outline" size="sm" onClick={() => void reload.current()}>{t("Réessayer")}</Button>
              </div>
            ) : !feed ? (
              <p role="status" className="px-3 py-3 text-sm text-muted-foreground">{t("Chargement…")}</p>
            ) : !feed.active.length && !feed.recent.length ? (
              <p className="px-3 py-3 text-sm text-muted-foreground">{t("Aucune opération en cours ni récente.")}</p>
            ) : (
              <>
                {!!feed.active.length && (
                  <section aria-labelledby={runningId}>
                    <h3 id={runningId} className="lto-activity-heading">{t("En cours")}</h3>
                    <ul>{feed.active.map(item => <Row key={item.id} item={item} unread={false} onNavigate={close} />)}</ul>
                  </section>
                )}
                {!!feed.recent.length && (
                  <section aria-labelledby={recentId}>
                    <h3 id={recentId} className="lto-activity-heading">{t("Récent")}</h3>
                    <ul>{feed.recent.map(item => <Row key={item.id} item={item} unread={shown.has(item.id)} onNavigate={close} />)}</ul>
                  </section>
                )}
              </>
            )}
          </div>
          {selectedVtl && (
            <a href={vtlHash("jobs", selectedVtl)} onClick={close} className="lto-activity-footer">{t("Tout voir dans Activité")}</a>
          )}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
