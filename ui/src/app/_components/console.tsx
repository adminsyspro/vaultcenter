"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";

import {useCallback,useEffect,useRef,useState} from 'react';
import {useTheme} from 'next-themes';
import {Activity,HardDrive,LayoutDashboard,Moon,RefreshCw,Server,Settings2,Sun,Videotape,WifiOff} from 'lucide-react';
import {cn} from 'cn';
import {Button} from '@/components/ui/button';
import {Alert,AlertDescription,AlertTitle} from '@/components/ui/alert';
import {Skeleton} from '@/components/ui/skeleton';
import {Select,SelectOption} from '@/components/ui/select';
import {type AuthUser,isActive} from '@/lib/api';
import {Account} from './account';
import {LanguageSelector} from './language-selector';
import {AccountMenu} from './account-menu';
import {StorageGauge} from './storage-gauge';
import {ActivityBell} from './activity-bell';
import {VaultCenterMark} from './vaultcenter-mark';
import {LibraryControls} from './library-controls';
import {LibraryStatus} from './library-status';
import {Notifications} from './notifications';
import {useConsole} from './use-console';
import {useTelemetry} from './use-telemetry';
import {Overview} from './overview';
import {MediaTable} from './media-table';
import {Jobs} from './jobs';
import {Storage} from './storage';
import {Fleet} from './fleet';
import {Vtls} from './vtls';
import {VtlScope} from './vtl-scope';
import {useVtlSelection,vtlHash,vtlStorageHash,vtlWindowHash} from './use-vtl-selection';

