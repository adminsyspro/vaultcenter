"use client";
import { translate as t, isPlural, useI18n } from "@/lib/i18n";

import {useContext,useEffect,useRef,useState} from 'react';
import {LoaderCircle,Plus,Videotape} from 'lucide-react';
import {localizedToast as toast} from './providers';
import {Button} from '@/components/ui/button';
import {Input} from '@/components/ui/input';
import {Label} from '@/components/ui/label';
import {Select,SelectOption} from '@/components/ui/select';
import {Alert,AlertTitle,AlertDescription} from '@/components/ui/alert';
import {Dialog,DialogContent,DialogHeader,DialogTitle,DialogDescription,DialogFooter} from '@/components/ui/dialog';
import {formatBytes,type Cartridge} from '@/lib/api';
import {VtlScope,useScopedRequest} from './vtl-scope';
import {vtlHash} from './use-vtl-selection';

// A logarithmic track keeps both small media and multi-TiB capacities usable.
// Manual values remain exact; dragging and arrow keys select powers of two.
function SizeSlider({label,description,value,min,max,disabled,onChange}:{label:string;description:string;value:number;min:number;max:number;disabled:boolean;onChange:(value:number)=>void}) {
  useI18n();
 const bounded=Number.isFinite(value)?Math.max(min,Math.min(max,value)):min;
 const lower=Math.log2(min),upper=Math.log2(max),position=Math.log2(bounded);
 const change=(next:number)=>onChange(Math.max(min,Math.min(max,next)));
 return <input type="range" aria-label={label} aria-describedby={description}
  aria-valuetext={formatBytes(bounded*1024**2)} min={lower} max={upper} step="any" value={position}
  disabled={disabled||min===max}
  className="h-8 w-full min-w-0 cursor-pointer accent-primary focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-50"
  onChange={event=>change(2**Math.round(Number(event.target.value)))}
  onKeyDown={event=>{
   let next:number;
   switch(event.key){
    case 'ArrowRight':case 'ArrowUp':next=2**(Math.floor(position)+1);break;
    case 'ArrowLeft':case 'ArrowDown':next=2**(Math.ceil(position)-1);break;
    case 'Home':next=min;break;
    case 'End':next=max;break;
    default:return;
   }
   event.preventDefault();change(next);
  }}/>;
}

