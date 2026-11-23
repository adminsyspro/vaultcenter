"use client";
import {useI18n} from '@/lib/i18n';
import { Activity, HardDrive } from 'lucide-react';
import { type Cartridge, formatBytes } from '@/lib/api';
import { driveLabels, type DriveObservation, type MediaObservation, type RatePoint } from '@/lib/telemetry';
import { MediaThroughput } from './media-throughput';
import { MediaCartridge, type CartridgeControls } from './media-cartridge';

export function MediaDrive({ drive, media, observation, controls, stale, points }: {
  drive: DriveObservation;
  media?: Cartridge;
  observation?: MediaObservation;
  controls: CartridgeControls;
  stale: boolean;
  points: RatePoint[];
}) {
 const {t}=useI18n();
  const empty = !stale && !drive.barcode && (drive.state === 'empty' || drive.state === 'idle');
  const active = !stale && (drive.state === 'writing' || drive.state === 'reading');
  return (
    <section className="lto-drive" aria-label={t(drive.label)} data-active={active}>
      <div className="lto-drive-media">
        {media ? <MediaCartridge media={media} observation={observation} controls={controls} running={active} /> :
          <div className="lto-drive-empty"><HardDrive aria-hidden="true" className="size-8" /><span>{drive.barcode ? t("{barcode} · hors sélection", {barcode:drive.barcode}) : empty ? t('Lecteur vide') : t('Cartouche non identifiée')}</span><div className="lto-drive-mouth" /></div>}
      </div>
      <div className="lto-drive-body">
        <header className="lto-drive-heading">
          <h3><Activity aria-hidden="true" /><span>{empty ? t('Lecteur vide') : t(driveLabels[drive.state])}</span><span className="lto-led" aria-hidden="true" /></h3>
          <p>{t(drive.label)}{drive.origin_slot !== null && <span>{t(" · Emplacement {slot}", {slot:drive.origin_slot})}</span>}</p>
        </header>
        <dl className="lto-drive-metrics">
          <div><dt title={t("Taille des fichiers sur disque, distincte de l’occupation logique de la bande.")}>{t(stale?"Volume local · dernier relevé":"Volume local")}</dt><dd>{observation?.local_size_bytes != null ? formatBytes(observation.local_size_bytes) : t('Indisponible')}</dd></div>
          <div><dt>{t("Croissance du volume local")}</dt><dd>{observation?.local_growth_bps != null ? `${formatBytes(observation.local_growth_bps)}/s` : t('Indisponible')}</dd></div>
          <div><dt>{t("Débit d’écriture")}</dt><dd>{drive.write_rate_bps != null ? `${formatBytes(drive.write_rate_bps)}/s` : t('Débit indisponible')}</dd></div>
        </dl>
        <MediaThroughput driveId={drive.id} barcode={drive.barcode??''} points={points} writeRate={drive.write_rate_bps} localGrowth={observation?.local_growth_bps ?? null} available={!stale} />
      </div>
    </section>
  );
}
