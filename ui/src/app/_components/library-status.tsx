"use client";
import type {ReactNode} from 'react';
import {useI18n} from '@/lib/i18n';
import {Activity,Wifi,WifiOff} from 'lucide-react';
import {formatBytes} from '@/lib/api';
import {driveLabels,type Telemetry} from '@/lib/telemetry';

export function LibraryStatus({data,connected,selected,gauge}:{data:Telemetry|null;connected:boolean;selected:boolean;gauge?:ReactNode}) {
 const {t}=useI18n();
 return <footer className="lto-console-status" role="region" aria-label={t("État des lecteurs")}>
  <div className="lto-console-drive-status">
   {data?.drives.length?data.drives.map(d=><span className="lto-console-drive" key={d.id} data-active={data.status==='available'&&['writing','reading'].includes(d.state)}>
    <Activity aria-hidden="true"/>
    <span>{t(d.label)}</span><strong>{d.barcode??(data.status==='available'&&['empty','idle'].includes(d.state)?t('Sans cartouche'):t('Cartouche non identifiée'))}</strong><span>{t(driveLabels[d.state])}</span>
    {data.status==='available'&&d.write_rate_bps!==null&&<span className="tabular-nums">{formatBytes(d.write_rate_bps)}/s</span>}
   </span>):<span>{selected?t('Suivi des lecteurs indisponible'):t('Aucune bibliothèque sélectionnée')}</span>}
  </div>
  {gauge}
  <span className="lto-console-connection" aria-label={connected?t('Service connecté'):t('Service indisponible')} title={connected?t('Service connecté'):t('Service indisponible')}>{connected?<Wifi aria-hidden="true"/>:<WifiOff aria-hidden="true"/>}<span className="lto-console-connection-label">{connected?t('Service connecté'):t('Service indisponible')}</span></span>
 </footer>;
}
