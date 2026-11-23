"use client";
import { translate as t, useI18n, isPlural, getLocale } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { CircleAlert, HardDrive, CloudUpload } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { type Cartridge, type Job, formatBytes, formatDate, isActive, jobLabels, kindLabels, mediaLabels, request } from "@/lib/api";
import { type Telemetry, ageTelemetry, driveLabels } from "@/lib/telemetry";
import { transferProgress } from "@/lib/export-progress";
import { type Tally, type VtlSnapshot, activeDrives, activeJobs, families, family, familyLabels, summarize, tally, usedBytes } from "@/lib/fleet";
import { storageStatus } from "@/lib/tape-storage";
import { LtoBarcodeLabel } from "./lto-barcode-label";
import { LibraryControls } from "./library-controls";
import { VtlScope } from "./vtl-scope";

type Library = { id: string; name: string; lifecycle: string };
const lifecycleLabels: Record<string, string> = { draft: "À configurer", ready: "Prête", failed: "À vérifier", disabled: "Désactivée" };
const rate = (bps: number | null | undefined) => bps == null ? "—" : `${formatBytes(bps)}/s`;
const percent = (value: number | null) => value == null ? "—" : `${new Intl.NumberFormat(getLocale(), { maximumFractionDigits: 1 }).format(value * 100)} %`;

