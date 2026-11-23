"use client";
import { translate as t, isPlural, getLocale, useI18n } from "@/lib/i18n";
import {Cloud,PackageOpen} from 'lucide-react';
import {useContext} from 'react';
import {VtlScope} from './vtl-scope';
import {Button} from '@/components/ui/button';
import {formatBytes} from '@/lib/api';
import {LtoBarcodeLabel} from './lto-barcode-label';
import {LtoReel} from './lto-reel';

export type BucketInventory = {
 complete:boolean;objects:number;bytes:number;outside_prefix:number;unrecognized_objects:number;
 cartridges:{barcode:string;objects:number;bytes:number}[];
 manifests:{barcode:string;generation:number;key:string}[];
};
export function StorageCartridges({inventory,demo}:{inventory:BucketInventory|null;demo?:boolean}) {
  useI18n();
  const vtlId=useContext(VtlScope);
 return <section className="storage-bucket-content" aria-label={t("Cartouches repérées dans le bucket")}>
  <header className="storage-inventory-heading"><h3>{t("Cartouches repérées dans le bucket")}</h3>{demo&&<span className="lto-demo-label">{t("Démonstration locale")}</span>}</header>
  {inventory?<>
   <div className="storage-inventory-summary"><p>{t(isPlural(inventory.cartridges.length) ? "{count} cartouches" : "{count} cartouche", { count: inventory.cartridges.length.toLocaleString(getLocale()) })} · {t(isPlural(inventory.objects) ? "{count} objets actuels" : "{count} objet actuel", { count: inventory.objects.toLocaleString(getLocale()) })} · {formatBytes(inventory.bytes)}</p><span>{inventory.complete?t("Liste complète des objets actuels"):t("Liste partielle")}</span></div>
   {inventory.cartridges.length?<div className="storage-lto-shelf">{inventory.cartridges.map(media=>{
    const generations=[...new Set(inventory.manifests.filter(m=>m.barcode===media.barcode).map(m=>m.generation))].sort((a,b)=>a-b);
    return <article className="lto-cartridge storage-lto" key={media.barcode} aria-label={t("Copie S3 de {barcode}", { barcode: media.barcode })}>
     <div className="lto-cartridge-top"><Cloud size={16} aria-hidden="true"/><span>LTO</span><span className="lto-ribs" aria-hidden="true"/></div>
     <LtoReel/>
     <div className="lto-label"><LtoBarcodeLabel barcode={media.barcode}/><span className="lto-label-status">{t("Copie S3")}</span></div>
     <div className="storage-lto-volume"><strong>{formatBytes(media.bytes)}</strong><span>{t(isPlural(media.objects) ? "{count} objets" : "{count} objet", { count: media.objects.toLocaleString(getLocale()) })}</span><p>{t("Volume stocké dans ce bucket")}</p></div>
     <details className="storage-lto-generations"><summary>{generations.length?t(isPlural(generations.length) ? "{count} générations repérées" : "{count} génération repérée", { count: generations.length.toLocaleString(getLocale()) }):t("Aucun manifeste repéré")}</summary><p>{t("Versions repérées :")} {generations.join(', ')||t("aucun manifeste")}</p></details>
    </article>;
   })}</div>:<div className="storage-inventory-empty"><PackageOpen size={30} aria-hidden="true"/>{inventory.complete&&inventory.objects===0?<div className="space-y-3"><p>{t("Aucune copie S3 dans ce bucket.")}</p><Button variant="outline" asChild><a href={vtlId?`#media?vtl=${encodeURIComponent(vtlId)}`:'#media'}>{t("Choisir des cartouches à envoyer")}</a></Button></div>:<p>{t("Aucune cartouche VTL reconnue dans cette liste.")}</p>}</div>}
   <p className="storage-inventory-note">{t(isPlural(inventory.outside_prefix) ? "{count} objets hors du préfixe VTL" : "{count} objet hors du préfixe VTL", { count: inventory.outside_prefix.toLocaleString(getLocale()) })} · {t(isPlural(inventory.unrecognized_objects) ? "{count} objets non reconnus dans ce préfixe." : "{count} objet non reconnu dans ce préfixe.", { count: inventory.unrecognized_objects.toLocaleString(getLocale()) })}</p>
  </>:<div className="storage-inventory-empty"><PackageOpen size={30} aria-hidden="true"/><p>{t("Inventaire non disponible. Les cartouches concernées ne peuvent pas être confirmées.")}</p></div>}
 </section>;
}
