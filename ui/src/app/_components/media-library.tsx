"use client";
import {useI18n} from '@/lib/i18n';
import {type Cartridge, type Job, isActive} from '@/lib/api';
import {type RateHistory,type Telemetry} from '@/lib/telemetry';
import {MediaCartridge,type CartridgeControls} from './media-cartridge';
import {MediaS3Transfer} from './media-s3-transfer';
import {MediaThroughput} from './media-throughput';
import {MediaDrive} from './media-drive';
import {MediaLcd} from './media-lcd';
import {MediaExportCart} from './media-export-cart';
import {MediaS3Flows} from './media-s3-flows';
export function MediaLibrary({media,jobs,data,controls,history,transferHistory,connected,deleted,retryable,onRetry}:{media:Cartridge[];jobs:Job[];connected:boolean;deleted:string[];retryable?:string[];onRetry?:(barcode:string)=>void;data:Telemetry|null;history:RateHistory;transferHistory:{id:string;points:import('@/lib/telemetry').RatePoint[]}|null;controls:CartridgeControls}) {
 const {t}=useI18n();
 const observations=new Map(data?.media.map(m=>[m.barcode,m])??[]);
 const exports=new Map(jobs.filter(job=>job.kind==='export'&&isActive(job)).map(job=>[job.subject,job]));
 const loaded=new Set(data?.drives.flatMap(d=>d.barcode?[d.barcode]:[])??[]);
 const knownSlots=data?.slots.filter(s=>s.position_known)??[];
 const storageSlots=knownSlots.filter(s=>s.kind==='storage').toSorted((a,b)=>a.number-b.number);
 const exportSlots=(data?.slots.filter(s=>s.kind==='import_export')??[]).toSorted((a,b)=>a.number-b.number);
 const exported=new Set(exportSlots.filter(s=>s.position_known).map(s=>s.barcode));
 const transfer=data?.source==='demo'?data.demo_transfer:undefined;
 const source=transfer?media.find(m=>m.barcode===transfer.barcode):undefined;
 const stored=media.filter(m=>!loaded.has(m.barcode)&&!exported.has(m.barcode)&&m!==source);
 const positioned=new Set(storageSlots.map(s=>s.barcode));
 const bays=[...storageSlots.map(slot=>({slot,cartridge:stored.find(m=>m.barcode===slot.barcode)})),...stored.filter(m=>!positioned.has(m.barcode)).map(cartridge=>({slot:undefined,cartridge}))];
 return <div className="lto-chassis-interior">
  <div className="lto-reader-section">{transfer&&source?<MediaS3Transfer transfer={transfer} media={source} observation={observations.get(source.barcode)} controls={controls} points={transferHistory?.id===transfer.id?transferHistory.points:[]}/>:data?.drives.length?data.drives.map(d=><MediaDrive key={d.id} drive={d} media={media.find(m=>m.barcode===d.barcode)} observation={d.barcode?observations.get(d.barcode):undefined} controls={controls} points={history[d.id]?.barcode===d.barcode?history[d.id].points:[]} stale={data.status!=='available'}/>):<section className="lto-history-offline"><p className="lto-source-empty">{t("L’activité des lecteurs n’est pas disponible. L’historique enregistré reste consultable.")}</p><MediaThroughput barcode="" points={[]} writeRate={null} localGrowth={null} available={false}/></section>}</div>
  <div className="lto-library-lower">
  <MediaLcd data={data} jobs={jobs} connected={connected}/>
  <section className="lto-library-inventory" aria-label={t("Inventaire des cartouches")}>
  <div className="lto-magazine-heading"><h3>{storageSlots.length?t('Magasin de cartouches'):t('Inventaire des cartouches')}</h3>{!storageSlots.length&&<span>{t('Positions non connues')}</span>}<div className="lto-magazine-grip" aria-hidden="true"/></div>
  <div className="lto-magazine lto-magazine-spines" tabIndex={0} role="region" aria-label={t("Emplacements des cartouches")}>
   {!media.length && <p className="lto-source-empty">{t("Aucune cartouche à afficher.")}</p>}
   {bays.map(({slot,cartridge})=>{
    const number=slot?String(slot.number).padStart(2,'0'):'—';
    const label=slot?t('Emplacement {slot}',{slot:number}):t('Position non connue');
    const inDrive=slot&&data?.drives.some(d=>d.origin_slot===slot.number||!!slot.barcode&&d.barcode===slot.barcode);
    const state=inDrive?t('Cartouche dans le lecteur'):slot?.barcode??t('Vide');
    return <div className="lto-magazine-slot" key={slot?`slot-${slot.number}`:cartridge!.barcode} role="group" aria-label={label}>
     <span className="lto-slot-number" title={label}>{number}</span>
     <div className={`lto-bay${cartridge?'':' lto-empty-bay'}`}>
      {cartridge?<MediaCartridge spine media={cartridge} exportJob={exports.get(cartridge.barcode)} observation={observations.get(cartridge.barcode)} controls={controls}/>:<span className={slot?.barcode&&!inDrive?'lto-slot-state':'sr-only'}>{state}</span>}
     </div>
    </div>;
   })}
  </div>
  </section>
  <MediaExportCart slots={exportSlots} media={media.filter(m=>m!==source)} exports={exports} observations={observations} controls={controls} positionsAvailable={knownSlots.length>0} loaded={loaded}/>
  </div>
  <MediaS3Flows jobs={jobs} connected={connected} deleted={deleted} retryable={retryable} onRetry={onRetry}/>
 </div>;
}