/** Fleet-wide, read-only synthesis; every action stays in its VTL. */
export function Fleet({ libraries, canManage }: { libraries: Library[]; canManage: boolean }) {
  useI18n();
  const [snapshots, setSnapshots] = useState<Record<string, VtlSnapshot>>({});
  const [history, setHistory] = useState<Record<string, number[]>>({});
  const [updated, setUpdated] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const key = libraries.map(l => l.id).join(",");
  const librariesRef = useRef(libraries); librariesRef.current = libraries;
  const reload = useRef<() => Promise<void>>(async () => {});
  useEffect(() => {
    const controller = new AbortController(), signal = controller.signal;
    const patch = (id: string, value: Partial<VtlSnapshot>) => setSnapshots(previous => {
      const library = librariesRef.current.find(l => l.id === id); if (!library) return previous;
      const base: VtlSnapshot = previous[id] ?? { id, name: library.name, lifecycle: library.lifecycle, cartridges: null, telemetry: null, jobs: null, disk: null, failed: false };
      return { ...previous, [id]: { ...base, name: library.name, lifecycle: library.lifecycle, ...value } };
    });
    const scoped = (id: string, path: string) => `/v1/vtls/${encodeURIComponent(id)}${path}`;
    async function slow() {
      await Promise.all(librariesRef.current.map(async ({ id }) => {
        const [cartridges, jobs] = await Promise.allSettled([request<Cartridge[]>(scoped(id, "/cartridges"), { signal }), request<Job[]>(scoped(id, "/jobs"), { signal })]);
        if (signal.aborted) return;
        patch(id, { cartridges: cartridges.status === "fulfilled" && Array.isArray(cartridges.value) ? cartridges.value : null, jobs: jobs.status === "fulfilled" && Array.isArray(jobs.value) ? jobs.value : null, failed: cartridges.status === "rejected" });
      }));
      if (!signal.aborted) setUpdated(Date.now());
    }
    async function fast() {
      await Promise.all(librariesRef.current.map(async ({ id }) => {
        try {
          const value = ageTelemetry(await request<Telemetry>(scoped(id, "/telemetry"), { signal }), Date.now());
          if (signal.aborted) return;
          patch(id, { telemetry: value });
          setHistory(previous => {
            const next = { ...previous };
            for (const drive of value.drives) { const k = `${id}:${drive.id}`; next[k] = [...(next[k] ?? []), drive.write_rate_bps ?? 0].slice(-60); }
            return next;
          });
        } catch { if (!signal.aborted) patch(id, { telemetry: null }); }
      }));
    }
    async function disks() {
      await Promise.all(librariesRef.current.map(async ({ id }) => {
        try { const status = storageStatus(await request<unknown>(scoped(id, "/settings/tape-storage"), { signal }), id); if (!signal.aborted) patch(id, { disk: status.capacity }); }
        catch { /* viewers or unconfigured storage: the disk tile stays partial */ }
      }));
    }
    reload.current = async () => { await Promise.all([slow(), fast()]); };
    void slow(); void fast(); void disks();
    const timers = [setInterval(() => { if (!document.hidden) void fast(); }, 5000), setInterval(() => { if (!document.hidden) void slow(); }, 15000), setInterval(() => { if (!document.hidden) void disks(); }, 60000), setInterval(() => setNow(Date.now()), 1000)];
    return () => { controller.abort(); timers.forEach(clearInterval); };
  }, [key]);
  const vtls = libraries.map(l => snapshots[l.id]).filter((v): v is VtlSnapshot => !!v);
  const loaded = vtls.length === libraries.length && vtls.every(v => v.cartridges !== null || v.failed);
  const summary = summarize(vtls);
  const [opened, setOpened] = useState<{ vtl: string; barcode: string } | null>(null);
  const openedVtl = opened ? vtls.find(v => v.id === opened.vtl) : undefined;
  const openedCartridge = openedVtl?.cartridges?.find(c => c.barcode === opened?.barcode);
  const diskRatio = summary.disk ? summary.disk.used / summary.disk.total : null;
  return <section className="fleet" aria-label={t("Vue du parc")}>
    {!loaded ? <Skeleton className="h-64" /> : <>
      <div className="lto-lcd fleet-readout">
        <header><h2>{t("Parc de {count} VTL", { count: vtls.length })}</h2>{updated && <span className="tabular-nums">{t("Actualisé il y a {seconds} s", { seconds: Math.max(0, Math.round((now - updated) / 1000)) })}</span>}</header>
        <dl>
          <Reading label={t("Cartouches")} value={String(summary.count)} note={t("dans {count} VTL", { count: vtls.length })} />
          <Reading label={t("Volume écrit")} value={formatBytes(summary.written)} note={t("sur {capacity} de capacité", { capacity: formatBytes(summary.capacity) })}>
            <Segments ratio={summary.capacity ? summary.written / summary.capacity : 0} label={t("Volume écrit / capacité")} />
          </Reading>
          <Reading label={t("Protégé dans S3")} value={percent(summary.protectedShare)} note={t("{bytes} sur {total} écrits", { bytes: formatBytes(summary.total.protected.bytes), total: formatBytes(summary.written) })}>
            <Segments ratio={summary.protectedShare ?? 0} label={t("Part du volume protégée dans S3")} />
          </Reading>
          <Reading label={t("Disque local")} value={percent(diskRatio)} note={summary.disk ? t("{free} libres sur {total}", { free: formatBytes(summary.disk.total - summary.disk.used), total: formatBytes(summary.disk.total) }) : t("Capacité indisponible")}>
            {diskRatio != null && <Segments ratio={diskRatio} label={t("Occupation du disque local")} warn={diskRatio > 0.85} />}
          </Reading>
        </dl>
        <section className="fleet-activity" aria-labelledby="fleet-activity">
          <h3 id="fleet-activity">{t("Activité en cours")}</h3>
          <ActivityList vtls={vtls} history={history} />
        </section>
      </div>
      <section aria-labelledby="fleet-vtls">
        <div className="fleet-heading"><h2 id="fleet-vtls">{t("Par VTL")}</h2><Legend /></div>
        <ul className="fleet-rack">{vtls.map(vtl => <RackUnit key={vtl.id} vtl={vtl} canManage={canManage} onRefresh={() => reload.current()} onOpen={barcode => setOpened({ vtl: vtl.id, barcode })} />)}</ul>
      </section>
      {openedVtl && openedCartridge && <CartridgeDetails vtl={openedVtl} cartridge={openedCartridge} onClose={() => setOpened(null)} />}
    </>}
  </section>;
}

function Reading({ label, value, note, children }: { label: string; value: string; note: string; children?: React.ReactNode }) {
  return <div className="fleet-reading"><dt>{label}</dt><dd><strong>{value}</strong>{children}<span>{note}</span></dd></div>;
}

