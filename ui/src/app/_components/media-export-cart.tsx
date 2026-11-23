"use client";
import {useI18n} from '@/lib/i18n';
import {type Cartridge, type Job} from '@/lib/api';
import {type MediaObservation,type SlotObservation} from '@/lib/telemetry';
import {MediaCartridge,type CartridgeControls} from './media-cartridge';

export function MediaExportCart({slots,media,exports,observations,controls,positionsAvailable,loaded}:{
 slots:SlotObservation[];media:Cartridge[];observations:Map<string,MediaObservation>;
 controls:CartridgeControls;positionsAvailable:boolean;loaded:Set<string>;
 exports:Map<string,Job>;
}) {
 const {t}=useI18n();
 const known=slots.length>0&&slots.every(s=>s.position_known);
 const occupied=slots.filter(s=>s.position_known&&s.barcode&&!loaded.has(s.barcode)).length;
 return <section className="lto-export-cart" aria-label={t('Chariot d’import / export')}>
  <div className="lto-magazine-heading"><h3>{t('Import / export')}</h3><div className="lto-magazine-grip" aria-hidden="true"/></div>
  <div className="lto-export-cart-frame">
   <div className="lto-export-cart-status">{known?t('{occupied} / {total} occupés',{occupied,total:slots.length}):t('Positions non connues')}</div>
   {slots.length?<div className="lto-magazine lto-magazine-spines" tabIndex={0} role="region" aria-label={t('Emplacements d’import / export')}>
    {slots.map(slot=>{
     const barcode=slot.position_known&&slot.barcode&&!loaded.has(slot.barcode)?slot.barcode:null;
     const cartridge=barcode?media.find(m=>m.barcode===barcode):undefined;
     const label=t('Emplacement {slot}',{slot:slot.number});
     return <div key={slot.number} className="lto-magazine-slot">
      <span className="lto-slot-number" aria-label={label}>{String(slot.number).padStart(2,'0')}</span>
      <div className={`lto-bay${cartridge?'':' lto-empty-bay'}`}>
       {cartridge?<MediaCartridge spine media={cartridge} exportJob={exports.get(cartridge.barcode)} observation={observations.get(barcode!)} controls={controls}/>:<span className="lto-slot-state">{barcode??t(!slot.position_known?'Position non connue':slot.barcode&&loaded.has(slot.barcode)?'Cartouche dans le lecteur':'Vide')}</span>}
      </div>
     </div>;
    })}
   </div>:<p className="lto-export-cart-empty">{t(positionsAvailable?'Aucun emplacement d’import / export signalé.':'Les emplacements d’import / export ne sont pas disponibles.')}</p>}
  </div>
  <div className="lto-export-cart-base" aria-hidden="true"/>
 </section>;
}