type Creation={barcode:string;media_type:string;capacity_bytes:number;segment_size_mib:number;multipart_part_size_mib:number};
type BatchRequest={request_id:string;count:number;media_type:string;capacity_bytes:number;segment_size_mib:number;multipart_part_size_mib:number};
type Batch={request:BatchRequest;barcodes:string[];completed:number;phase:string};
type Budget={free_bytes:number;reserved_bytes:number;available_bytes:number;overhead_per_cartridge:number};
type Options={online_creation?:boolean;pending_batch?:Batch|null;storage?:Budget|null;default_multipart_part_size_mib:number;min_multipart_part_size_mib:number;max_multipart_part_size_mib:number;pending_creation?:Creation|null;configured:boolean;ready:boolean;reason:string|null;media_types:string[];default_media_type:string|null;default_capacity_mib:number;default_segment_size_mib:number;min_capacity_mib:number;max_capacity_mib:number;min_segment_size_mib:number;max_segment_size_mib:number;free_slots:number;demo?:boolean};
export function CreateCartridge({cartridges,connected,activeJobs,onRefresh}:{cartridges:Cartridge[];connected:boolean;activeJobs:boolean;onRefresh:()=>Promise<void>}) {
  useI18n();
 const [open,setOpen]=useState(false);const id=useContext(VtlScope);
 useEffect(()=>setOpen(false),[id]);
 return <><Button size="sm" className="lto-toolbar-icon" title={t("Créer des cartouches")} disabled={!connected} onClick={()=>setOpen(true)}><Plus/><span className="lto-tool-label">{t("Créer des cartouches")}</span></Button><Dialog open={open} onOpenChange={setOpen}>{open&&<CreateForm key={id} cartridges={cartridges} connected={connected} activeJobs={activeJobs} onRefresh={onRefresh} onClose={()=>setOpen(false)}/>}</Dialog></>;
}
function CreateForm({connected,activeJobs,onRefresh,onClose}:{cartridges:Cartridge[];connected:boolean;activeJobs:boolean;onRefresh:()=>Promise<void>;onClose:()=>void}) {
  useI18n();
 const request=useScopedRequest();const id=useContext(VtlScope);
 const [options,setOptions]=useState<Options|null>(null);const [loadError,setLoadError]=useState('');
 const [count,setCount]=useState('1');const [batch,setBatch]=useState<Batch|null>(null);
 const attempted=useRef<BatchRequest|null>(null);
 const requestId=useRef(Array.from(crypto.getRandomValues(new Uint8Array(16)),v=>v.toString(16).padStart(2,'0')).join(''));
 const [revision,setRevision]=useState(0);const [barcode,setBarcode]=useState('');const [mediaType,setMediaType]=useState('');
 const [capacity,setCapacity]=useState('2');const [unit,setUnit]=useState('1024');const [segments,setSegments]=useState('1');const [segmentUnit,setSegmentUnit]=useState('1048576');const [partSize,setPartSize]=useState('256');
 function setSegmentMib(value:number){const divisor=value>=1048576&&value%1048576===0?1048576:value>=1024&&value%1024===0?1024:1;setSegmentUnit(String(divisor));setSegments(String(value/divisor));}
 const [busy,setBusy]=useState(false);const [error,setError]=useState('');const submitting=useRef(false);const alive=useRef(true);
 useEffect(()=>{alive.current=true;return()=>{alive.current=false;};},[]);
 useEffect(()=>{
  const abort=new AbortController();setOptions(null);setLoadError('');
  request<Options>('/v1/cartridges/options',{signal:abort.signal}).then(value=>{
   if(abort.signal.aborted)return;
   setOptions(value);const initialType=value.default_media_type??value.media_types[0]??'';setMediaType(initialType);
   if(value.pending_batch){const saved=value.pending_batch;setBatch(saved);setCount(String(saved.request.count));attempted.current=saved.request;requestId.current=saved.request.request_id;setMediaType(saved.request.media_type);}
   setCapacity(String(value.default_capacity_mib/1024));setUnit('1024');setSegmentMib(value.default_segment_size_mib);setPartSize(String(value.default_multipart_part_size_mib??256));
   if(value.pending_batch){const pending=value.pending_batch.request;setCapacity(String(pending.capacity_bytes/1024**3));setSegmentMib(pending.segment_size_mib);setPartSize(String(pending.multipart_part_size_mib));}
   else if(value.pending_creation){const pending=value.pending_creation;setBarcode(pending.barcode);setMediaType(pending.media_type);setCapacity(String(pending.capacity_bytes/1024**3));setSegmentMib(pending.segment_size_mib);setPartSize(String(pending.multipart_part_size_mib??256));}
  }).catch(e=>{if(!abort.signal.aborted)setLoadError(e.message);});
  return()=>abort.abort();
 },[request,revision]);
 const capacityMib=Number(capacity)*Number(unit);const segmentMib=Number(segments)*Number(segmentUnit);const partMib=Number(partSize);
 const payload={barcode,media_type:mediaType,capacity_bytes:capacityMib*1024**2,segment_size_mib:segmentMib,multipart_part_size_mib:partMib};
 const recovering=!!options?.pending_creation||!!batch||!!attempted.current;
 const countNumber=Number(count);
 const countValid=Number.isInteger(countNumber)&&countNumber>=1&&countNumber<=Math.min(64,options?.free_slots??0);
 const requestedBytes=countNumber*(capacityMib*1024**2+(options?.storage?.overhead_per_cartridge??0));
 const spaceValid=!!options?.storage&&requestedBytes<=options.storage.available_bytes;
 const capacityValid=!!capacity.trim()&&Number.isSafeInteger(capacityMib)&&!!options&&capacityMib>=options.min_capacity_mib&&capacityMib<=options.max_capacity_mib;
 const segmentsValid=!!segments.trim()&&Number.isInteger(segmentMib)&&!!options&&segmentMib>=options.min_segment_size_mib&&segmentMib<=options.max_segment_size_mib;
 const partCount=partMib>0?Math.ceil(segmentMib/partMib):Infinity;
 const partsValid=!!partSize.trim()&&Number.isInteger(partMib)&&!!options&&partMib>=(options.min_multipart_part_size_mib??16)&&partMib<=(options.max_multipart_part_size_mib??1024)&&partCount<=10000;
 const available=connected&&!activeJobs&&options?.configured&&options.ready;
 const valid=available&&(recovering||(countValid&&spaceValid))&&capacityValid&&segmentsValid&&partsValid&&options.media_types.includes(mediaType);
 async function submit(event:React.FormEvent) {
  event.preventDefault();if(!valid||submitting.current)return;
  submitting.current=true;setBusy(true);setError('');
  try {
   if(options?.pending_creation&&!batch){
    await request<Cartridge>('/v1/cartridges',{method:'POST',signal:AbortSignal.timeout(310000),headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});
   }else{
    const value=attempted.current??{request_id:requestId.current,count:countNumber,media_type:mediaType,capacity_bytes:capacityMib*1024**2,segment_size_mib:segmentMib,multipart_part_size_mib:partMib};
    attempted.current=value;
    for(;;){
     const result=await request<Batch>('/v1/cartridges/batch',{method:'POST',signal:AbortSignal.timeout(310000),headers:{'Content-Type':'application/json'},body:JSON.stringify(value)});
     if(!alive.current)return;
     setBatch(result);
     if(result.phase==='completed')break;
    }
   }
   if(!alive.current)return;
   toast.success(options?.pending_creation&&!batch?t("Cartouche créée"):(isPlural(countNumber) ? t("{count} cartouches créées", { count: countNumber }) : t("{count} cartouche créée", { count: countNumber })),{description:options?.online_creation?t("Les cartouches ont été ajoutées à la bibliothèque."):t("Quittez la maintenance pour rendre les cartouches accessibles à PBS.")});
   try {await onRefresh();}catch {toast.error(t("Création terminée, mais le rafraîchissement a échoué. Actualisez la bibliothèque."));}
   onClose();
  }catch(cause){
   if(alive.current){
    setError(cause instanceof Error?cause.message:t("La création a échoué."));
    try {const updated=await request<Options>('/v1/cartridges/options');if(alive.current){setOptions(updated);if(updated.pending_batch)setBatch(updated.pending_batch);}}catch{/* Keep the same request for a safe retry. */}
   }
  }
  finally {submitting.current=false;if(alive.current)setBusy(false);}
 }
 return <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg" showCloseButton={!busy} onEscapeKeyDown={e=>{if(busy)e.preventDefault();}} onInteractOutside={e=>{if(busy)e.preventDefault();}}>
  <DialogHeader icon={Videotape}><DialogTitle>{t("Créer des cartouches")}</DialogTitle><DialogDescription>{t("Créez un lot de cartouches avec les mêmes paramètres dans la bibliothèque sélectionnée.")}</DialogDescription></DialogHeader>
  {loadError?<Alert variant="destructive"><AlertTitle>{t("Options indisponibles")}</AlertTitle><AlertDescription>{t(loadError)}<Button variant="outline" size="sm" onClick={()=>setRevision(v=>v+1)}>{t("Réessayer")}</Button></AlertDescription></Alert>:!options?<p role="status" className="flex gap-2 text-sm"><LoaderCircle className="size-4 animate-spin"/>{t("Lecture des capacités de la bibliothèque…")}</p>:<form onSubmit={submit} className="space-y-5">
   {options.pending_creation&&!batch&&<Alert><AlertTitle>{t("Création à reprendre")}</AlertTitle><AlertDescription>{t("Une création a été interrompue. Ses paramètres sont conservés pour terminer l’opération sans recréer le média.")}</AlertDescription></Alert>}
   {available&&options.online_creation&&<p className="text-sm text-muted-foreground">{t("Ajout à chaud disponible. Les sauvegardes PBS peuvent continuer pendant la création.")}</p>}
   {options.demo&&<p className="lto-demo-label w-fit">{t("Démonstration locale · aucun média réel créé")}</p>}
   {(!available)&&<Alert><AlertTitle>{t("Création indisponible")}</AlertTitle><AlertDescription>{!connected?t("Connexion au service interrompue."):activeJobs?t("Attendez la fin des opérations en cours."):t(options.reason || "La bibliothèque ne permet pas la création actuellement.")}{options.configured&&!options.ready&&!options.online_creation&&<a className="underline" href={vtlHash('media/maintenance',id)} onClick={onClose}>{t("Ouvrir la maintenance")}</a>}</AlertDescription></Alert>}
   {batch&&<Alert><AlertTitle>{busy?t("Création du lot"):t("Lot à reprendre")}</AlertTitle><AlertDescription><span role="status" aria-live="polite">{t(isPlural(batch.completed) ? "{completed} / {count} cartouches créées." : "{completed} / {count} cartouche créée.", { completed: batch.completed, count: batch.request.count })}</span> {t("Les paramètres et identifiants sont conservés.")}</AlertDescription></Alert>}
   <div className="space-y-2"><Label htmlFor="create-count">{t("Nombre de cartouches")}</Label><Input id="create-count" autoFocus type="number" min="1" max={Math.min(64,options.free_slots)} step="1" value={count} onChange={e=>setCount(e.target.value)} disabled={busy||recovering} aria-describedby="create-count-help" aria-invalid={!recovering&&!countValid}/><p id="create-count-help" className="text-xs text-muted-foreground">{t(isPlural(options.free_slots) ? "{count} emplacements libres. Identifiants attribués automatiquement à la suite de cette VTL." : "{count} emplacement libre. Identifiants attribués automatiquement à la suite de cette VTL.", { count: options.free_slots })}</p></div>
   {options.pending_creation&&!batch&&<p className="text-sm">{t("Cartouche à reprendre :")} {barcode}</p>}
   <div className="space-y-1 text-sm" aria-live="polite"><p>{t("Capacité du lot :")} <strong>{formatBytes(countNumber*capacityMib*1024**2)}</strong></p><p className="text-muted-foreground">{t("Disponible pour de nouvelles cartouches :")} {options.storage?formatBytes(options.storage.available_bytes):t("indisponible")}</p><p className="text-xs text-muted-foreground">{t("Les capacités restantes des cartouches et lots du même volume sont prises en compte, avec une marge de métadonnées par cartouche.")}</p>{!recovering&&!spaceValid&&<p role="alert" className="text-destructive">{options.storage?t("Espace insuffisant pour ce lot. Réduisez le nombre ou la capacité des cartouches."):t("Le stockage doit être vérifié avant de créer un lot.")}</p>}</div>
   <div className="space-y-2"><Label htmlFor="create-media-type">{t("Type de cartouche")}</Label><Select id="create-media-type" value={mediaType} onValueChange={(selectedValue) => setMediaType(selectedValue)} disabled={busy||recovering||!options.media_types.length} className="w-full">{!options.media_types.length&&<SelectOption value="">{t("Aucun type compatible")}</SelectOption>}{options.media_types.map(type=><SelectOption key={type} value={type}>{type.replace('LTO','LTO-')}</SelectOption>)}</Select><p className="text-xs text-muted-foreground">{t("Types compatibles avec les lecteurs de cette bibliothèque.")}</p></div>
   <div className="space-y-2"><Label htmlFor="create-capacity">{t("Capacité virtuelle")}</Label><div className="grid grid-cols-[minmax(0,1fr)_5.5rem_4.5rem] items-center gap-2"><SizeSlider label={t("Régler la capacité virtuelle")} description="create-capacity-help" value={capacityMib} min={options.min_capacity_mib} max={options.max_capacity_mib} disabled={busy||recovering} onChange={value=>{const divisor=value>=1048576?1048576:value>=1024?1024:1;setCapacity(String(value/divisor));setUnit(String(divisor));}}/><Input className="text-right tabular-nums" id="create-capacity" type="number" step="any" min={options.min_capacity_mib/Number(unit)} max={options.max_capacity_mib/Number(unit)} value={capacity} onChange={e=>setCapacity(e.target.value)} disabled={busy||recovering} aria-describedby="create-capacity-help"/><Select className="w-full" aria-label={t("Unité de capacité")} value={unit} onValueChange={(selectedValue) => {if(capacity.trim())setCapacity(String(capacityMib/Number(selectedValue)));setUnit(selectedValue);}} disabled={busy||recovering}><SelectOption value="1">{t("Mio")}</SelectOption><SelectOption value="1024">{t("Gio")}</SelectOption><SelectOption value="1048576">{t("Tio")}</SelectOption></Select></div><p id="create-capacity-help" className="text-xs text-muted-foreground">{t("De {min} à {max}, par multiples de 1 Mio.", { min: formatBytes(options.min_capacity_mib*1024**2), max: formatBytes(options.max_capacity_mib*1024**2) })}</p></div>
   <fieldset className="space-y-4 border-t pt-4"><legend className="pr-2 text-sm font-medium">{t("Découpage et transfert vers S3")}</legend>
    <div className="space-y-2"><Label htmlFor="create-segments">{t("Taille des segments de la tape")}</Label><div className="grid grid-cols-[minmax(0,1fr)_5.5rem_4.5rem] items-center gap-2"><SizeSlider label={t("Régler la taille des segments")} description="create-segments-help" value={segmentMib} min={options.min_segment_size_mib} max={options.max_segment_size_mib} disabled={busy||recovering} onChange={setSegmentMib}/><Input className="text-right tabular-nums" id="create-segments" type="number" min={options.min_segment_size_mib/Number(segmentUnit)} max={options.max_segment_size_mib/Number(segmentUnit)} step="any" value={segments} onChange={e=>setSegments(e.target.value)} disabled={busy||recovering} aria-describedby="create-segments-help"/><Select className="w-full" aria-label={t("Unité des segments")} value={segmentUnit} onValueChange={(selectedValue) => {if(segments.trim())setSegments(String(segmentMib/Number(selectedValue)));setSegmentUnit(selectedValue);}} disabled={busy||recovering}><SelectOption value="1">{t("Mio")}</SelectOption><SelectOption value="1024">{t("Gio")}</SelectOption><SelectOption value="1048576">{t("Tio")}</SelectOption></Select></div><p id="create-segments-help" className="text-xs text-muted-foreground">{t("Un segment devient un objet S3, jusqu’à {max}. Chaque fichier est découpé séparément ; son dernier segment peut être plus petit.", { max: formatBytes(options.max_segment_size_mib*1024**2) })}</p></div>
    <div className="space-y-2"><Label htmlFor="create-parts">{t("Taille des parts multipart (Mio)")}</Label><div className="grid grid-cols-[minmax(0,1fr)_5.5rem_4.5rem] items-center gap-2"><SizeSlider label={t("Régler la taille des parts multipart")} description="create-parts-help" value={partMib} min={options.min_multipart_part_size_mib??16} max={options.max_multipart_part_size_mib??1024} disabled={busy||recovering} onChange={value=>setPartSize(String(value))}/><Input className="text-right tabular-nums" id="create-parts" type="number" min={options.min_multipart_part_size_mib??16} max={options.max_multipart_part_size_mib??1024} step="1" value={partSize} onChange={e=>setPartSize(e.target.value)} disabled={busy||recovering} aria-describedby="create-parts-help" aria-invalid={partCount>10000}/><span className="text-sm text-muted-foreground" aria-hidden="true">{t("Mio")}</span></div><p id="create-parts-help" className="text-xs text-muted-foreground">{t("Les gros segments sont envoyés par parts, puis assemblés en un seul objet S3. Ce réglage ne change pas les écritures PBS.")}</p>{partCount>10000&&<p role="alert" className="text-xs text-destructive">{t("Ce réglage dépasse 10 000 parts par segment. Choisissez au moins {min} Mio par part.", { min: Math.ceil(segmentMib/10000) })}</p>}</div>
   </fieldset>
   <p className="text-xs text-muted-foreground">{t(isPlural(options.free_slots) ? "{count} emplacements libres. Chaque cartouche sera placée dans un emplacement disponible." : "{count} emplacement libre. Chaque cartouche sera placée dans un emplacement disponible.", { count: options.free_slots })}</p>
   {error&&<Alert variant="destructive"><AlertTitle>{t("Création non confirmée")}</AlertTitle><AlertDescription>{t(error)}</AlertDescription></Alert>}
   <DialogFooter><Button variant="outline" type="button" disabled={busy} onClick={onClose}>{t("Fermer")}</Button><Button type="submit" disabled={!valid||busy}>{busy&&<LoaderCircle className="animate-spin"/>}{busy?t("Création {completed} / {count}…", { completed: batch?.completed ?? 0, count }):options.pending_creation&&!batch?t("Reprendre la création"):recovering?t("Reprendre le lot"):(isPlural(countNumber) ? t("Créer {count} cartouches", { count }) : t("Créer {count} cartouche", { count }))}</Button></DialogFooter>
  </form>}
 </DialogContent>;
}