/** LCD bargraph: 24 segments, lit up to the ratio. */
function Segments({ ratio, label, warn }: { ratio: number; label: string; warn?: boolean }) {
  const clamped = Math.max(0, Math.min(1, ratio)), lit = Math.round(clamped * 24);
  return <span role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(clamped * 100)} className="fleet-segments" data-warn={warn || undefined}>
    {Array.from({ length: 24 }, (_, i) => <i key={i} data-lit={i < lit || undefined} />)}
  </span>;
}

const counts = (value: Tally) => families.filter(f => value[f].count).map(f => `${t(familyLabels[f])} ${value[f].count}`).join(", ");

function Legend() {
  return <ul className="fleet-legend" aria-label={t("Légende des états")}>
    {families.map(f => <li key={f}><i aria-hidden="true" style={{ background: `var(--fleet-${f})` }} />{t(familyLabels[f])}</li>)}
  </ul>;
}

/** One rack unit per VTL: status LED, drives, then the magazine with one spine per cartridge (cap = state, fill = usage). */
function RackUnit({ vtl, canManage, onRefresh, onOpen }: { vtl: VtlSnapshot; canManage: boolean; onRefresh: () => Promise<void>; onOpen: (barcode: string) => void }) {
  const value = tally(vtl);
  const live = (vtl.cartridges ?? []).filter(c => !c.local_deleted).toSorted((a, b) => a.barcode.localeCompare(b.barcode));
  const used = families.reduce((s, f) => s + value[f].bytes, 0);
  const capacity = live.reduce((s, c) => s + c.capacity_bytes, 0);
  const drives = vtl.telemetry?.status === "available" ? vtl.telemetry.drives : [];
  const busy = new Set(activeDrives(vtl).map(d => d.id));
  // Blinking LEDs: drive I/O (read/write/move) on the loaded cartridge, network for S3 operations.
  const io = new Map(activeDrives(vtl).flatMap(d => d.barcode ? [[d.barcode, t(driveLabels[d.state])] as const] : []));
  const network = new Map(activeJobs(vtl).map(j => [j.subject, t(kindLabels[j.kind] ?? "Opération en arrière-plan")] as const));
  const free = vtl.telemetry?.status === "available" ? vtl.telemetry.slots.filter(s => s.kind === "storage" && s.position_known && !s.barcode).length : 0;
  const signal = vtl.failed || vtl.lifecycle === "failed" ? "fault" : vtl.lifecycle === "ready" ? "ready" : "idle";
  return <li className="fleet-unit">
    <span className="lto-rack-ear lto-rack-ear-left" aria-hidden="true"><i /><i /></span>
    <div className="fleet-faceplate">
      <a href={`#media?vtl=${encodeURIComponent(vtl.id)}`}><span className="fleet-led" data-signal={signal} aria-hidden="true" /><strong>{vtl.name}</strong></a>
      <p>{t(lifecycleLabels[vtl.lifecycle] ?? "État inconnu")}</p>
      <ul>{drives.map(d => <li key={d.id} data-active={busy.has(d.id) || undefined}><HardDrive aria-hidden="true" /><span>{t(d.label)}</span>{busy.has(d.id) && <i className="fleet-io" data-kind="io" aria-hidden="true" />}<span>{busy.has(d.id) && d.write_rate_bps ? rate(d.write_rate_bps) : t(driveLabels[d.state])}</span></li>)}</ul>
    </div>
    {vtl.failed ? <p className="fleet-unit-error"><CircleAlert aria-hidden="true" />{t("Données indisponibles pour cette VTL.")}</p> : <>
      <div className="fleet-window">
        <div className="fleet-spines" role="group" aria-label={live.length ? `${vtl.name} : ${counts(value)}` : t("{label} : aucune cartouche", { label: vtl.name })}>
          {live.map(c => {
            const bytes = usedBytes(c, vtl.telemetry), ratio = c.capacity_bytes ? Math.min(1, bytes / c.capacity_bytes) : 0;
            const activity = io.get(c.barcode) ?? network.get(c.barcode);
            return <button type="button" key={c.barcode} onClick={() => onOpen(c.barcode)} aria-label={t("Détails de {barcode}", { barcode: c.barcode })} data-activity={io.has(c.barcode) ? "io" : network.has(c.barcode) ? "network" : undefined} title={`${c.barcode} · ${t(mediaLabels[c.state] ?? c.state)} · ${formatBytes(bytes)} / ${formatBytes(c.capacity_bytes)}${activity ? ` · ${activity}` : ""}`} style={{ "--cap": `var(--fleet-${family(c.state)})`, "--fill": `${bytes > 0 ? Math.max(4, ratio * 100) : 0}%` } as React.CSSProperties}>
              {io.has(c.barcode) && <i className="fleet-io" data-kind="io" />}
              {network.has(c.barcode) && <i className="fleet-io" data-kind="network" />}
            </button>;
          })}
          {Array.from({ length: Math.min(free, 200) }, (_, i) => <span key={`free-${i}`} data-free />)}
          {!live.length && !free && <em>{t("Magasin vide")}</em>}
        </div>
        <p>{t(isPlural(live.length) ? "{count} cartouches" : "{count} cartouche", { count: live.length })}{live.length > 0 && <> : {counts(value)}</>}{free > 0 && <>. {t(isPlural(free) ? "{count} emplacements libres" : "{count} emplacement libre", { count: free })}</>}{value.failed.count > 0 && <CircleAlert aria-label={t("Échec")} className="fleet-alert" />}</p>
      </div>
      <div className="fleet-usage">
        <strong>{formatBytes(used)}</strong><span>{t("sur {capacity}", { capacity: formatBytes(capacity) })}</span>
        <div className="lto-gauge" role="meter" aria-label={t("Occupation de {name}", { name: vtl.name })} aria-valuemin={0} aria-valuemax={100} aria-valuenow={capacity ? Math.round(used / capacity * 100) : 0}><span style={{ width: `${capacity ? Math.min(100, used / capacity * 100) : 0}%` }} /></div>
      </div>
    </>}
    <div className="fleet-controls" role="group" aria-label={t("Commandes de {name}", { name: vtl.name })}>
      <VtlScope.Provider value={vtl.id}><LibraryControls routed={false} canManage={canManage} connected={!vtl.failed && vtl.cartridges !== null} jobs={vtl.jobs ?? []} onRefresh={onRefresh} data={vtl.telemetry} /></VtlScope.Provider>
    </div>
    <span className="lto-rack-ear lto-rack-ear-right" aria-hidden="true"><i /><i /></span>
  </li>;
}

