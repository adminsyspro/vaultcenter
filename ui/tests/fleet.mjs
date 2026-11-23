import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {selectOption} from './helpers/select.mjs';
const ui=await staticUi(),browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM||undefined,args:['--no-sandbox']});
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const GiB=1024**3,TiB=1024**4,now=()=>Date.now();
 const pbs={id:'00000000-0000-4000-8000-000000000001',name:'PBS',lifecycle:'ready',provisioning_revision:1};
 const veeam={id:'59033096-a6ee-474c-b24e-bae6e1ac205e',name:'VEEAM',lifecycle:'ready',provisioning_revision:3};
 const tape=(barcode,state,written,extra={})=>({barcode,state,media_type:'LTO8',capacity_bytes:12*TiB,bytes_written:written,generation:1,...extra});
 const telemetry=(id,drives,media)=>({version:1,vtl_id:id,generated_at:now(),observed_at:now(),source:'mhvtl',status:'available',reason:null,slots:[],
  drives:drives.map(d=>({origin_slot:null,observed_at:now(),bytes_written:null,...d})),media:media.map(m=>({observed_at:now(),local_growth_bps:null,logical_used_bytes:null,logical_capacity_bytes:null,...m}))});
 const disk=id=>({vtl_id:id,mode:'managed',phase:'ready',revision:1,path:'/srv/vaultcenter/tapes',capacity:{total_bytes:100*TiB,available_bytes:75*TiB},operation:null,message:null,data_locked:true,change_allowed:false,change_lock_reason:'data_locked'});
 let veeamDown=false,writeRate=173*1024**2;
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,user:{id:'t',username:'admin',role:'admin',disabled:false},csrf_token:'csrf'}}));
 await page.route('**/v1/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[pbs,veeam]});
  if(path===`/v1/vtls/${pbs.id}/cartridges`)return route.fulfill({json:[tape('PBS001L8','verified',2*TiB),tape('PBS002L8','writable',TiB),tape('PBS003L8','failed',0),tape('OLD001L8','archived',0,{local_deleted:true})]});
  if(path===`/v1/vtls/${veeam.id}/cartridges`)return veeamDown?route.fulfill({status:503,json:{error:'indisponible'}}):route.fulfill({json:[tape('CVT001L8','writable',0),tape('CVT002L8','sealed',0)]});
  if(path===`/v1/vtls/${veeam.id}/jobs`)return route.fulfill({json:[{id:'j1',kind:'export',subject:'CVT002L8',status:'running',progress:10,created_at:'2026-10-09 10:00:00',updated_at:'2026-10-09 10:00:00',message:JSON.stringify({export_progress:{phase:'uploading',completed_bytes:256*GiB,total_bytes:1024*GiB}})}]});
  if(path===`/v1/vtls/${veeam.id}/telemetry`)return route.fulfill({json:telemetry(veeam.id,[{id:'31',label:'Lecteur 1',state:'writing',barcode:'CVT001L8',write_rate_bps:writeRate}],[{barcode:'CVT001L8',local_size_bytes:512*GiB}])});
  if(path===`/v1/vtls/${pbs.id}/telemetry`)return route.fulfill({json:telemetry(pbs.id,[{id:'21',label:'Lecteur 1',state:'idle',barcode:null,write_rate_bps:null}],[])});
  if(path.endsWith('/maintenance'))return route.fulfill({json:{configured:true,client:path.includes(veeam.id)?'veeam':'pbs',phase:'online',ready:false,blocked:false,services:{'tgt.service':'active'}}});
  if(path.endsWith('/settings/tape-storage'))return route.fulfill({json:disk(path.split('/')[3])});
  if(path.endsWith('/status'))return route.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  return route.fulfill({json:[]});
 });
 await page.goto(`${ui.url}/#media?vtl=${pbs.id}`);
 // Own page, reached from the header, not a fake VTL in the selector.
 await expect(page.getByRole('combobox',{name:'Bibliothèque VTL'})).toBeVisible();
 await page.getByRole('combobox',{name:'Bibliothèque VTL'}).click();
 await expect(page.getByRole('listbox')).not.toContainText('Toutes les VTL');await page.keyboard.press('Escape');
 await page.getByRole('link',{name:'Vue du parc'}).click();
 await expect(page).toHaveURL(/#fleet/);
 const fleet=page.getByRole('region',{name:'Vue du parc'});
 // The status bar belongs to one VTL and stays off the fleet page.
 await expect(page.getByText('Sans cartouche',{exact:true})).toHaveCount(0);
 // Key figures: 5 live cartridges (deleted excluded); real usage (local size) instead of the catalog's 0.
 await expect(fleet).toContainText('Cartouches');
 await expect(fleet.getByText('5',{exact:true}).first()).toBeVisible();
 await expect(fleet).toContainText('3,5 Tio');
 await expect(fleet).toContainText('57,1 %');
 await expect(fleet).toContainText('75 Tio libres sur 100 Tio');
 // Each rack unit names every family with its count (never colour alone).
 await expect(fleet.getByRole('group',{name:'PBS : Inscriptible 1, Protégée S3 1, Échec 1'})).toBeVisible();
 await expect(fleet.getByRole('group',{name:'VEEAM : Inscriptible 1, En transfert 1'})).toBeVisible();
 // No separate breakdown nor list: the chassis is the inventory.
 await expect(fleet.getByRole('heading',{name:'Répartition par état'})).toHaveCount(0);
 await expect(fleet.getByText('Liste des cartouches')).toHaveCount(0);
 // Live activity sits in the main LCD: VEEAM drive writing with its rate, S3 export with progress.
 const activity=fleet.getByRole('region',{name:'Activité en cours'});
 await expect(fleet.locator('.fleet-readout').getByRole('region',{name:'Activité en cours'})).toBeVisible();
 await expect(activity).toContainText('VEEAM · Lecteur 1 · Écriture en cours');
 await expect(activity).toContainText('173 Mio/s');
 await expect(activity.getByRole('progressbar',{name:'Progression de CVT002L8'})).toHaveAttribute('aria-valuenow','25');
 // Every chassis carries its own power and maintenance buttons; maintenance opens here, scoped to that VTL.
 const pbsControls=fleet.getByRole('group',{name:'Commandes de PBS'}),veeamControls=fleet.getByRole('group',{name:'Commandes de VEEAM'});
 for(const controls of [pbsControls,veeamControls]){await expect(controls.getByRole('button',{name:'Alimentation de la bibliothèque'})).toBeEnabled();await expect(controls).toContainText('En service');}
 const maintenanceReads=[];page.on('request',r=>{const p=new URL(r.url()).pathname;if(p.endsWith('/maintenance'))maintenanceReads.push(p);});
 await veeamControls.getByRole('button',{name:'Maintenance',exact:true}).click();
 await expect(page.getByRole('dialog',{name:'Maintenance'})).toBeVisible();
 await expect(page).toHaveURL(/#fleet/);
 await expect.poll(()=>maintenanceReads.some(p=>p===`/v1/vtls/${veeam.id}/maintenance`)).toBe(true);
 await page.keyboard.press('Escape');await expect(page.getByRole('dialog',{name:'Maintenance'})).toHaveCount(0);
 await expect(page).toHaveURL(/#fleet/);
 // Active cartridges blink: drive I/O on the loaded tape, network on the one sent to S3; idle tapes stay dark.
 await expect(fleet.getByRole('button',{name:'Détails de CVT001L8'})).toHaveAttribute('data-activity','io');
 await expect(fleet.getByRole('button',{name:'Détails de CVT002L8'})).toHaveAttribute('data-activity','network');
 await expect(fleet.getByRole('button',{name:'Détails de PBS001L8'})).not.toHaveAttribute('data-activity');
 await expect(fleet.locator('.fleet-faceplate .fleet-io')).toHaveCount(1);
 // A cartridge in the chassis opens its read-only sheet, which leads to its VTL.
 await fleet.getByRole('button',{name:'Détails de CVT002L8'}).click();
 const sheet=page.getByRole('dialog',{name:'Cartouche CVT002L8'});
 await expect(sheet).toContainText('VEEAM');
 await expect(sheet).toContainText('Prête à envoyer');
 await expect(sheet).toContainText('Envoi vers S3');
 await expect(sheet).toContainText('25 %');
 await page.keyboard.press('Escape');await expect(sheet).toHaveCount(0);
 await fleet.getByRole('button',{name:'Détails de CVT001L8'}).click();
 await expect(page.getByRole('dialog',{name:'Cartouche CVT001L8'})).toContainText('Lecteur 1 · Écriture en cours');
 await expect(page.getByRole('dialog',{name:'Cartouche CVT001L8'})).toContainText('512 Gio');
 await page.getByRole('link',{name:'Ouvrir dans VEEAM'}).click();
 // It opens that cartridge in its VTL without filtering the magazine, then drops the barcode from the URL.
 await expect(page.getByRole('dialog',{name:'Cartouche CVT001L8'})).toBeVisible();
 await expect(page).toHaveURL(new RegExp(`#media\\?vtl=${veeam.id}$`));
 await page.keyboard.press('Escape');
 await expect(page.getByRole('textbox',{name:'Rechercher une cartouche'})).toHaveValue('');
 await expect(page.getByRole('button',{name:'Détails de CVT002L8'})).toBeVisible();
 // Tabs belong to the selected VTL; the fleet page stays reachable.
 await page.getByRole('link',{name:'Vue du parc'}).click();await expect(page).toHaveURL(/#fleet/);
 // An unavailable VTL is reported in its row; the others stay visible.
 veeamDown=true;await page.reload();
 await expect(fleet).toContainText('Données indisponibles pour cette VTL.');
 await expect(fleet.getByRole('group',{name:'PBS : Inscriptible 1, Protégée S3 1, Échec 1'})).toBeVisible();
 for(const theme of ['dark','light']){await page.evaluate(theme=>{document.documentElement.classList.toggle('dark',theme==='dark');},theme);
  assert.notEqual(await page.locator('.fleet').evaluate(e=>getComputedStyle(e).getPropertyValue('--fleet-writable').trim()),'');}
 await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'fleet page overflows on mobile');
 assert.deepEqual(errors,[]);
 console.log('PASS fleet: own page, key figures with real usage, rack units with cartridge spines, activity LEDs and their own power/maintenance, activity in the main LCD, cartridge sheet leading to its VTL, unavailable VTL, themes, mobile');
} finally { await browser.close(); await ui.close(); }
