import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
import {storageGauge} from '../src/lib/storage-gauge.ts';
const pbs='00000000-0000-4000-8000-0000000000aa';
const TiB=1024**4,GiB=1024**3;
const budget=(total,free,reserved)=>({total_bytes:total,free_bytes:free,reserved_bytes:reserved,available_bytes:Math.max(0,free-reserved),overhead_per_cartridge:16*1024**2});

// Parts of the volume: what is written, what cartridges may still write, what can be provisioned.
assert.deepEqual(storageGauge(budget(5*TiB,3*TiB,1*TiB)),{total:5*TiB,occupied:2*TiB,reserved:1*TiB,available:2*TiB,shortfall:0,level:'ok'});
assert.equal(storageGauge(budget(10*TiB,2*TiB,1.2*TiB)).level,'low');
assert.deepEqual(storageGauge(budget(4*TiB,1*TiB,1.5*TiB)),{total:4*TiB,occupied:3*TiB,reserved:1*TiB,available:0,shortfall:0.5*TiB,level:'empty'});
for(const bad of [null,'x',{},{storage:null},budget(0,0,0),budget(-1,0,0),budget(TiB,2*TiB,0),budget(TiB,TiB/2,-1),{...budget(TiB,TiB/2,0),total_bytes:'1'},{...budget(TiB,TiB/2,0),free_bytes:Number.NaN}])
 assert.equal(storageGauge(bad),null,JSON.stringify(bad));