/** Read-only cartridge sheet; actions stay in the cartridge's own library. */
function CartridgeDetails({ vtl, cartridge, onClose }: { vtl: VtlSnapshot; cartridge: Cartridge; onClose: () => void }) {
  const used = usedBytes(cartridge, vtl.telemetry), ratio = cartridge.capacity_bytes ? Math.min(1, used / cartridge.capacity_bytes) : null;
  const telemetry = vtl.telemetry?.status === "available" ? vtl.telemetry : null;
  const drive = telemetry?.drives.find(d => d.barcode === cartridge.barcode);
  const slot = telemetry?.slots.find(s => s.position_known && s.barcode === cartridge.barcode);
  const place = drive ? `${t(drive.label)} · ${t(driveLabels[drive.state])}`
    : slot ? t(slot.kind === "storage" ? "Emplacement {slot}" : "Chariot d’export, emplacement {slot}", { slot: String(slot.number).padStart(2, "0") })
    : t("Position non connue");
  const operations = (vtl.jobs ?? []).filter(j => j.subject === cartridge.barcode).toSorted((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, 5);
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent className="fleet-palette sm:max-w-lg">
      <DialogHeader icon={HardDrive}>
        <DialogTitle>{t("Cartouche {barcode}", { barcode: cartridge.barcode })}</DialogTitle>
        <DialogDescription>{vtl.name}</DialogDescription>
      </DialogHeader>
      <div className="fleet-sheet">
        <div className="lto-label fleet-sheet-label"><LtoBarcodeLabel barcode={cartridge.barcode} /></div>
        <dl>
          <dt>{t("État")}</dt><dd><i aria-hidden="true" style={{ background: `var(--fleet-${family(cartridge.state)})` }} />{cartridge.local_deleted ? t("Supprimée localement") : t(mediaLabels[cartridge.state] ?? cartridge.state)}</dd>
          <dt>{t("Position")}</dt><dd>{place}</dd>
          <dt>{t("Type")}</dt><dd>{cartridge.media_type?.replace("LTO", "LTO-") ?? "—"}</dd>
          <dt>{t("Occupation")}</dt><dd className="fleet-sheet-usage"><span>{formatBytes(used)} / {formatBytes(cartridge.capacity_bytes)}{ratio != null && ` · ${percent(ratio)}`}</span>{ratio != null && <span className="lto-gauge" role="meter" aria-label={t("Occupation de {name}", { name: cartridge.barcode })} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(ratio * 100)}><span style={{ width: `${ratio * 100}%` }} /></span>}</dd>
          <dt>{t("Génération")}</dt><dd>{cartridge.generation}</dd>
        </dl>
        <section aria-labelledby="fleet-sheet-operations">
          <h3 id="fleet-sheet-operations">{t("Dernières opérations")}</h3>
          {operations.length ? <ul>{operations.map(j => { const p = isActive(j) ? transferProgress(j) : null; return <li key={j.id}><span>{t(kindLabels[j.kind] ?? "Opération en arrière-plan")}</span><span>{p?.percent != null ? `${p.percent} %` : t(jobLabels[j.status] ?? j.status)}</span><time>{formatDate(j.created_at)}</time></li>; })}</ul> : <p>{t("Aucune opération sur cette cartouche.")}</p>}
        </section>
      </div>
      <DialogFooter>
        <Button asChild><a href={`#media?vtl=${encodeURIComponent(vtl.id)}&barcode=${encodeURIComponent(cartridge.barcode)}`}>{t("Ouvrir dans {vtl}", { vtl: vtl.name })}</a></Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}

