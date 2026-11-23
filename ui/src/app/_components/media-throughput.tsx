"use client";
import {useI18n,getLocale} from '@/lib/i18n';
import {useContext,useEffect,useState} from 'react';
import {Area,AreaChart,CartesianGrid,ResponsiveContainer,Tooltip,XAxis,YAxis} from 'recharts';
import {formatBytes} from '@/lib/api';
import {mergeRateHistory,type RatePoint} from '@/lib/telemetry';
import {VtlScope} from './vtl-scope';
import {useThroughputHistory} from './use-throughput-history';
import {Select,SelectOption} from '@/components/ui/select';
import {Button} from '@/components/ui/button';
import {Input} from '@/components/ui/input';
import {ChevronLeft,ChevronRight} from 'lucide-react';

const scales=[{seconds:120,label:'Instantané'},{seconds:3600,label:'1 h'},{seconds:86400,label:'24 h'},{seconds:604800,label:'7 jours'},{seconds:2592000,label:'30 jours'}];
/** Selector value of the curve that sums every tape. */
export const ALL_TAPES='{"aggregate":true}';
const timeLabel=(time:number)=>new Date(time).toLocaleTimeString(getLocale(),{hour:'2-digit',minute:'2-digit',second:'2-digit'});
type Props={barcode:string;driveId?:string;transferId?:string;points:RatePoint[];writeRate:number|null;localGrowth:number|null;available:boolean;metricTitle?:string;cartridgeHistory?:boolean;initialEndMs?:number};
export function MediaThroughput(props:Props) {
 const id=useContext(VtlScope);
 return <Throughput key={`${id}:${props.driveId}:${props.barcode}:${props.transferId}`} {...props}/>;
}
function Throughput({barcode,driveId,transferId,points,writeRate,localGrowth,available,metricTitle,cartridgeHistory=false,initialEndMs}:Props) {
 const {t}=useI18n();
 const id=useContext(VtlScope);
 const preference=`vaultcenter:throughput-scale:${id}${cartridgeHistory?':cartridge':''}`;
 const [seconds,setSeconds]=useState(cartridgeHistory?86400:120);
 const [endMs,setEndMs]=useState<number|undefined>(initialEndMs);
 const [historyFocused,setHistoryFocused]=useState(initialEndMs!==undefined);
 const [selected,setSelected]=useState('');
 const originalKind=metricTitle?'s3':'pbs';
 const [kind,setKind]=useState<'pbs'|'s3'|'s3_restore'>(originalKind);
 const currentSource=kind===originalKind;
 const scopedDrive=currentSource?driveId:undefined;
 useEffect(()=>{try{const saved=Number(localStorage.getItem(preference));if(scales.some(s=>s.seconds===saved))setSeconds(saved);}catch{}},[preference]);
 // Every tape summed: when the operator picks it, or by default when this drive holds no tape for the shown transfer.
 const aggregate=!cartridgeHistory&&(selected===ALL_TAPES||(!selected&&!(currentSource&&barcode)));
 const selection=selected&&selected!==ALL_TAPES?JSON.parse(selected) as {barcode:string;drive_id?:string;transfer_id?:string}:null;
 const shownBarcode=cartridgeHistory?barcode:aggregate?'':selection?.barcode??(currentSource?barcode:'');
 const shownTransfer=aggregate?undefined:selection?selection.transfer_id:(currentSource?transferId:undefined);
 const {data,error,loading}=useThroughputHistory({barcode:shownBarcode,driveId:selection?.drive_id??scopedDrive,transferId:shownTransfer,kind,seconds,endMs,aggregate});
 // The selector needs the full inventory, independent of the selected curve.
 // Cartridge history also keeps that inventory anchored to now for older windows.
 const inventory=useThroughputHistory({barcode:'',driveId:scopedDrive,kind,seconds:cartridgeHistory?86400:seconds,endMs:cartridgeHistory?undefined:endMs});
 const live=endMs===undefined&&currentSource&&!aggregate&&shownBarcode===barcode&&shownTransfer===transferId;
 const end=endMs??(seconds===120?Date.now():(data?.end_ms??Date.now()));
 const start=seconds===120?end-120000:(data?.start_ms??end-seconds*1000);
 const livePoints=live&&seconds===120?points.filter(p=>p.at>=start):[];
 const historic=(data?.points??[]).filter(point=>point.at>=start&&point.at<=end);
 // Persisted measurements bridge browser absences; genuine acquisition gaps remain.
 const combined=mergeRateHistory(historic,livePoints);
 const copy=kind!=='pbs'||(live&&writeRate!==null)||combined.some(p=>p.writeRate!==null);
 const title=kind==='s3_restore'?t('Débit depuis S3'):kind==='s3'?t('Débit vers S3'):copy?t('Débit de copie'):t('Croissance du volume local');
 const recent=historic.filter(p=>(aggregate&&!copy?p.localGrowth:p.writeRate)!==null).at(-1);
 const current=available&&live?(copy?writeRate:localGrowth):(kind!=='pbs'||aggregate)&&endMs===undefined&&recent&&Date.now()-recent.at<=15000?(aggregate&&!copy?recent.localGrowth:recent.writeRate):null;
 const values=combined.map(p=>({at:p.at,rate:copy?p.writeRate:p.localGrowth}));
 const hasValues=values.some(p=>p.rate!==null);
 const axisLabel=(at:number)=>seconds>=604800?new Date(at).toLocaleDateString(getLocale(),{day:'2-digit',month:'2-digit'}):seconds>=86400?new Date(at).toLocaleString(getLocale(),{day:'2-digit',month:'2-digit',hour:'2-digit',minute:'2-digit'}):timeLabel(at);
 const series=(inventory.data?.series??data?.series??[]).filter(s=>!cartridgeHistory||s.barcode===barcode);
 const keyFor=(s:{barcode:string;drive_id?:string;transfer_id?:string})=>JSON.stringify({barcode:s.barcode,...(!scopedDrive&&s.drive_id?{drive_id:s.drive_id}:{}),...(s.transfer_id?{transfer_id:s.transfer_id}:{})});
 const matching=series.find(s=>s.barcode===shownBarcode&&s.transfer_id===shownTransfer);
 const selectedKey=aggregate?ALL_TAPES:selected||(matching?keyFor(matching):JSON.stringify({barcode:shownBarcode,...(shownTransfer?{transfer_id:shownTransfer}:{})}));
 const cutoff=Date.now()-(data?.retention_days??30)*86400000;
 const latest=series.filter(s=>!shownTransfer||s.transfer_id===shownTransfer).reduce((last,s)=>Math.max(last,s.last_at),0);
 useEffect(()=>{
  if(!cartridgeHistory||historyFocused||!data)return;
  setHistoryFocused(true);
  if(seconds!==120&&latest&&latest<start)setEndMs(Math.min(Date.now(),latest+1000));
 },[cartridgeHistory,historyFocused,data,latest,start,seconds]);
 const localDate=(at:number)=>{const date=new Date(at);return new Date(at-date.getTimezoneOffset()*60000).toISOString().slice(0,16);};
 function chooseScale(next:number){setSeconds(next);if(next===120)setEndMs(undefined);try{localStorage.setItem(preference,String(next));}catch{}}
 return <figure className="lto-throughput" aria-label={t("{title} de {barcode}", {title,barcode:aggregate?t('toutes les cartouches'):shownBarcode||t('ce lecteur')})}>
  <figcaption className="lto-throughput-caption lto-throughput-selectors">
   <Select size="sm" aria-label={t("Type de transfert du graphique")} className="shrink-0" value={kind} onValueChange={(selectedValue) => {setSelected('');setKind(selectedValue as 'pbs'|'s3'|'s3_restore');if(cartridgeHistory){setEndMs(undefined);setHistoryFocused(false);}}}><SelectOption value="pbs">{t("Backup Client → VAULTCENTER")}</SelectOption><SelectOption value="s3">{t("VAULTCENTER → S3")}</SelectOption><SelectOption value="s3_restore">{t("VAULTCENTER → LTO")}</SelectOption></Select>
   <Select size="sm" disabled={!series.length&&!shownBarcode&&!selected} aria-label={t(cartridgeHistory?"Copie du graphique":"Cartouche du graphique")} className="max-w-44" value={selectedKey} onValueChange={(selectedValue) => {setSelected(selectedValue);const chosen=series.find(s=>keyFor(s)===selectedValue);if(cartridgeHistory&&chosen&&seconds!==120)setEndMs(Math.min(Date.now(),chosen.last_at+1000));}}>
    {!cartridgeHistory&&<SelectOption value={ALL_TAPES}>{t("Toutes les cartouches")}</SelectOption>}
    {!series.length&&!aggregate&&<SelectOption value={selectedKey}>{shownBarcode||t('Aucun historique')}</SelectOption>}
    {currentSource&&barcode&&!series.some(s=>s.barcode===barcode&&s.transfer_id===transferId)&&<SelectOption value={JSON.stringify({barcode,...(transferId?{transfer_id:transferId}:{})})}>{barcode}</SelectOption>}
    {series.map(s=>{const key=keyFor(s);return <SelectOption key={key} value={key}>{s.barcode}{!scopedDrive&&kind==='pbs'?t(" · Lecteur {id}", {id:s.drive_id}):''}{s.transfer_id?` · ${new Date(s.last_at).toLocaleString(getLocale())}`:''}</SelectOption>;})}
   </Select>
  </figcaption>
  <div className="lto-throughput-controls">
   <div role="group" aria-label={t("Échelle de temps")} className="lto-throughput-scales">{scales.map(scale=><button type="button" key={scale.seconds} aria-pressed={seconds===scale.seconds} onClick={()=>chooseScale(scale.seconds)}>{t(scale.label)}</button>)}</div>
   {!cartridgeHistory&&<strong className="lto-throughput-value" title={current===null?t('Débit indisponible'):t('Débit actuel')}>{current===null?<><span className="lto-throughput-unavailable">{t("Débit indisponible")}</span><span className="lto-throughput-unavailable-short" aria-hidden="true">—</span></>:`${formatBytes(current)}/s`}</strong>}
  </div>
  {cartridgeHistory&&seconds!==120&&<div className="lto-history-navigation">
   <div className="flex items-center gap-2">
    <Button variant="outline" size="icon-sm" aria-label={t('Période précédente')} disabled={end-seconds*1000<=cutoff} onClick={()=>setEndMs(end-seconds*1000)}><ChevronLeft/></Button>
    <Input type="datetime-local" aria-label={t('Fin de la période')} min={localDate(cutoff)} max={localDate(Date.now())} value={localDate(end)} onChange={event=>{const value=new Date(event.target.value).getTime();if(Number.isFinite(value)&&value>=cutoff&&value<=Date.now())setEndMs(value);}}/>
    <Button variant="outline" size="icon-sm" aria-label={t('Période suivante')} disabled={endMs===undefined} onClick={()=>setEndMs(end+seconds*1000>=Date.now()?undefined:end+seconds*1000)}><ChevronRight/></Button>
   </div>
   <div className="flex flex-wrap gap-2"><Button variant="ghost" size="sm" disabled={!latest} onClick={()=>setEndMs(Math.min(Date.now(),latest+1000))}>{t('Dernières mesures')}</Button><Button variant="ghost" size="sm" disabled={endMs===undefined} onClick={()=>setEndMs(undefined)}>{t('Revenir à maintenant')}</Button></div>
  </div>}
  {cartridgeHistory&&<p className="text-sm font-medium">{t('{title} (Mio/s)',{title})}</p>}
  <div className="lto-throughput-plot" aria-hidden="true">
   {hasValues?<ResponsiveContainer width="100%" height="100%" minWidth={0}>
    <AreaChart data={values} margin={{top:5,right:8,bottom:0,left:0}}>
     <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="3 3"/>
     <XAxis dataKey="at" type="number" domain={[start,Math.max(end,livePoints.at(-1)?.at??end)]} tickFormatter={axisLabel} tick={{fontSize:10,fill:'var(--muted-foreground)'}} axisLine={false} tickLine={false} minTickGap={45}/>
     <YAxis domain={[0,'auto']} width={36} tickFormatter={v=>String(Math.round(Number(v)/1024**2*10)/10)} tick={{fontSize:10,fill:'var(--muted-foreground)'}} axisLine={false} tickLine={false}/>
     <Tooltip labelFormatter={v=>new Date(Number(v)).toLocaleString(getLocale())} formatter={v=>[`${formatBytes(Number(v))}/s`,title]} contentStyle={{background:'var(--popover)',borderColor:'var(--border)',color:'var(--foreground)',borderRadius:6,fontSize:12}}/>
     <Area dataKey="rate" type="linear" stroke="var(--primary)" fill="var(--primary)" fillOpacity={0.12} strokeWidth={2} connectNulls={false} isAnimationActive={false} dot={values.some((p,i)=>p.rate!==null&&(values[i-1]?.rate??null)===null&&(values[i+1]?.rate??null)===null)?{r:3}:false}/>
    </AreaChart>
   </ResponsiveContainer>:<div className="flex h-full items-center justify-center text-sm text-muted-foreground">{loading?t('Chargement des mesures…'):error?t('Historique indisponible'):seconds===120?t('En attente de mesures de débit'):t('Aucune mesure enregistrée sur cette période')}</div>}
  </div>
  {!copy&&<p className="lto-throughput-note">{t("Variation des fichiers sur disque, distincte du débit réseau du client de sauvegarde.")}</p>}
  {cartridgeHistory&&kind!=='pbs'&&!loading&&!error&&!hasValues&&<p className="lto-throughput-note">{t(kind==='s3_restore'?'Les récupérations réalisées sans enregistrement du débit n’ont pas de courbe.':'Les exports réalisés sans enregistrement du débit n’ont pas de courbe.')}</p>}
  {cartridgeHistory&&<p className="lto-throughput-note">{t('Mesures conservées {days} jours. Les périodes sans relevé restent vides.',{days:data?.retention_days??30})}</p>}
  <div className="sr-only"><table aria-label={t("Historique du débit")}><thead><tr><th>{t("Heure")}</th><th>{title}</th></tr></thead><tbody>{values.map(p=><tr key={p.at}><td>{new Date(p.at).toLocaleString(getLocale())}</td><td>{p.rate===null?t('Indisponible'):`${formatBytes(p.rate)}/s`}</td></tr>)}</tbody></table></div>
 </figure>;
}