const ui=await staticUi();let browser;
const volumes={[demoId]:budget(5*TiB,2.9*TiB,1.7*TiB),[pbs]:budget(10*TiB,1*TiB,0.2*TiB)};
let cartridges=[],optionCalls=0,failing=false;
async function routes(page){
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer',issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[
   {id:demoId,name:'Veeam',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-b',prefix:'vtl'}},
   {id:pbs,name:'PBS',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-p',prefix:'vtl'}},
  ]});
  const options=path.match(/^\/v1\/vtls\/([^/]+)\/cartridges\/options$/);
  if(options){optionCalls++;return failing?route.fulfill({status:500,json:{error:'Contrôleur indisponible'}}):route.fulfill({json:{configured:true,ready:true,storage:volumes[options[1]]??null}});}
  if(path==='/v1/activity')return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  if(path.endsWith('/cartridges'))return route.fulfill({json:cartridges});
  if(path.endsWith('/jobs'))return route.fulfill({json:[]});
  return route.fulfill({json:{status:'ok',cartridges:cartridges.length,jobs:0,media_root:'/fixture'}});
 });
}
const part=(gauge,name)=>gauge.locator(`[data-part=${name}]`).evaluate(element=>element.style.width);
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:900}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 await routes(page);
 await page.clock.install();
 await page.goto(`${ui.url}/#media?vtl=${demoId}`);

 // In the status bar, between the drives and the connection: what can still be provisioned, readable by viewers.
 const nav=page.getByRole('region',{name:'État des lecteurs'});
 await expect(page.getByRole('navigation',{name:'Navigation de la bibliothèque'}).getByRole('button',{name:/^Stockage local/})).toHaveCount(0);
 const gauge=nav.getByRole('button',{name:'Stockage local : 1,2 Tio libres à provisionner',exact:true});
 await expect(gauge).toBeVisible();
 await expect(gauge).toContainText('1,2 Tio à provisionner sur 5 Tio');
 await expect(gauge).toHaveAttribute('data-level','ok');
 assert.deepEqual([await part(gauge,'occupied'),await part(gauge,'reserved'),await part(gauge,'available')],['42%','34%','24%']);
 await gauge.click();
 const panel=page.getByRole('dialog',{name:'Stockage local',exact:true});
 const value=label=>panel.locator('dt',{hasText:label}).locator('xpath=following-sibling::dd[1]');
 await expect(value('Occupé')).toHaveText('2,1 Tio');
 await expect(value('Réservé aux cartouches')).toHaveText('1,7 Tio');
 await expect(value('Libre à provisionner')).toHaveText('1,2 Tio');
 await expect(value('Volume total')).toHaveText('5 Tio');
 await expect(panel).toContainText('Les VTL qui partagent ce volume sont comptées ensemble.');
 await expect(panel.getByRole('alert')).toHaveCount(0);
 await page.screenshot({path:'test-results/header-storage-desktop.png'});
 await page.keyboard.press('Escape');
 await expect(panel).toHaveCount(0);
 await expect(gauge).toBeFocused();

 // A new cartridge in the library refreshes the gauge; so does the minute timer.
 let before=optionCalls;
 cartridges=[{barcode:'CVT001L8',capacity_bytes:TiB,bytes_written:0,generation:0,state:'writable'}];
 await page.clock.runFor(16000);
 await expect.poll(()=>optionCalls).toBeGreaterThan(before);
 before=optionCalls;await page.clock.runFor(61000);
 await expect.poll(()=>optionCalls).toBeGreaterThan(before);

 // Another VTL shows its own volume: little left to provision.
 await page.getByLabel('Bibliothèque VTL',{exact:true}).click();
 await page.getByRole('option',{name:'PBS'}).click();
 const low=nav.getByRole('button',{name:/^Stockage local : /});
 await expect(low).toHaveAccessibleName('Stockage local : 819,2 Gio libres à provisionner');
 await expect(low).toHaveAttribute('data-level','low');

 // Reservations above the free space: nothing left and a warning.
 volumes[pbs]=budget(4*TiB,1*TiB,1.3*TiB);await page.reload();
 await expect(low).toHaveAccessibleName('Stockage local : 0 o libres à provisionner');
 await expect(low).toHaveAttribute('data-level','empty');
 await low.click();
 await expect(panel.getByRole('alert')).toHaveText('Les cartouches existantes ne pourront pas toutes se remplir : il manque 307,2 Gio.');
 await expect(value('Réservé aux cartouches')).toHaveText('1 Tio');
 await page.keyboard.press('Escape');

 // No figures, an error or an unexpected answer: no gauge, nothing breaks.
 volumes[pbs]=null;await page.reload();
 await expect(page.getByRole('button',{name:'Menu du compte'})).toBeVisible();
 await page.clock.runFor(2000);
 await expect(low).toHaveCount(0);
 volumes[pbs]={total_bytes:'5'};await page.reload();
 await page.clock.runFor(2000);
 await expect(low).toHaveCount(0);
 failing=true;await page.reload();
 await page.clock.runFor(2000);
 await expect(low).toHaveCount(0);
 failing=false;volumes[pbs]=budget(10*TiB,1*TiB,0.2*TiB);
 await page.clock.runFor(61000);
 await expect(low).toBeVisible();

 // Phone width: the bar stays in the status bar, the page does not scroll sideways.
 await page.setViewportSize({width:390,height:844});await page.reload();
 await expect(low).toBeVisible();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await low.click();
 const box=await panel.boundingBox();
 assert.ok(box.x>=0&&box.x+box.width<=390,`panel ${box.x}+${box.width} must fit 390px`);
 await page.screenshot({path:'test-results/header-storage-mobile.png'});
 assert.deepEqual(errors,[]);

 // English labels.
 const english=await browser.newPage({locale:'en-US',viewport:{width:1440,height:900}});
 english.on('pageerror',error=>errors.push(error.message));
 await routes(english);
 await english.goto(`${ui.url}/#media?vtl=${demoId}`);
 const englishGauge=english.getByRole('button',{name:'Local storage: 1.2 TiB free to provision',exact:true});
 await expect(englishGauge).toContainText('1.2 TiB to provision of 5 TiB');
 await englishGauge.click();
 const englishPanel=english.getByRole('dialog',{name:'Local storage',exact:true});
 await expect(englishPanel).toContainText('Reserved for cartridges');
 await expect(englishPanel).toContainText('Free to provision');
 assert.deepEqual(errors,[]);
 console.log('PASS header storage gauge: parts, details, refresh on library change and every minute, per VTL, low, over-reserved, hidden without figures, mobile, English');
} finally {await browser?.close();await ui.close();}
