"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useEffect, useId, useState } from "react";
import { Popover } from "radix-ui";
import { Database } from "lucide-react";
import { formatBytes, request } from "@/lib/api";
import { type StorageGauge as Gauge, gaugeWidths, storageGauge } from "@/lib/storage-gauge";

const REFRESH_MS = 60000;
const parts = [
  { key: "occupied", label: "Occupé" },
  { key: "reserved", label: "Réservé aux cartouches" },
  { key: "available", label: "Libre à provisionner" },
] as const;

/** Gauge of a VTL's volume, in the status bar or in a row of the VTL list; `revision` changes when the library does. */
export function StorageGauge({ vtlId, revision = "", variant = "status" }: { vtlId: string | null; revision?: string; variant?: "status" | "row" }) {
  useI18n();
  const titleId = useId();
  const [gauge, setGauge] = useState<Gauge | null>(null);
  useEffect(() => setGauge(null), [vtlId]);
  useEffect(() => {
    if (!vtlId) return;
    let pending: AbortController | null = null;
    async function load() {
      if (document.hidden || pending) return;
      const controller = new AbortController();
      pending = controller;
      try {
        const options = await request<{ storage?: unknown }>(`/v1/vtls/${encodeURIComponent(vtlId!)}/cartridges/options`, { signal: AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]) });
        if (!controller.signal.aborted) setGauge(storageGauge(options?.storage));
      } catch {
        // Unready VTL, unreadable volume or controller error: the gauge stays hidden.
        if (!controller.signal.aborted) setGauge(null);
      } finally {
        if (pending === controller) pending = null;
      }
    }
    const timer = setInterval(() => void load(), REFRESH_MS);
    const visibility = () => { if (!document.hidden) void load(); };
    document.addEventListener("visibilitychange", visibility);
    void load();
    return () => {
      clearInterval(timer);
      pending?.abort();
      document.removeEventListener("visibilitychange", visibility);
    };
  }, [vtlId, revision]);
  if (!gauge) return null;
  const free = formatBytes(gauge.available);
  const total = formatBytes(gauge.total);
  const widths = gaugeWidths(gauge);
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button type="button" className="lto-storage-gauge" data-variant={variant} data-level={gauge.level} aria-label={t("Stockage local : {free} libres à provisionner", { free })} title={t("Stockage local : {free} libres à provisionner", { free })}>
          <Database aria-hidden="true" />
          <span className="lto-storage-gauge-bar" aria-hidden="true">
            {parts.map(part => <span key={part.key} data-part={part.key} style={{ width: `${widths[part.key]}%` }} />)}
          </span>
          <span className="lto-storage-gauge-text" aria-hidden="true">{t("{free} à provisionner sur {total}", { free, total })}</span>
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content side={variant === "status" ? "top" : "bottom"} align="end" sideOffset={8} collisionPadding={16} aria-labelledby={titleId}
          className="z-50 w-[20rem] max-w-[calc(100vw-2rem)] rounded-xl bg-popover p-4 text-popover-foreground shadow-md">
          <h2 id={titleId} className="text-sm font-semibold">{t("Stockage local")}</h2>
          <dl className="lto-storage-gauge-details">
            {parts.map(part => (
              <div key={part.key}>
                <dt><i aria-hidden="true" data-part={part.key} />{t(part.label)}</dt>
                <dd>{formatBytes(gauge[part.key])}</dd>
              </div>
            ))}
            <div className="lto-storage-gauge-total"><dt>{t("Volume total")}</dt><dd>{formatBytes(gauge.total)}</dd></div>
          </dl>
          {gauge.shortfall > 0 && (
            <p role="alert" className="mt-3 text-sm text-destructive">
              {t("Les cartouches existantes ne pourront pas toutes se remplir : il manque {missing}.", { missing: formatBytes(gauge.shortfall) })}
            </p>
          )}
          <p className="mt-3 text-xs text-muted-foreground">{t("Les VTL qui partagent ce volume sont comptées ensemble.")}</p>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
