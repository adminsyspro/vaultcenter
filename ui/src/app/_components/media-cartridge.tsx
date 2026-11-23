"use client";
import {useI18n} from '@/lib/i18n';
import {LtoBarcodeLabel} from './lto-barcode-label';
import {LtoReel} from './lto-reel';
import {MediaCopyBadge} from './media-copy-badge';
import { Checkbox } from '@/components/ui/checkbox';
import {exportProgress,exportPhases} from '@/lib/export-progress';
import { CloudUpload } from 'lucide-react';
import { type Cartridge, type Job, formatBytes, isActive, mediaLabels } from '@/lib/api';
import { occupancy, type MediaObservation } from '@/lib/telemetry';

export type CartridgeControls = {
  selected: Set<string>;
  disabled: boolean;
  onSelect: (media: Cartridge, checked: boolean) => void;
  onDetails: (barcode: string) => void;
};

export function MediaCartridge({ media, observation, controls, exportJob, running = false, spine = false }: {
  media: Cartridge;
  observation?: MediaObservation;
  controls: CartridgeControls;
  exportJob?: Job;
  running?: boolean;
  spine?: boolean;
}) {
 const {t,locale}=useI18n();
  const fill = occupancy(media, observation);
  const percentage = fill.percent === null ? '—' : `${Number(fill.percent.toFixed(1)).toLocaleString(locale)} %`;
  const activeExport = exportJob?.subject === media.barcode && exportJob.kind === 'export' && isActive(exportJob) ? exportJob : undefined;
  const sending = !!activeExport || media.state === 'uploading';
  const transfer = activeExport ? exportProgress(activeExport) : null;
  const progress = transfer?.percent ?? undefined;
  const sendLabel = t("Envoi de {barcode} vers S3", {barcode:media.barcode});
  if(spine)return <article className="lto-cartridge-spine" data-selected={controls.selected.has(media.barcode)} aria-label={t("Cartouche {barcode}", {barcode:media.barcode})} title={`${media.barcode} · ${t(mediaLabels[media.state]??media.state)} · ${formatBytes(fill.used)} / ${formatBytes(fill.capacity)}`}>
    <Checkbox aria-label={t("Sélectionner {barcode}", {barcode:media.barcode})} checked={controls.selected.has(media.barcode)} onCheckedChange={value=>controls.onSelect(media,value===true)}/>
    <button type="button" className="lto-label" aria-label={t("Détails de {barcode}", {barcode:media.barcode})} disabled={controls.disabled} onClick={()=>controls.onDetails(media.barcode)}><LtoBarcodeLabel barcode={media.barcode}/></button>
    <MediaCopyBadge media={media} observation={observation}/>
    {sending ? <div className="lto-s3-cloud" role="progressbar" aria-label={sendLabel} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress} title={transfer ? `${sendLabel} · ${t(exportPhases[transfer.phase])}` : sendLabel}>
      <CloudUpload aria-hidden="true" />
      <span aria-hidden="true">{progress === undefined ? 'S3' : `${Math.floor(progress)} %`}</span>
    </div> : <div className="lto-occupancy-donut" role={fill.percent === null ? 'img' : 'meter'} aria-label={t("Occupation {barcode} · {label}{unknown}", {barcode:media.barcode,label:t(fill.label),unknown:fill.percent===null?t(" · inconnue"):""})} aria-valuemin={fill.percent === null ? undefined : 0} aria-valuemax={fill.percent === null ? undefined : 100} aria-valuenow={fill.percent ?? undefined} aria-valuetext={fill.percent === null ? undefined : percentage} title={`${t(fill.label)} · ${percentage}`}>
      <svg viewBox="0 0 40 40" aria-hidden="true"><circle className="lto-donut-track" cx="20" cy="20" r="17"/>{fill.percent !== null && <circle className="lto-donut-fill" cx="20" cy="20" r="17" pathLength="100" strokeDasharray={`${fill.percent} 100`} transform="rotate(-90 20 20)"/>}</svg>
      <span aria-hidden="true">{percentage}</span>
    </div>}
  </article>;
  return (
    <article className="lto-cartridge" data-running={running} data-selected={controls.selected.has(media.barcode)} aria-label={t("Cartouche {barcode}", {barcode:media.barcode})}>
      <div className="lto-cartridge-top">
        <Checkbox aria-label={t("Sélectionner {barcode}", {barcode:media.barcode})} checked={controls.selected.has(media.barcode)} onCheckedChange={v => controls.onSelect(media, v === true)} />
        <span>{media.media_type?.replace('LTO','LTO-')??'LTO'}</span><span className="lto-ribs" aria-hidden="true" />
        <MediaCopyBadge media={media} observation={observation}/>
      </div>
      <LtoReel/>
      <button type="button" className="lto-label" aria-label={t("Détails de {barcode}", {barcode:media.barcode})} disabled={controls.disabled} onClick={() => controls.onDetails(media.barcode)}>
        <LtoBarcodeLabel barcode={media.barcode}/>
        <span className="lto-label-status">{t(mediaLabels[media.state] ?? media.state)}</span>
      </button>
      <div className="lto-usage">
        <div className="flex justify-between gap-2 tabular-nums"><span>{formatBytes(fill.used)} / {formatBytes(fill.capacity)}</span><span>{percentage}</span></div>
        {fill.percent !== null ? <div className="lto-gauge" role="meter" aria-label={t("Occupation {barcode} · {label}", {barcode:media.barcode,label:t(fill.label)})} aria-valuemin={0} aria-valuemax={100} aria-valuenow={fill.percent}><span style={{ width: `${fill.percent}%` }} /></div> : <p>{t("Occupation inconnue")}</p>}
        <p className="lto-usage-provenance" title={media.segment_size_mib?t("Segments S3 : {size} · Parts multipart : {parts} Mio", {size:formatBytes(media.segment_size_mib*1024**2),parts:media.multipart_part_size_mib??256}):undefined}>{t(fill.label)}</p>
      </div>
    </article>
  );
}
