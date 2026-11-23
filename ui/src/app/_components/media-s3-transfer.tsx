"use client";
import {useI18n} from '@/lib/i18n';
import {useState} from 'react';
import {CloudUpload,Database,ArrowRight,RotateCcw} from 'lucide-react';
import {localizedToast as toast} from './providers';
import {formatBytes,type Cartridge} from '@/lib/api';
import type {DemoTransfer,MediaObservation,RatePoint} from '@/lib/telemetry';
import {MediaCartridge,type CartridgeControls} from './media-cartridge';
import {MediaThroughput} from './media-throughput';

export function MediaS3Transfer({transfer,media,observation,controls,points}:{transfer:DemoTransfer;media:Cartridge;observation?:MediaObservation;controls:CartridgeControls;points:RatePoint[]}) {
 const {t}=useI18n();
 const [restarting,setRestarting]=useState(false);
 const active=!transfer.stale&&transfer.status==='running';
 const percent=transfer.total_bytes>0?Math.min(100,transfer.transferred_bytes/transfer.total_bytes*100):0;
 async function restart() {
  setRestarting(true);
  try {const response=await fetch('/__demo/scenario?state=s3-export',{method:'POST'});if(!response.ok)throw Error();}
  catch {toast.error(t('Impossible de relancer la simulation'));}
  finally {setRestarting(false);}
 }
 return <section className="lto-drive s3-transfer" aria-label={t("Externalisation S3 simulée")} data-active={active}>
  <div className="lto-drive-media"><MediaCartridge media={media} observation={observation} controls={controls}/></div>
  <div className="lto-drive-body s3-transfer-body">
   <header className="s3-transfer-heading"><h3><CloudUpload size={18}/>{transfer.stale?t('Mesures interrompues'):active?t('Externalisation vers S3'):t('Externalisation terminée')}</h3><span>{t("Simulation locale")}</span><button type="button" onClick={restart} disabled={restarting} aria-label={t("Relancer la simulation")} title={t("Relancer la simulation")}><RotateCcw size={16}/></button></header>
   <div className="s3-transfer-flow" aria-label={t("{barcode} vers le bucket {bucket}", {barcode:media.barcode,bucket:transfer.bucket})}>
    <span className="s3-transfer-endpoint"><Database size={22}/><span>{media.barcode}<small>{t("Volume local")}</small></span></span>
    <div className="s3-transfer-stream" aria-hidden="true"><div className="s3-stream-packets">{[0,1].map(copy=><span key={copy}>{'01001010 11010001 00101101 10110010 01010110 10010101 '.repeat(8)}</span>)}</div></div>
    <ArrowRight className="s3-transfer-arrow" size={18} aria-hidden="true"/>
    <span className="s3-transfer-endpoint"><CloudUpload size={26}/><span>{transfer.bucket}<small>{t("Stockage S3")}</small></span></span>
   </div>
   <div className="s3-transfer-progress"><div><span>{t("{sent} / {total} envoyés", {sent:formatBytes(transfer.transferred_bytes),total:formatBytes(transfer.total_bytes)})}</span><strong>{Math.floor(percent)} %</strong></div><progress aria-label={t("Progression de l’externalisation simulée")} value={transfer.transferred_bytes} max={transfer.total_bytes}/></div>
   <MediaThroughput transferId={transfer.id} barcode={media.barcode} points={points} writeRate={transfer.rate_bps} localGrowth={null} available={!transfer.stale} metricTitle={t("Débit vers S3")}/>
  </div>
 </section>;
}
