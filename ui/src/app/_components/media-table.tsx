"use client";
import {useI18n,isPlural} from '@/lib/i18n';
import { useScopedRequest } from "./vtl-scope";

import { type ReactNode, useEffect, useRef, useState } from "react";
import {
  ArrowDownAZ,
  ArrowUpAZ,
  ChevronLeft,
  ChevronRight,
  CloudUpload,
  HardDrive,
  LoaderCircle,
  LibraryBig,
  ListChecks,
  Table2,
  X,
} from "lucide-react";
import { localizedToast as toast } from "./providers";
import { MediaDemoActions } from "./media-demo-actions";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectOption,
} from "@/components/ui/select";
import { Progress } from "@/components/ui/progress";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  type Cartridge,
  type Job,
  formatBytes,
  isActive,
  mediaLabels,
} from "@/lib/api";
import {CreateCartridge} from './create-cartridge';
import { DeleteCartridges } from "./delete-cartridges";
import { CartridgeDialog } from "./cartridge-dialog";
import { StatusBadge } from "./status-badge";

import type { useTelemetry } from './use-telemetry';
import { MediaLibrary } from './media-library';
import { MediaRateSparkline } from './media-rate-sparkline';
import { driveLabels, occupancy } from '@/lib/telemetry';

type Action = { barcodes: string[]; excluded: number; syncing: string[] };

