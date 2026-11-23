import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
const server=await staticUi(); let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
 let scenario='writing'; let calls=0; let pending=0; let maxPending=0; let delay=0;
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/auth/**',r=>r.fulfill({json:new URL(r.request().url()).pathname==='/auth/config'?{local:true,oidc:false}:{user:{id:'demo',username:'demo',role:'viewer',issuer:null,disabled:false},csrf_token:'demo'}}));
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Bibliothèque test',lifecycle:'ready',provisioning_revision:1}]});
  // The header bell lists every VTL, so its route is the one global read.
  if(path==='/v1/activity')return r.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  assert.equal(r.request().method(),'GET');
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/maintenance'))return r.fulfill({json:{configured:false}});
  if(path.endsWith('/storage/buckets'))return r.fulfill({json:[]});
  // The header storage gauge stays hidden without figures.
  if(path.endsWith('/cartridges/options'))return r.fulfill({json:{storage:null}});
  if(path.endsWith('/telemetry')) {calls++;pending++;maxPending=Math.max(maxPending,pending);await new Promise(resolve=>setTimeout(resolve,delay));pending--;const value=telemetryFixture(demoId,scenario==='idle-empty'?'idle':scenario);if(scenario==='idle-empty'){value.drives[0].barcode=null;value.drives[0].origin_slot=null;}return r.fulfill({json:value});}
  const value=path.endsWith('/cartridges')?demoMedia().map((m,i)=>i===0?{...m,state:'writable',bytes_written:0}:i===4?{...m,capacity_bytes:0}:m):path.endsWith('/jobs')?[]:path.endsWith('/status')?{status:'ok',cartridges:9,jobs:0,media_root:'/demo'}:null;
  assert.ok(value,`Unexpected ${path}`);return r.fulfill({json:value});
 });
 await page.goto(server.url+'/#media');
 await expect(page.getByRole('button',{name:'Bibliothèque',exact:true})).toHaveAttribute('aria-pressed','true');
 await expect(page.locator('.lto-drive').getByText('Écriture en cours',{exact:true}).first()).toBeVisible();
 await expect(page.getByRole('button',{name:'Détails de CVT001L8',exact:true})).toHaveCount(1);
 await expect(page.getByText('Démonstration locale',{exact:true})).toBeVisible();
 await expect(page.getByText('Débit indisponible',{exact:true})).toBeVisible();
 await expect(page.getByRole('term').filter({hasText:'Croissance du volume local'})).toBeVisible();
 const liveGauge=page.locator('.lto-cartridge .lto-gauge');
 await expect(liveGauge).toHaveAttribute('aria-label','Occupation CVT001L8 · Occupation locale estimée');
 assert.ok(Number(await liveGauge.getAttribute('aria-valuenow'))>40,'The loaded tape must use live bytes when its catalogue is still zero');
 const inventory=page.locator('.lto-cartridge-spine');
 await expect(inventory.getByRole('meter',{name:'Occupation CVT002L8 · Dernier relevé du catalogue',exact:true})).toHaveAttribute('aria-valuenow','29.296875');
 await expect(inventory.getByRole('img',{name:/Occupation CVT005L8.*inconnue/})).toBeVisible();
 await expect(inventory.getByRole('meter',{name:'Occupation CVT004L8 · Occupation locale estimée',exact:true})).toHaveAttribute('aria-valuenow','0');
 await page.screenshot({path:'../.local/media-occupancy-20261005/local.png',fullPage:true});
 await page.getByRole('checkbox',{name:'Sélectionner CVT003L8',exact:true}).check();
 await expect(page.getByRole('button',{name:'Envoyer vers S3 (1)',exact:true})).toBeDisabled();
 await page.getByRole('button',{name:'Tableau',exact:true}).click();
 await expect(page.getByRole('checkbox',{name:'Sélectionner CVT003L8',exact:true})).toBeChecked();
 await expect(page.getByRole('table')).toBeVisible();
 await expect(page.getByRole('row').filter({hasText:'CVT001L8'})).toContainText('Occupation locale estimée');
 await page.getByRole('button',{name:'Bibliothèque',exact:true}).click();
 scenario='idle-empty';await expect(page.locator('.lto-drive-empty')).toContainText('Lecteur vide',{timeout:6000});
 await expect(page.locator('.lto-drive-heading h3')).toHaveText('Lecteur vide');
 scenario='idle';await expect(page.locator('.lto-drive-heading h3')).toHaveText('Au repos',{timeout:6000});
 await expect(page.locator('.lto-drive .lto-cartridge')).toBeVisible();
 scenario='reading';await expect(page.locator('.lto-drive').getByText('Lecture en cours',{exact:true}).first()).toBeVisible({timeout:6000});
 scenario='stale';await expect(page.getByText('Relevé ancien · activité non confirmée',{exact:true})).toBeVisible({timeout:6000});
 await expect(page.locator('.lto-drive[data-active="true"]')).toHaveCount(0);
 await expect(page.getByRole('meter',{name:'Occupation CVT001L8 · Dernier relevé du catalogue',exact:true})).toHaveAttribute('aria-valuenow','0');
 scenario='missing';await expect(page.getByText('Suivi du lecteur indisponible',{exact:true})).toBeVisible({timeout:6000});
 await expect(page.getByRole('button',{name:'Détails de CVT001L8',exact:true})).toHaveCount(1);
 scenario='unknown';await expect(page.locator('.lto-drive-empty')).toContainText('Cartouche non identifiée',{timeout:6000});await expect(page.getByRole('region',{name:'Inventaire des cartouches',exact:true}).getByText('Positions non connues',{exact:true})).toBeVisible({timeout:6000});
 await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});document.dispatchEvent(new Event('visibilitychange'));});
 await page.waitForTimeout(500);const hiddenCalls=calls;await page.waitForTimeout(4500);assert.equal(calls,hiddenCalls);
 await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>false});document.dispatchEvent(new Event('visibilitychange'));});
 delay=2600;await page.waitForTimeout(6500);assert.equal(maxPending,1);
 // Locally age the last data while a request remains unresolved.
 delay=30000; await page.waitForTimeout(18000);
 await expect(page.locator('.lto-drive[data-active="true"]')).toHaveCount(0);
 for(const dark of [false,true]) {
  await page.evaluate(dark=>document.documentElement.classList.toggle('dark',dark),dark);
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
 }
 assert.ok(calls>2); assert.deepEqual(errors,[]);
 console.log('PASS telemetry: writing/reading, honest metrics, source loss, expiry, selection, viewer, no overlapping polling and mobile');
} finally {await browser?.close();await server.close();}