type View='media'|'jobs'|'overview'|'storage'|'notifications'|'vtls'|'account'|'profile'|'fleet';
const pages:Record<View,{label:string;description:string}>={
 media:{label:'Bandes et lecteurs',description:''},
 fleet:{label:'Vue du parc',description:'Synthèse de toutes les bibliothèques, avec leur alimentation et leur maintenance.'},
 jobs:{label:'Activité',description:'Opérations en cours et résultats de la bibliothèque.'},
 overview:{label:'Vue d’ensemble',description:'État de la bibliothèque et opérations à suivre.'},
 storage:{label:'Stockage S3',description:'Buckets, archivage et configuration de la connexion S3.'},
 notifications:{label:'Notifications',description:'Envoi des emails et suivi des notifications de toutes les bibliothèques.'},
 vtls:{label:'Bibliothèques virtuelles',description:'Créez, configurez et gérez les bibliothèques de cette appliance.'},
 account:{label:'Utilisateurs',description:'Comptes, rôles et accès à cette appliance.'},
 profile:{label:'Profil',description:'Votre identité, votre mot de passe et vos jetons d’accès.'},
};
type ConsoleProps={user:AuthUser;onLogout:()=>Promise<void>;logoutBusy:boolean;onSessionEnded:()=>void};
export function Console(props:ConsoleProps){
 const selection=useVtlSelection();
 return <VtlScope.Provider key={selection.id??'unselected'} value={selection.id}><ScopedConsole {...props} selection={selection}/></VtlScope.Provider>;
}
function ScopedConsole({user,onLogout,logoutBusy,onSessionEnded,selection}:ConsoleProps&{selection:ReturnType<typeof useVtlSelection>}){
 const {t,locale}=useI18n();
 const {data,error,refreshing,updatedAt,refresh}=useConsole(selection.id);
 const telemetry=useTelemetry(selection.id,true);
 const refreshLibrary=useCallback(async()=>{await Promise.allSettled([refresh(),telemetry.refresh()]);},[refresh,telemetry.refresh]);
 const [view,setView]=useState<View>('media');
 const {resolvedTheme,setTheme}=useTheme();
 const screen=useRef<HTMLElement>(null);
 const canManage=user.role==='admin';
 useEffect(()=>{
  const read=()=>{
   const [hash,query='']=location.hash.slice(1).split('?');
   const suffix=query?`?${query}`:selection.id?`?vtl=${encodeURIComponent(selection.id)}`:'';
   if(hash==='maintenance'){location.replace(`#media/maintenance${suffix}`);setView('media');return;}
   if(hash==='tape-storage'){location.replace(`#media${suffix}`);setView('media');return;}
   // Former S3 configuration routes now open the library's Stockage dialog on its S3 tab.
   if(hash==='s3settings'||hash==='storage/configuration'){const id=new URLSearchParams(query).get('vtl')??selection.id;if(canManage&&id){location.replace(vtlStorageHash(id));setView('vtls');}else{location.replace(`#storage${suffix}`);setView('storage');}return;}
   if(hash==='media/maintenance')return;
   if(hash.startsWith('storage/')){setView('storage');return;}
   // The former « Réglages › Bibliothèque » page opens the VTL window on Stockage › Local.
   if(hash==='settings'){const id=new URLSearchParams(query).get('vtl')??selection.id;if(canManage&&id){location.replace(vtlWindowHash(id,'storage','local'));setView('vtls');}else{history.replaceState(null,'',`#profile${suffix}`);setView('profile');}return;}
   if(['account','notifications'].includes(hash)&&!canManage){history.replaceState(null,'',`#profile${suffix}`);setView('profile');return;}
   setView(Object.hasOwn(pages,hash)?hash as View:'media');
  };
  read();window.addEventListener('hashchange',read);return()=>window.removeEventListener('hashchange',read);
 },[canManage,selection.id]);
 useEffect(()=>{if(screen.current)screen.current.scrollTop=0;},[view]);
 const connected=!!data&&!error&&data.status.status==='ok';
 const globalView=['vtls','account','profile','notifications','fleet'].includes(view);
 const settingsView=['notifications','account','profile'].includes(view);
 const section=view==='fleet'?'fleet':view==='media'?'media':['jobs','overview'].includes(view)?'jobs':view==='storage'?'storage':'settings';
 const nav=[{id:'media',label:t("Bibliothèque"),icon:HardDrive},{id:'jobs',label:t("Activité"),icon:Activity},{id:'storage',label:t("Stockage S3"),icon:Server},{id:canManage?'notifications':'profile',label:t("Réglages"),icon:Settings2}];
 const activeJobs=data?.jobs.filter(isActive).length??0;
 // Created, deleted or resized cartridges change what the volume can still hold.
 const libraryRevision=data?.cartridges.map(c=>`${c.barcode}:${c.capacity_bytes}:${c.local_deleted?1:0}`).join()??'';
 return <div className="lto-console-page">
  <a href="#main-content" className="lto-skip-link" onClick={e=>{e.preventDefault();screen.current?.focus();}}>{t("Aller au contenu")}</a>
  <div className="lto-chassis lto-console">
   <div className="lto-rack-ear lto-rack-ear-left" aria-hidden="true"><i/><i/><i/></div>
   <div className="lto-rack-ear lto-rack-ear-right" aria-hidden="true"><i/><i/><i/></div>
   <header className="lto-chassis-header">
    <div className="lto-console-facade">
     <a className="lto-console-brand" href={vtlHash('media',selection.id)} aria-label={t("VaultCenter — bibliothèque")}><VaultCenterMark className="lto-console-mark"/><span className="lto-console-brand-text"><span className="lto-chassis-brand">Vault<span>Center</span></span><span className="lto-console-model">{t("Bibliothèque LTO")}</span></span></a>
     <div className="lto-console-selector">
      <label htmlFor="vtl-selection" className="sr-only">{t("Bibliothèque VTL")}</label>
      <Select id="vtl-selection" value={selection.id??''} disabled={!selection.loaded} onValueChange={(selectedValue) => {if(view==='fleet')location.hash=vtlHash('media',selectedValue);else selection.select(selectedValue);}}>
       <SelectOption value="" disabled>{selection.loaded?t("Choisir une VTL"):t("Chargement…")}</SelectOption>
       {selection.libraries.map(vtl=><SelectOption key={vtl.id} value={vtl.id}>{vtl.name}{vtl.lifecycle==='draft'?t(" · À configurer"):''}</SelectOption>)}
      </Select>
      <a href={vtlHash('vtls',selection.id)} className="lto-console-manage" aria-label={t("Bibliothèques virtuelles")} title={t("Gérer les bibliothèques VTL")}><Videotape aria-hidden="true"/><span>{t("Gérer les VTL")}</span></a>
      <a href={vtlHash('fleet',selection.id)} className="lto-console-manage" aria-current={view==='fleet'?'page':undefined} title={t("Synthèse de toutes les VTL")}><LayoutDashboard aria-hidden="true"/><span>{t("Vue du parc")}</span></a>
     </div>
     <nav className="lto-console-nav" aria-label={t("Navigation de la bibliothèque")}>
      {nav.map(item=><a key={item.id} href={vtlHash(item.id,selection.id)} aria-current={section===(['profile','notifications'].includes(item.id)?'settings':item.id)?'page':undefined}><item.icon aria-hidden="true"/><span>{t(item.label)}</span>{item.id==='jobs'&&activeJobs>0&&<span className="lto-console-count">{activeJobs}</span>}</a>)}
     </nav>
     <div className="lto-console-utilities">
      <LanguageSelector/>
      <Button variant="ghost" size="icon" aria-label={t("Changer le thème")} onClick={()=>setTheme(resolvedTheme==='dark'?'light':'dark')}>{resolvedTheme==='dark'?<Sun/>:<Moon/>}</Button>
      <ActivityBell selectedVtl={selection.id}/>
      <AccountMenu user={user} onLogout={onLogout} logoutBusy={logoutBusy}/>
     </div>
     {selection.id&&view!=='fleet'&&<LibraryControls canManage={canManage} connected={connected} jobs={data?.jobs??[]} onRefresh={refreshLibrary} data={telemetry.data}/>}
    </div>
   </header>
   <main ref={screen} id="main-content" tabIndex={-1} className="lto-console-screen">
    <div className={view==='media'?'lto-console-media':'lto-console-panel'}>
     {view==='media'?<h1 className="sr-only">{t("Bandes et lecteurs")}</h1>:<div className="lto-console-page-title"><div><h1>{t(pages[view].label)}</h1><p>{t(pages[view].description)}</p></div>{['jobs','overview'].includes(view)&&<Button variant="outline" disabled={refreshing} onClick={()=>void refresh()}><RefreshCw className={cn(refreshing&&'animate-spin')}/>{t("Actualiser")}</Button>}</div>}
     {settingsView&&<nav className="lto-console-subnav" aria-label={t("Rubriques des réglages")}>
      {(canManage?[{id:'notifications',label:t("Notifications")},{id:'account',label:t("Utilisateurs")},{id:'profile',label:t("Profil")}]:[{id:'profile',label:t("Profil")}]).map(item=><a key={item.id} href={vtlHash(item.id,selection.id)} aria-current={view===item.id?'page':undefined}>{t(item.label)}</a>)}
     </nav>}
     {['jobs','overview'].includes(view)&&<nav className="lto-console-subnav" aria-label={t("Rubriques de l’activité")}><a href={vtlHash('jobs',selection.id)} aria-current={view==='jobs'?'page':undefined}>{t("Opérations")}</a><a href={vtlHash('overview',selection.id)} aria-current={view==='overview'?'page':undefined}>{t("Vue d’ensemble")}</a></nav>}
     {selection.error&&<Alert variant="destructive"><AlertTitle>{t("Bibliothèques indisponibles")}</AlertTitle><AlertDescription>{t(selection.error)}</AlertDescription></Alert>}
     {selection.unknown&&<Alert variant="destructive"><AlertTitle>{t("Bibliothèque introuvable")}</AlertTitle><AlertDescription>{t("Choisissez une bibliothèque dans le sélecteur en façade.")}</AlertDescription></Alert>}
     {selection.loaded&&!selection.libraries.length&&<Alert><AlertTitle>{t("Aucune bibliothèque configurée")}</AlertTitle><AlertDescription><a href="#vtls" className="underline">{t("Ouvrir les bibliothèques VTL")}</a></AlertDescription></Alert>}
     {!globalView&&error&&<Alert variant="destructive"><WifiOff/><AlertTitle>{t("Connexion interrompue")}</AlertTitle><AlertDescription>{t(error)} {data?t("Les dernières données reçues restent affichées. Les actions sont suspendues jusqu’à la reconnexion."):t("Vérifiez que le service VaultCenter est accessible.")}</AlertDescription></Alert>}
     {selection.id&&!globalView&&!data&&!error&&<div role="status" aria-label={t("Chargement de la console")} className="p-5"><Skeleton className="h-48"/></div>}
     {data&&view==='media'&&<MediaTable cartridges={data.cartridges} jobs={data.jobs} connected={connected} canManage={canManage} onRefresh={refreshLibrary} telemetry={telemetry}
      refreshAction={<Button variant="outline" size="icon" title={t("Actualiser")} aria-label={t("Actualiser")} disabled={refreshing} onClick={()=>void refreshLibrary()}><RefreshCw className={cn(refreshing&&'animate-spin')}/></Button>}/>}
     {data&&view==='overview'&&<Overview data={data} onJobs={()=>{location.hash=vtlHash('jobs',selection.id);}}/>}
     {data&&view==='jobs'&&<Jobs jobs={data.jobs}/>}
     {selection.id&&view==='storage'&&<Storage vtlId={selection.id} vtlName={selection.selected?.name??''} needsConfiguration={selection.selected?.storage===null} ready={selection.selected?.lifecycle==='ready'} connected={connected} canManage={canManage} jobs={data?.jobs??[]} onRefresh={refresh}/>}
     {view==='vtls'&&<Vtls canManage={canManage}/>}
     {view==='fleet'&&<Fleet libraries={selection.libraries} canManage={canManage}/>}
     {view==='notifications'&&canManage&&<Notifications/>}
     {(view==='account'||view==='profile')&&<Account key={view} view={view==='profile'?'profile':'access'} user={user} onSessionEnded={onSessionEnded}/>}
    </div>
   </main>
   {view!=='storage'&&view!=='fleet'&&<LibraryStatus data={telemetry.data} connected={connected} selected={!!selection.id} gauge={<StorageGauge vtlId={selection.id} revision={libraryRevision}/>}/>}
  </div>
  <p className="lto-console-timestamp">{updatedAt?t('Actualisé à {time}',{time:updatedAt.toLocaleTimeString(locale)}):t("En attente du serveur")}</p>
 </div>;
}