export function MediaTable({
  cartridges,
  jobs,
  connected,
  canManage,
  onRefresh,
  actions,
  refreshAction,
  telemetry,
}: {
  cartridges: Cartridge[];
  jobs: Job[];
  connected: boolean;
  canManage: boolean;
  onRefresh: () => Promise<void>;
  actions?: ReactNode;
  refreshAction?: ReactNode;
  telemetry: ReturnType<typeof useTelemetry>;
}) {
 const {t}=useI18n();
  const request = useScopedRequest();
  const [view, setView] = useState<'library'|'table'>('library');
  // A barcode in the URL (from the fleet view) opens that cartridge without filtering the magazine.
  const [detailBarcode, setDetailBarcode] = useState<string | null>(() => typeof location === "undefined" ? null : new URLSearchParams(location.hash.split("?")[1] ?? "").get("barcode"));
  useEffect(() => {
    const [path, search = ""] = location.hash.split("?"), params = new URLSearchParams(search);
    if (!params.has("barcode")) return;
    params.delete("barcode");
    history.replaceState(history.state, "", `${location.pathname}${location.search}${path}${params.size ? `?${params}` : ""}`);
  }, []);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [descending, setDescending] = useState(false);
  const [page, setPage] = useState(0);
  const [selectedBarcodes, setSelectedBarcodes] = useState<Set<string>>(new Set());
  const [action, setAction] = useState<Action | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<{barcode:string;message:string} | null>(null);
  const [accepted, setAccepted] = useState<string[]>([]);
  const submitting = useRef(false);
  const [destination, setDestination] = useState<string | null>(null);
  // null: the client type is unknown, the wording stays neutral.
  const [client, setClient] = useState<string | null>(null);
  useEffect(() => {
    if (!action) return;
    const controller = new AbortController();
    setDestination(null); setClient(null);
    void request<{ rotation_due: boolean; active_bucket: string | null }>("/v1/storage/schedule", { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setDestination(value.rotation_due ? t("nouveau bucket de la période, créé à l’envoi") : value.active_bucket ?? t("aucune destination active")); })
      .catch(cause => { if (!controller.signal.aborted) setDestination(t("Destination indisponible.")); console.error(cause); });
    void request<{ client?: string | null }>("/v1/client", { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setClient(value.client === "veeam" ? "Veeam" : value.client === "pbs" ? "PBS" : null); })
      .catch(cause => { if (!controller.signal.aborted) console.error(cause); });
    return () => controller.abort();
  }, [action, request, t]);
  const filtered = cartridges
    .filter(
      (media) =>
        (filter === "deleted" ? media.local_deleted : !media.local_deleted && (filter === "all" || media.state === filter)) &&
        media.barcode.toLowerCase().includes(query.trim().toLowerCase()),
    )
    .sort((a, b) => a.barcode.localeCompare(b.barcode) * (descending ? -1 : 1));
  const lastPage = Math.max(0, Math.ceil(filtered.length / 10) - 1);
  const currentPage = Math.min(page, lastPage);
  const visible = filtered.slice(currentPage * 10, currentPage * 10 + 10);
  const selection = cartridges.filter(media => selectedBarcodes.has(media.barcode));
  const visibleSelected = visible.filter(media => selectedBarcodes.has(media.barcode)).length;
  const filteredSelected = filtered.filter(media => selectedBarcodes.has(media.barcode)).length;
  useEffect(() => {
    const existing = new Set(cartridges.map(media => media.barcode));
    setSelectedBarcodes(previous => {
      if ([...previous].every(barcode => existing.has(barcode))) return previous;
      return new Set([...previous].filter(barcode => existing.has(barcode)));
    });
  }, [cartridges]);
  function selectMedia(media: Cartridge[], checked: boolean) {
    setSelectedBarcodes(previous => {
      const next = new Set(previous);
      for (const item of media) {
        if (checked) next.add(item.barcode);
        else next.delete(item.barcode);
      }
      return next;
    });
  }
  const activeFor = (barcode: string) =>
    jobs.some((job) => job.subject === barcode && isActive(job));
  // Its own synchronisation is not "another operation": it is reported as such.
  const otherActiveFor = (barcode: string) =>
    jobs.some((job) => job.subject === barcode && isActive(job) && job.kind !== "reconcile");
  const resumableExport = (media: Cartridge) =>
    media.state === "failed" && media.export_resumable === true;
  const eligibleOnceSynchronised = (media: Cartridge) =>
    !media.local_deleted && !otherActiveFor(media.barcode) && (media.state === "writable" || media.state === "sealed" || resumableExport(media));
  const eligibleFor = (media: Cartridge) =>
    eligibleOnceSynchronised(media) && !activeFor(media.barcode) && !media.reconcile_pending;
  const exportable = selection.filter(eligibleFor);
  const detailMedia = cartridges.find(media => media.barcode === detailBarcode);
  const exportDisabledReason = !connected ? "Connexion interrompue. Attendez le retour de la connexion."
    : !canManage ? "Votre compte ne permet pas d’envoyer des cartouches vers S3."
    : busy || (detailBarcode && otherActiveFor(detailBarcode)) ? "Une opération est en cours sur cette cartouche. Consultez son activité."
    : detailMedia?.reconcile_pending ? "La cartouche se synchronise avec le bucket. L’envoi sera possible à la fin de la synchronisation."
    : detailBarcode && activeFor(detailBarcode) ? "Une opération est en cours sur cette cartouche. Consultez son activité."
    : !detailMedia || !eligibleFor(detailMedia) ? "Cette cartouche n’est pas disponible pour un envoi. Consultez son état dans la bibliothèque."
    : null;
  const allowed =
    !!action &&
    action.barcodes.length > 0 &&
    connected &&
    canManage &&
    !actionError &&
    action.barcodes.every(barcode => {
      const media = cartridges.find(item => item.barcode === barcode);
      return !!media && eligibleFor(media);
    });
  function propose(barcodes: string[], excluded = 0, syncing: string[] = []) {
    setAction({ barcodes, excluded: Math.max(0, excluded - syncing.length), syncing });
    setAcknowledged(false);
    setActionError(null);
    setAccepted([]);
  }
  async function submit() {
    if (
      !action ||
      !allowed ||
      submitting.current ||
      !acknowledged
    )
      return;
    submitting.current = true;
    setBusy(true);
    setActionError(null);
    const completed: string[] = [];
    let current = action.barcodes[0];
    try {
      for (const barcode of action.barcodes) {
        current = barcode;
        await request(
          `/v1/cartridges/${encodeURIComponent(barcode)}/export`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: "{}",
            // The first export of a period may create the period's bucket before answering.
            signal: AbortSignal.timeout(330000),
          },
        );
        completed.push(barcode);
        setAccepted([...completed]);
        setSelectedBarcodes(previous => {
          const next = new Set(previous);
          next.delete(barcode);
          return next;
        });
      }
      toast.success(
        completed.length === 1 ? t("Envoi de {barcode} lancé.", {barcode:completed[0]}) : t("{count} envois lancés.", {count:completed.length}),
        { action: { label: t("Voir l’activité"), onClick: () => { location.hash = "jobs"; } } },
      );
      setAction(null);
    } catch (cause) {
      const message =
        cause instanceof Error && cause.name === "TimeoutError" ? "La réponse se fait attendre. L’envoi a peut-être été lancé. Consultez l’activité."
        : cause instanceof Error ? cause.message : t("L’opération a échoué.");
      setActionError({barcode:current,message});
    } finally {
      // Stop on the first error; a lost response may follow an accepted request.
      try {
        await onRefresh();
      } finally {
        setBusy(false);
        submitting.current = false;
      }
    }
  }
  const locationOf = (barcode: string) => {
    const drive = telemetry.data?.drives.find(item => item.barcode === barcode);
    if (drive) return drive.label;
    const slot = telemetry.data?.slots.find(item => item.barcode === barcode);
    return slot ? t(slot.kind === "import_export" ? "Emplacement d’échange {number}" : "Emplacement {number}", { number: slot.number }) : null;
  };
  return (
    <>
      {telemetry.data?.source === 'demo' && <section className="lto-source" aria-label={t("Démonstration locale")}>
          <span className="lto-demo-label">{t("Démonstration locale")}</span><Select aria-label={t("Scénario de démonstration")} value={telemetry.data.demo_scenario??"writing"} onValueChange={async (selectedValue) => {const state=selectedValue;try { const response=await fetch(`/__demo/scenario?state=${encodeURIComponent(state)}`,{method:'POST'}); if(!response.ok)throw Error(); } catch { toast.error(t('Scénario indisponible')); }}}><SelectOption value="s3-export">{t("Externalisation S3 simulée")}</SelectOption><SelectOption value="writing">{t("Écriture en cours")}</SelectOption><SelectOption value="reading">{t("Lecture en cours")}</SelectOption><SelectOption value="loaded">{t("Cartouche chargée")}</SelectOption><SelectOption value="idle">{t("Au repos")}</SelectOption><SelectOption value="empty">{t("Lecteur vide")}</SelectOption><SelectOption value="error">{t("Erreur de chargement")}</SelectOption><SelectOption value="unknown">{t("Positions inconnues")}</SelectOption><SelectOption value="stale">{t("Source périmée")}</SelectOption><SelectOption value="missing">{t("Source indisponible")}</SelectOption></Select>
          <MediaDemoActions barcodes={cartridges.map(media => media.barcode)} drive={telemetry.data.demo_drive ?? "CVT001L8"} failNext={!!telemetry.data.demo_fail_next}/>
      </section>}
      <div className="lto-library">
        <header className="lto-media-header">
          <div className="lto-toolbar" role="group" aria-label={t("Outils de la bibliothèque")}>
            <div className="lto-toolbar-group">
              {canManage && <CreateCartridge cartridges={cartridges} connected={connected} activeJobs={jobs.some(isActive)} onRefresh={onRefresh} />}
              {actions}
            </div>
            <div className="lto-toolbar-group lto-toolbar-browse">
            <Input
              aria-label={t("Rechercher une cartouche")}
              placeholder={t("Code-barres…")}
              className="lto-toolbar-search"
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setPage(0);
              }}
            />
            <Select
              className="lto-toolbar-filter"
              aria-label={t("Filtrer les cartouches par état")}
              value={filter}
              onValueChange={(selectedValue) => {
                setFilter(selectedValue);
                setPage(0);
              }}
            >
              <SelectOption value="all">{t("Tous les états")}</SelectOption>
              <SelectOption value="deleted">{t("Supprimées localement")}</SelectOption>
              {Object.entries(mediaLabels).map(([value, label]) => (
                <SelectOption key={value} value={value}>
                  {t(label)}
                </SelectOption>
              ))}
            </Select>
            <div role="group" aria-label={t("Présentation des cartouches")} className="flex gap-1">
              <Button size="sm" variant={view==='library'?'secondary':'ghost'} aria-pressed={view==='library'} onClick={()=>setView('library')} aria-label={t("Bibliothèque")} title={t("Bibliothèque")}><LibraryBig /><span className="lto-tool-label lto-view-label">{t("Bibliothèque")}</span></Button>
              <Button size="sm" variant={view==='table'?'secondary':'ghost'} aria-pressed={view==='table'} onClick={()=>setView('table')} aria-label={t("Tableau")} title={t("Tableau")}><Table2 /><span className="lto-tool-label lto-view-label">{t("Tableau")}</span></Button>
            </div>
            <Button size="sm" variant="ghost" onClick={()=>setDescending(!descending)} aria-label={t("Trier les cartouches")} title={t("Trier les cartouches par code-barres")}>{descending?<ArrowUpAZ/>:<ArrowDownAZ/>}</Button>
            {refreshAction}
            </div>
          </div>
          <div className="lto-selection-row" role="group" aria-label={t("Sélection des cartouches")}>
            {view === 'library' && <Checkbox aria-label={t("Sélectionner toutes les cartouches de cette page")} checked={visible.length > 0 && visibleSelected===visible.length ? true : visibleSelected>0 ? 'indeterminate' : false} onCheckedChange={v=>selectMedia(visible,v===true)} />}
            {selection.length === 0 && <p className="lto-selection-summary">{t(isPlural(cartridges.length)?"{count} cartouches · Aucune sélection":"{count} cartouche · Aucune sélection",{count:cartridges.length})}</p>}
        {selection.length > 0 && (
          <>
            <p role="status" className="text-sm font-medium" title={t("{count} cartouches sélectionnées", {count:selection.length})}>
              {t(isPlural(selection.length)?"{count} cartouches sélectionnées":"{count} cartouche sélectionnée",{count:selection.length})}
              {selection.length > filteredSelected && (
                <span className="ml-1 text-muted-foreground">
                  {t("(dont {count} hors filtre)", {count:selection.length-filteredSelected})}
                </span>
              )}
            </p>
            {filteredSelected < filtered.length && (
              <Button variant="outline" size="sm" aria-label={t("Sélectionner les {count} résultats", {count:filtered.length})} title={t("Sélectionner les {count} résultats", {count:filtered.length})} onClick={() => selectMedia(filtered, true)}>
                <ListChecks /><span>{t("Tous les résultats ({count})", {count:filtered.length})}</span>
              </Button>
            )}
          </>
        )}
        <div className="lto-selection-actions">
        {selection.length > 0 && (<>
            <Button variant="outline" size="sm" title={t("Envoyer vers S3 ({count})", {count:exportable.length})}
              disabled={!connected || !canManage || busy || !exportable.length}
              onClick={() => propose(exportable.map(media => media.barcode), selection.length - exportable.length, selection.filter(media => media.reconcile_pending && eligibleOnceSynchronised(media)).map(media => media.barcode))}>
              <CloudUpload data-icon="inline-start" />
              <span className="lto-tool-label lto-action-label">{t("Envoyer vers S3")} </span>({exportable.length})
            </Button>
          </>
        )}
        {canManage && <DeleteCartridges barcodes={selection.filter(media => !media.local_deleted).map(media => media.barcode)} connected={connected}
          onDeleted={async barcodes => { setSelectedBarcodes(previous => new Set([...previous].filter(barcode => !barcodes.includes(barcode)))); await onRefresh(); }} />}
          {selection.length > 0 && <Button variant="ghost" size="sm" aria-label={t("Tout désélectionner")} title={t("Tout désélectionner")} onClick={() => setSelectedBarcodes(new Set())}><X /></Button>}
        </div>
          </div>
        </header>
        <div className={view === 'table' || filter === 'deleted' ? "px-3 py-4 sm:px-5" : undefined}>
          {view === 'library' && filter !== 'deleted' ? <MediaLibrary media={visible} jobs={jobs} connected={connected} deleted={cartridges.filter(media => media.local_deleted).map(media => media.barcode)} retryable={connected && canManage && !busy ? cartridges.filter(media => resumableExport(media) && eligibleFor(media)).map(media => media.barcode) : []} onRetry={barcode => propose([barcode])} data={telemetry.data} history={telemetry.history} transferHistory={telemetry.transferHistory} controls={{selected:selectedBarcodes,disabled:!connected||busy,onSelect:(media,checked)=>selectMedia([media],checked),onDetails:setDetailBarcode}}/> : visible.length ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-10">
                    <Checkbox
                      aria-label={t("Sélectionner toutes les cartouches de cette page")}
                      checked={visibleSelected === visible.length ? true : visibleSelected > 0 ? "indeterminate" : false}
                      onCheckedChange={checked => selectMedia(visible, checked === true)}
                    />
                  </TableHead>
                  <TableHead
                    aria-sort={descending ? "descending" : "ascending"}
                  >
                    <Button
                      variant="ghost"
                      size="sm"
                      className="-ml-2"
                      onClick={() => setDescending(!descending)}
                    >
                      {t("Cartouche")}{descending ? (
                        <ArrowUpAZ data-icon="inline-end" />
                      ) : (
                        <ArrowDownAZ data-icon="inline-end" />
                      )}
                    </Button>
                  </TableHead>
                  <TableHead>{t("État")}</TableHead>
                  <TableHead>{t("Utilisation")}</TableHead>
                  <TableHead className="w-full">{t("Débit")}<span className="block text-xs font-normal text-muted-foreground">{t("Backup Client → VAULTCENTER · 2 min")}</span></TableHead>
                  <TableHead className="text-right">{t("Actions")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {visible.map((media) => {
                  const fill = occupancy(media,telemetry.data?.media.find(item=>item.barcode===media.barcode));
                  const working = !media.local_deleted && (
                    (telemetry.data?.status === 'available' && telemetry.data.drives.some(d => d.barcode === media.barcode && ['writing','reading','loading','unloading'].includes(d.state))) ||
                    (connected && jobs.some(job => job.subject === media.barcode && job.status === 'running'))
                  );
                  return (
                  <TableRow key={media.barcode} data-state={selectedBarcodes.has(media.barcode) ? "selected" : undefined}>
                    <TableCell>
                      <Checkbox
                        aria-label={t("Sélectionner {barcode}", {barcode:media.barcode})}
                        checked={selectedBarcodes.has(media.barcode)}
                        onCheckedChange={checked => selectMedia([media], checked === true)}
                      />
                    </TableCell>
                    <TableCell className="py-4">
                      <div className="flex items-center gap-3">
                        <span className="lto-media-icon" data-working={working} role="img" aria-label={working?t("Opération en cours sur {barcode}", {barcode:media.barcode}):t("Cartouche {barcode}", {barcode:media.barcode})} >
                          <HardDrive className="size-4" aria-hidden="true" />
                          {working && <LoaderCircle className="lto-media-work-ring" aria-hidden="true" />}
                        </span>
                        <span className="font-mono text-xs font-medium">
                          {media.barcode}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      {media.local_deleted ? <span className="text-sm text-muted-foreground">{t("Supprimée localement")}</span> : <StatusBadge state={media.state} />}
                      {!media.local_deleted && telemetry.data?.drives.filter(d=>d.barcode===media.barcode).map(d=><p key={d.id} className="mt-1 text-xs text-muted-foreground">{t(driveLabels[d.state])}</p>)}
                    </TableCell>
                    <TableCell>
                      {media.local_deleted ? <span className="text-sm text-muted-foreground">{t("Aucun fichier local")}</span> : <div className="flex w-36 flex-col gap-2">
                        <span className="text-xs text-muted-foreground">{t(fill.label)}<br/>
                          {formatBytes(fill.used)} /{" "}
                          {formatBytes(fill.capacity)}
                        </span>
                        <Progress
                          value={fill.percent}
                          aria-label={t("Utilisation {barcode}", {barcode:media.barcode})}
                        />
                      </div>}
                    </TableCell>
                    <TableCell>
                      <MediaRateSparkline barcode={media.barcode}
                        points={Object.values(telemetry.history).find(h=>h.barcode===media.barcode)?.points??[]}
                        rate={telemetry.data?.status==='available'?(telemetry.data.drives.find(d=>d.barcode===media.barcode)?.write_rate_bps??null):null}/>
                    </TableCell>
                    <TableCell>
                      <div
                        role="group"
                        aria-label={t("Actions de {barcode}", {barcode:media.barcode})}
                        className="flex justify-end [&>button]:relative [&>button:focus-visible]:z-10 [&>button:not(:first-child)]:-ml-px [&>button:not(:first-child)]:rounded-l-none [&>button:not(:last-child)]:rounded-r-none"
                      >
                        <Button
                          size="sm"
                          variant="outline"
                          aria-label={t("Détails de {barcode}", {barcode:media.barcode})}
                          disabled={!connected || busy}
                          onClick={() => setDetailBarcode(media.barcode)}
                        >
                          {t("Détails")}</Button>
                        <Button
                          size="sm"
                          variant="default"
                          disabled={
                            !connected ||
                            !canManage ||
                            busy ||
                            !eligibleFor(media) ||
                            activeFor(media.barcode)
                          }
                          aria-label={t(resumableExport(media)?"Reprendre l’envoi de {barcode}":"Envoyer {barcode} vers S3",{barcode:media.barcode})}
                          onClick={() => propose([media.barcode])}
                        >
                          <CloudUpload data-icon="inline-start" />
                          {resumableExport(media)?t("Reprendre"):t("Envoyer vers S3")}
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                );})}
              </TableBody>
            </Table>
          ) : (
            <Empty>
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <HardDrive />
                </EmptyMedia>
                <EmptyTitle>
                  {cartridges.length
                    ? t("Aucune cartouche correspondante")
                    : t("Aucune cartouche découverte")}
                </EmptyTitle>
                <EmptyDescription>
                  {cartridges.length
                    ? t("Essayez un autre code-barres ou un autre état.")
                    : t("Les médias apparaîtront après leur découverte par le service.")}
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          )}
        </div>
        <footer className="lto-chassis-footer flex-wrap gap-3">
          <span className="text-xs text-muted-foreground">
            {t(isPlural(filtered.length)?"{count} cartouches":"{count} cartouche",{count:filtered.length})}
            {filter !== "all" || query ? t(" sur {count}", {count:cartridges.length}) : ""}
          </span>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="icon-sm"
              aria-label={t("Page précédente des cartouches")}
              disabled={!currentPage}
              onClick={() => setPage(currentPage - 1)}
            >
              <ChevronLeft />
            </Button>
            <span className="text-xs">
              {t("Page {page} sur {total}",{page:currentPage+1,total:lastPage+1})}
            </span>
            <Button
              variant="outline"
              size="icon-sm"
              aria-label={t("Page suivante des cartouches")}
              disabled={currentPage === lastPage}
              onClick={() => setPage(currentPage + 1)}
            >
              <ChevronRight />
            </Button>
          </div>
        </footer>
      </div>
      {detailBarcode && (
        <CartridgeDialog
          key={detailBarcode}
          barcode={detailBarcode}
          media={detailMedia}
          location={locationOf(detailBarcode)}
          observation={telemetry.data?.media.find(item => item.barcode === detailBarcode)}
          drive={telemetry.data?.status === "available" ? telemetry.data.drives.find(item => item.barcode === detailBarcode) : undefined}
          jobs={jobs}
          exportDisabledReason={exportDisabledReason}
          onExport={() => {
            if (exportDisabledReason) return;
            propose([detailBarcode]);
            setDetailBarcode(null);
          }}
          onClose={() => setDetailBarcode(null)}
        />
      )}
      <Dialog
        open={!!action}
        onOpenChange={(open) => {
          if (!open && !busy) setAction(null);
        }}
      >
        <DialogContent showCloseButton={false} className="sm:max-w-md">
          <DialogHeader icon={HardDrive} className="pr-6">
            <DialogTitle>
              {t("Envoyer vers S3")}
            </DialogTitle>
            <DialogDescription>
              {action?.barcodes.length === 1
                ? t("{barcode} · Version {version}", {barcode:action.barcodes[0],version:cartridges.find(media=>media.barcode===action.barcodes[0])?.generation??'undefined'})
                : t("{count} cartouches", {count:action?.barcodes.length??0})}
            </DialogDescription>
          </DialogHeader>
          <DialogClose asChild>
            <Button
              className="absolute top-2 right-2"
              variant="ghost"
              size="icon-sm"
              disabled={busy}
              aria-label={t("Fermer la confirmation")}
            >
              <X />
            </Button>
          </DialogClose>
          {action && <ul aria-label={t("Cartouches concernées")} className="flex max-h-32 flex-wrap gap-1 overflow-y-auto">
            {action.barcodes.map(barcode => <li key={barcode} className="rounded-full border px-2 py-0.5 font-mono text-xs">{barcode}{accepted.includes(barcode) && <> ✓<span className="sr-only"> {t("Envoi lancé")}</span></>}</li>)}
          </ul>}
          <p className="text-sm">{t("Destination")} : <span className="break-all font-mono">{destination ?? t("Lecture…")}</span></p>
          {action && cartridges.some(media=>action.barcodes.includes(media.barcode)&&resumableExport(media)) && <p className="text-sm text-muted-foreground">{t("Les exports interrompus reprennent la même génération. Les parts déjà confirmées sont réutilisées après contrôle de la source et de la destination.")}</p>}
          {!!action?.syncing.length && <p className="text-sm text-muted-foreground">{t(action.syncing.length > 1 ? "{barcodes} se synchronisent avec le bucket. Leur envoi sera possible à la fin de la synchronisation." : "{barcodes} se synchronise avec le bucket. Son envoi sera possible à la fin de la synchronisation.", { barcodes: action.syncing.join(", ") })}</p>}
          {!!action?.excluded && <p className="text-sm text-muted-foreground">
            {t("La sélection exclut {count} cartouche(s), car leur état ne le permet pas ou une opération est déjà en cours.", {count:action.excluded})}
          </p>}
          {action && (
            <div className="flex items-start gap-3">
              <Checkbox
                id="pbs-released"
                checked={acknowledged}
                onCheckedChange={(checked) => setAcknowledged(checked === true)}
                disabled={busy}
              />
              <Label htmlFor="pbs-released" className="leading-relaxed">
                {client
                  ? t(isPlural(action.barcodes.length) ? "Ces cartouches sont terminées et déchargées dans {client}." : "Cette cartouche est terminée et déchargée dans {client}.", { client })
                  : t(isPlural(action.barcodes.length) ? "Ces cartouches sont terminées et déchargées dans le logiciel de sauvegarde." : "Cette cartouche est terminée et déchargée dans le logiciel de sauvegarde.")}
              </Label>
            </div>
          )}
          {!allowed && !busy && !actionError && (
            <Alert>
              <AlertTitle>{t("Action indisponible")}</AlertTitle>
              <AlertDescription>
                {t("L’état de la cartouche ou la connexion a changé. Actualisez l’inventaire avant de continuer.")}</AlertDescription>
            </Alert>
          )}
          {actionError && (
            <Alert variant="destructive">
              <AlertTitle>{t("Opération non confirmée")}</AlertTitle>
              <AlertDescription>
                {actionError.barcode} : {t(actionError.message)} {t("{count} opération(s) confirmée(s). Le traitement est arrêté ; les cartouches suivantes n’ont pas été envoyées. Consultez l’activité avant de refaire une sélection.",{count:accepted.length})}
              </AlertDescription>
            </Alert>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setAction(null)}
            >
              {actionError ? t("Fermer") : t("Annuler")}
            </Button>
            <Button
              disabled={
                busy || !allowed || !acknowledged
              }
              onClick={() => void submit()}
            >
              {busy && (
                <LoaderCircle
                  data-icon="inline-start"
                  className="animate-spin"
                />
              )}
              {busy
                ? t("En cours… {done}/{total}", {done:accepted.length,total:action?.barcodes.length??'undefined'})
                : t("Envoyer vers S3")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
