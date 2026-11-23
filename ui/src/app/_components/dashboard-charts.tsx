"use client";
import {useI18n,getLocale,isPlural} from '@/lib/i18n';

import { useState } from "react";
import {
  Line, LineChart, CartesianGrid, Cell, Pie, PieChart,
  ResponsiveContainer, Tooltip, XAxis, YAxis,
} from "recharts";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { type Snapshot, mediaLabels } from "@/lib/api";

const colors = {
  completed: "var(--dashboard-success)",
  active: "var(--dashboard-active)",
  failed: "var(--dashboard-danger)",
  other: "var(--dashboard-neutral)",
};
const outcomes = [
  { key: "completed", name: "Terminées", fill: colors.completed },
  { key: "active", name: "En cours / en attente", fill: colors.active },
  { key: "failed", name: "En échec", fill: colors.failed },
  { key: "other", name: "Autres", fill: colors.other },
] as const;
type Outcome = typeof outcomes[number]["key"];
type Slice = { name: string; value: number; fill: string };
const stateColors: Record<string, string> = {
  verified: colors.completed, failed: colors.failed, writable: colors.active,
  sealed: "var(--dashboard-amber)", archived: "var(--dashboard-violet)",
  archiving: "var(--dashboard-violet)", uploading: "var(--dashboard-cyan)",
  restoring: "var(--dashboard-amber)", hydrating: "var(--dashboard-cyan)",
  online: colors.completed,
};
const tooltipStyle = {
  background: "var(--popover)", color: "var(--popover-foreground)",
  border: "1px solid var(--border)", borderRadius: 10, fontSize: 12,
};
const dayKey = (date: Date) => `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
const outcome = (status: string): Outcome => status === "completed" || status === "failed"
  ? status : ["queued", "running"].includes(status) ? "active" : "other";

function Donut({ slices, total, center, label }: {
  slices: Slice[]; total: number; center: string; label: string;
}) {
 const {t}=useI18n();
  return <>
    <div className="relative mx-auto h-52 w-full max-w-72" role="img" aria-label={label}>
      <ResponsiveContainer width="100%" height="100%" minWidth={0}>
        <PieChart>
          <Pie data={total ? slices.filter(slice => slice.value > 0) : [{ name: t("Aucune donnée"), value: 1, fill: "var(--muted)" }]}
            dataKey="value" nameKey="name" innerRadius="68%" outerRadius="90%"
            startAngle={90} endAngle={-270} stroke="var(--card)" strokeWidth={3} isAnimationActive={false}>
            {(total ? slices.filter(slice => slice.value > 0) : [{ name: "empty", fill: "var(--muted)" }])
              .map(slice => <Cell key={slice.name} fill={slice.fill} />)}
          </Pie>
          {!!total && <Tooltip contentStyle={tooltipStyle} itemStyle={{ color: "var(--popover-foreground)" }} />}
        </PieChart>
      </ResponsiveContainer>
      <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center" aria-hidden="true">
        <span className="text-4xl font-semibold tracking-tight tabular-nums">{total}</span>
        <span className="mt-1 text-xs text-muted-foreground">{center}</span>
      </div>
    </div>
    <ul className="mt-3 space-y-2.5 text-sm">
      {slices.map(slice => <li key={slice.name} className="flex items-center gap-2">
        <span className="size-2.5 shrink-0 rounded-full" style={{ background: slice.fill }} />
        <span className="min-w-0 flex-1 text-muted-foreground">{slice.name}</span>
        <span className="font-medium tabular-nums">{slice.value}</span>
        <span className="w-10 text-right text-xs tabular-nums text-muted-foreground">{total ? Math.round(slice.value / total * 100) : 0}%</span>
      </li>)}
    </ul>
  </>;
}

export function DashboardCharts({ data }: { data: Snapshot }) {
 const {t}=useI18n();
  const [period, setPeriod] = useState(7);
  const now = new Date();
  const days = Array.from({ length: period }, (_, index) => {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate() - period + 1 + index);
    return {
      key: dayKey(date),
      date: date.toLocaleDateString(getLocale(), { day: "numeric", month: "short" }),
      completed: 0, active: 0, failed: 0, other: 0,
    };
  });
  const byDay = new Map(days.map(day => [day.key, day]));
  const totals = { completed: 0, active: 0, failed: 0, other: 0 };
  for (const job of data.jobs) {
    // SQLite timestamps without a timezone represent UTC.
    const date = new Date(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(job.created_at)
      ? job.created_at.replace(" ", "T") + "Z" : job.created_at);
    const day = byDay.get(dayKey(date));
    if (!day || date > now) continue;
    const key = outcome(job.status);
    day[key] += 1;
    totals[key] += 1;
  }
  const total = Object.values(totals).reduce((sum, count) => sum + count, 0);
  const visibleOutcomes = outcomes.filter(item => item.key !== "other" || totals.other > 0);
  const counts = new Map<string, number>();
  for (const media of data.cartridges) counts.set(media.state, (counts.get(media.state) ?? 0) + 1);
  const mediaSlices = [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([state, value]) => ({
    name: t(mediaLabels[state] ?? state), value, fill: stateColors[state] ?? colors.other,
  }));

  return <section aria-label={t("Graphiques de la bibliothèque")} className="space-y-3">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <h2 className="text-base font-semibold">{t("La bibliothèque en chiffres")}</h2>
      <div className="flex gap-1 rounded-lg border p-1" role="group" aria-label={t("Période des opérations")}>
        {[7, 30].map(value => <Button key={value} size="sm" variant={period === value ? "secondary" : "ghost"}
          aria-pressed={period === value} onClick={() => setPeriod(value)}>{t("{count} jours", {count:value})}</Button>)}
      </div>
    </div>
    <div className="grid items-stretch gap-4 md:grid-cols-2 2xl:grid-cols-4">
      <Card className="min-w-0 md:col-span-2">
        <CardHeader>
          <CardTitle>{t("Activité par jour")}</CardTitle>
          <CardDescription>{t("Opérations créées sur {count} jours, réparties selon leur état actuel.", {count:period})}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-5">
          <div className="flex items-baseline gap-2"><span className="text-3xl font-semibold tabular-nums">{total}</span><span className="text-sm text-muted-foreground">{t(isPlural(total)?"opérations sur la période":"opération sur la période")}</span></div>
          {total ? <div className="h-64 w-full" aria-label={t("Nombre d’opérations par jour")}>
            <ResponsiveContainer width="100%" height="100%" minWidth={0}>
              <LineChart data={days} accessibilityLayer margin={{ top: 8, right: 4, bottom: 0, left: -24 }}>
                <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="3 3" />
                <XAxis dataKey="date" axisLine={false} tickLine={false} minTickGap={24} tickMargin={10} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                <YAxis allowDecimals={false} axisLine={false} tickLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                <Tooltip contentStyle={tooltipStyle} cursor={{ stroke: "var(--muted-foreground)", strokeDasharray: "3 3" }} />
                {visibleOutcomes.map(item => <Line key={item.key} type="monotone" dataKey={item.key} name={t(item.name)} stroke={item.fill} strokeWidth={2.5} dot={period === 7 ? { r: 3, fill: item.fill, strokeWidth: 0 } : false} activeDot={{ r: 5, stroke: "var(--card)", strokeWidth: 2 }} isAnimationActive={false} />)}
              </LineChart>
            </ResponsiveContainer>
          </div> : <div className="flex h-64 items-center justify-center rounded-lg border border-dashed text-sm text-muted-foreground">{t("Aucune opération sur cette période.")}</div>}
          <div className="flex flex-wrap gap-x-5 gap-y-2 text-xs text-muted-foreground">
            {visibleOutcomes.map(item => <span key={item.key} className="flex items-center gap-2"><span className="size-2.5 rounded-full" style={{ background: item.fill }} />{t(item.name)}</span>)}
          </div>
          <details className="text-xs text-muted-foreground">
            <summary className="cursor-pointer">{t("Voir les valeurs par jour")}</summary>
            <div className="mt-3 overflow-x-auto"><table className="w-full text-left tabular-nums">
              <caption className="sr-only">{t("Opérations créées par jour, état actuel")}</caption>
              <thead><tr><th className="p-2">{t("Jour")}</th>{visibleOutcomes.map(item => <th className="p-2" key={item.key}>{t(item.name)}</th>)}</tr></thead>
              <tbody>{days.map(day => <tr key={day.key}><th className="p-2 font-normal">{day.date}</th>{visibleOutcomes.map(item => <td className="p-2" key={item.key}>{day[item.key]}</td>)}</tr>)}</tbody>
            </table></div>
          </details>
        </CardContent>
      </Card>
      <Card className="min-w-0">
        <CardHeader><CardTitle>{t("État des cartouches")}</CardTitle><CardDescription>{t("Répartition actuelle du catalogue.")}</CardDescription></CardHeader>
        <CardContent>
          <Donut slices={mediaSlices} total={data.cartridges.length} center={t("cartouches")} label={t("Répartition des cartouches par état")} />
          {!data.cartridges.length && <p className="mt-3 text-center text-sm text-muted-foreground">{t("Aucune cartouche dans le catalogue.")}</p>}
        </CardContent>
      </Card>
      <Card className="min-w-0">
        <CardHeader><CardTitle>{t("Résultat des opérations")}</CardTitle><CardDescription>{t("Opérations créées sur {count} jours.", {count:period})}</CardDescription></CardHeader>
        <CardContent>
          <Donut slices={visibleOutcomes.map(item => ({ ...item, name:t(item.name), value: totals[item.key] }))} total={total} center={t("opérations")} label={t("Répartition des opérations par état")} />
        </CardContent>
      </Card>
    </div>
  </section>;
}
