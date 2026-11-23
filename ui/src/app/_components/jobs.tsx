"use client";
import {useI18n,isPlural} from '@/lib/i18n';
import { JobDetail } from "./job-detail";
import { transferProgress } from "@/lib/export-progress";

import { useEffect, useState } from "react";
import { useScopedRequest } from "./vtl-scope";
import {
  Activity,
  ArrowRight,
  ChevronLeft,
  ChevronRight,
  X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectOption,
} from "@/components/ui/select";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  type Job,
  formatDate,
  jobLabels,
  jobSummary,
  isActive,
  kindLabels,
} from "@/lib/api";
import { StatusBadge } from "./status-badge";

export function Jobs({
  jobs,
  compact = false,
  onShowAll,
}: {
  jobs: Job[];
  compact?: boolean;
  onShowAll?: () => void;
}) {
 const {t}=useI18n();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const request = useScopedRequest();
  const [linkedJob, setLinkedJob] = useState<Job | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  useEffect(() => {
    if (compact) return;
    let active = true;
    const readLink = () => {
      const [route, query = ""] = location.hash.slice(1).split("?");
      const id = new URLSearchParams(query).get("job");
      setLinkedJob(null); setLinkError(null);
      if (route !== "jobs" || !id) return;
      setSelectedId(id);
      void request<Job>(`/v1/jobs/${encodeURIComponent(id)}`).then(job => {
        if (active) setLinkedJob(job);
      }).catch(cause => {
        if (active) setLinkError(cause instanceof Error ? cause.message : "Opération introuvable.");
      });
    };
    readLink(); window.addEventListener("hashchange", readLink);
    return () => { active = false; window.removeEventListener("hashchange", readLink); };
  }, [compact, request]);
  const selected = jobs.find((job) => job.id === selectedId) ?? (linkedJob?.id === selectedId ? linkedJob : undefined);
  const filtered = [...jobs]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .filter(
      (job) =>
        (filter === "all" || job.status === filter) &&
        `${job.id} ${job.subject} ${job.kind}`
          .toLowerCase()
          .includes(query.trim().toLowerCase()),
    );
  const pageSize = compact ? 4 : 10;
  const lastPage = Math.max(0, Math.ceil(filtered.length / pageSize) - 1);
  const currentPage = Math.min(page, lastPage);
  const visible = filtered.slice(
    currentPage * pageSize,
    (currentPage + 1) * pageSize,
  );
  return (
    <>
      {linkError && <p role="alert" className="text-sm text-destructive">{t(linkError)}</p>}
      <Card>
        <CardHeader className={!compact ? "flex flex-wrap items-start justify-between gap-3" : undefined}>
          <div className="space-y-1">
          <CardTitle>
            {compact ? t("Activité récente") : t("Historique des opérations")}
          </CardTitle>
          <CardDescription>
            {compact
              ? t("Les dernières opérations de votre bibliothèque.")
              : t(isPlural(jobs.length)?"{count} opérations enregistrées.":"{count} opération enregistrée.",{count:jobs.length})}
          </CardDescription>
          </div>
          {compact && (
            <CardAction>
              <Button variant="ghost" size="sm" onClick={onShowAll}>
                {t("Tout voir")}<ArrowRight data-icon="inline-end" />
              </Button>
            </CardAction>
          )}
          {!compact && (
          <div className="flex w-full min-w-0 flex-wrap items-center gap-3 sm:w-auto">
            <Input
              aria-label={t("Rechercher une opération")}
              placeholder={t("Rechercher par cartouche ou identifiant…")}
              className="min-w-40 flex-1 sm:w-64 lg:w-80"
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setPage(0);
              }}
            />
            <Select
              aria-label={t("Filtrer les opérations par état")}
              value={filter}
              onValueChange={(selectedValue) => {
                setFilter(selectedValue);
                setPage(0);
              }}
            >
              <SelectOption value="all">
                {t("Tous les états")}</SelectOption>
              {Object.entries(jobLabels).map(([key, label]) => (
                <SelectOption key={key} value={key}>
                  {t(label)}
                </SelectOption>
              ))}
            </Select>
          </div>
          )}
        </CardHeader>
        <CardContent>
          {visible.length ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("Opération")}</TableHead>
                  <TableHead>{t("État")}</TableHead>
                  <TableHead>{t("Étape et progression")}</TableHead>
                  <TableHead>{t("Création")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {visible.map((job) => (
                  <TableRow
                    key={job.id}
                    tabIndex={0}
                    aria-label={t("Détails du job {id}", {id:job.id})}
                    aria-haspopup="dialog"
                    className="cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
                    onClick={() => setSelectedId(job.id)}
                    onKeyDown={event => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        setSelectedId(job.id);
                      }
                    }}
                  >
                    <TableCell className="py-4">
                      <div className="flex items-center gap-3">
                        <div className="flex size-9 items-center justify-center rounded-lg border">
                          <Activity className="size-4 text-muted-foreground" />
                        </div>
                        <div>
                          <p className="font-medium">
                            {t(kindLabels[job.kind] ?? job.kind)}
                          </p>
                          <p className="mt-0.5 font-mono text-xs text-muted-foreground">
                            {job.subject}
                          </p>
                        </div>
                      </div>
                    </TableCell>
                    <TableCell>
                      <StatusBadge state={job.status} job />
                    </TableCell>
                    <TableCell>
                      {isActive(job) ? <div className="space-y-1 text-xs text-muted-foreground"><span>{transferProgress(job) ? t(transferProgress(job)!.label) : jobSummary(job)}</span>{transferProgress(job)?.percent != null && <p className="tabular-nums">{transferProgress(job)!.percent} %</p>}</div> : <span className="text-xs text-muted-foreground">{job.status === 'completed' ? '100 %' : jobSummary(job)}</span>}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground">
                      {formatDate(job.created_at)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : (
            <Empty>
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <Activity />
                </EmptyMedia>
                <EmptyTitle>
                  {jobs.length
                    ? t("Aucune opération correspondante")
                    : t("Aucune opération pour le moment")}
                </EmptyTitle>
                <EmptyDescription>
                  {jobs.length
                    ? t("Modifiez la recherche ou le filtre d’état.")
                    : t("Les exports apparaîtront ici dès leur lancement.")}
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          )}
        </CardContent>
        {!compact && (
          <CardFooter className="flex-wrap justify-between gap-3">
            <span className="text-xs text-muted-foreground">
              {t(isPlural(filtered.length)?"{count} résultats":"{count} résultat",{count:filtered.length})}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="icon-sm"
                aria-label={t("Page précédente des opérations")}
                disabled={!currentPage}
                onClick={() => setPage(currentPage - 1)}
              >
                <ChevronLeft />
              </Button>
              <span className="text-xs">
                {t("Page {page} sur {total}", {page:currentPage+1,total:lastPage+1})}
              </span>
              <Button
                variant="outline"
                size="icon-sm"
                aria-label={t("Page suivante des opérations")}
                disabled={currentPage === lastPage}
                onClick={() => setPage(currentPage + 1)}
              >
                <ChevronRight />
              </Button>
            </div>
          </CardFooter>
        )}
      </Card>
      <Dialog
        open={!!selected}
        onOpenChange={(open) => {
          if (!open) setSelectedId(null);
        }}
      >
        <DialogContent
          className="max-h-[85dvh] overflow-y-auto sm:max-w-xl"
          showCloseButton={false}
        >
          <DialogHeader icon={Activity} className="pr-12">
            <DialogTitle>{selected && `${t(kindLabels[selected.kind] ?? selected.kind)} · ${selected.subject}`}</DialogTitle>
            <DialogDescription className="sr-only">
              {selected &&
                t("Détail de l’opération")}
            </DialogDescription>
          </DialogHeader>
          <DialogClose asChild>
            <Button
              variant="ghost"
              size="icon-sm"
              className="absolute top-3 right-3"
              aria-label={t("Fermer les détails")}
            >
              <X />
            </Button>
          </DialogClose>
          {selected && <JobDetail key={selected.id} job={selected}/>}
        </DialogContent>
      </Dialog>
    </>
  );
}