function Trace({ points, label }: { points: number[]; label: string }) {
  const max = Math.max(1, ...points), w = 120, h = 24;
  const path = points.map((p, i) => `${i ? "L" : "M"}${(i / Math.max(1, points.length - 1)) * w},${h - 2 - (p / max) * (h - 4)}`).join(" ");
  return <svg viewBox={`0 0 ${w} ${h}`} className="fleet-trace" role="img" aria-label={label}><path d={path} /></svg>;
}

function ActivityList({ vtls, history }: { vtls: VtlSnapshot[]; history: Record<string, number[]> }) {
  const drives = vtls.flatMap(v => activeDrives(v).map(d => ({ v, d })));
  const jobs = vtls.flatMap(v => activeJobs(v).map(j => ({ v, j, p: transferProgress(j) })));
  if (!drives.length && !jobs.length) return <p className="fleet-idle">{t("Aucune activité en cours sur le parc.")}</p>;
  return <ul className="fleet-operations">
    {drives.map(({ v, d }) => <li key={`${v.id}:${d.id}`}>
      <HardDrive aria-hidden="true" />
      <div><p>{v.name} · {t(d.label)} · {t(driveLabels[d.state])}</p><p>{d.barcode ?? "—"}</p></div>
      <Trace points={history[`${v.id}:${d.id}`] ?? []} label={t("Débit récent de {label}", { label: `${v.name} ${t(d.label)}` })} />
      <strong>{rate(d.write_rate_bps)}</strong>
    </li>)}
    {jobs.map(({ v, j, p }) => <li key={`${v.id}:${j.id}`}>
      <CloudUpload aria-hidden="true" />
      <div><p>{v.name} · {t(kindLabels[j.kind] ?? "Opération en arrière-plan")}</p><p>{j.subject}</p></div>
      {p?.percent != null ? <span role="progressbar" aria-label={t("Progression de {subject}", { subject: j.subject })} aria-valuenow={p.percent} aria-valuemin={0} aria-valuemax={100} className="fleet-segments">{Array.from({ length: 24 }, (_, i) => <i key={i} data-lit={i < Math.round(p.percent! / 100 * 24) || undefined} />)}</span> : <span />}
      <strong>{p?.percent != null ? `${p.percent} %` : t(p?.label ?? "En cours")}</strong>
    </li>)}
  </ul>;
}
