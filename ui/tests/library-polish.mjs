import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1600,height:1000}}),errors=[],writes=[];
 let scenario='writing',jobStatus='queued';
 page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'admin',username:'admin',role:'admin',issuer:null,disabled:false},local:true,oidc:false,csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(r.request().method()!=='GET')writes.push(path);
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Bibliothèque de test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry')){const value=telemetryFixture(demoId,scenario);value.source='mhvtl';value.drives[0].write_rate_bps=270*1024**2;value.slots.push(...Array.from({length:48},(_,i)=>({number:i+10,kind:'storage',position_known:true,barcode:null,observed_at:Date.now()})));return r.fulfill({json:value});}
  if(path.endsWith('/telemetry/history')){const value=emptyThroughputHistory(demoId,r.request().url());value.points=Array.from({length:24},(_,i)=>({at:Date.now()-(23-i)*5000,writeRate:270*1024**2,localGrowth:260*1024**2}));return r.fulfill({json:value});}
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[{id:'export',subject:'CVT002L8',kind:'export',status:jobStatus,progress:0,created_at:new Date().toISOString(),updated_at:new Date().toISOString()}]:path.endsWith('/maintenance')?{configured:false}:path.endsWith('/cartridges/deletion')?{pending:null}:{status:'ok'}});
 });
 const capture=async name=>{if(process.env.COLD_VTL_CAPTURE_DIR)await page.screenshot({path:`${process.env.COLD_VTL_CAPTURE_DIR}/${name}.png`});};
 await page.goto(ui.url+'/#media');
 await expect(page.locator('.lto-console-brand .vaultcenter-mark-dark')).toBeVisible();
 assert.equal(await page.locator('.lto-console-brand img').last().evaluate(e=>e.complete&&e.naturalWidth>0),true);
 await expect.poll(()=>page.getByRole('table',{name:'Historique du débit'}).getByRole('row').count()).toBeGreaterThanOrEqual(25);
 const dimensions=await page.locator('main').evaluate(e=>({height:e.clientHeight,scroll:e.scrollHeight}));
 assert.ok(dimensions.scroll<900,JSON.stringify(dimensions));
 const magazine=page.getByRole('region',{name:'Emplacements des cartouches'});
 assert.ok((await magazine.boundingBox()).height<285,'All slots must stay in a single accessible magazine row');
 await magazine.evaluate(e=>{e.scrollLeft=e.scrollWidth;});assert.ok(await magazine.evaluate(e=>e.scrollLeft>0));await magazine.evaluate(e=>{e.scrollLeft=0;});
 const lcd=page.getByRole('region',{name:'Journal d’activité de la bibliothèque'}),inventory=page.getByRole('region',{name:'Inventaire des cartouches',exact:true});
 const assertAligned=async()=>{const a=await lcd.boundingBox(),b=await inventory.boundingBox();assert.ok(Math.abs(a.y+a.height-b.y-b.height)<=1,'LCD and inventory must share their bottom edge');};
 await assertAligned();await capture('library-desktop');
 await page.getByRole('button',{name:'Changer le thème',exact:true}).click();
 await expect(page.locator('.lto-console-brand .vaultcenter-mark-light')).toBeVisible();
 await expect(page.locator('.lto-console')).toHaveCSS('background-color','rgb(226, 231, 233)');
 await expect(page.locator('.lto-reader-section')).toHaveCSS('background-color','rgb(255, 255, 255)');
 await assertAligned();await capture('library-light');
 await page.reload();await expect(page.locator('.lto-console-brand .vaultcenter-mark-light')).toBeVisible();await expect(magazine).toBeVisible();await assertAligned();
 await page.setViewportSize({width:390,height:844});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);await capture('library-light-mobile');
 await page.setViewportSize({width:1600,height:1000});await page.getByRole('button',{name:'Changer le thème',exact:true}).click();await expect(page.locator('.lto-console-brand .vaultcenter-mark-dark')).toBeVisible();

 await page.getByRole('button',{name:'Tableau',exact:true}).click();
 const first=page.getByRole('img',{name:'Opération en cours sur CVT001L8',exact:true}),ring=first.locator('.lto-media-work-ring');
 await expect(ring).toHaveCSS('animation-name','lto-work-spin');
 const initial=await ring.evaluate(e=>getComputedStyle(e).transform);await page.waitForTimeout(120);assert.notEqual(await ring.evaluate(e=>getComputedStyle(e).transform),initial);
 await expect(page.getByRole('img',{name:'Opération en cours sur CVT002L8',exact:true})).toHaveCount(0);
 await capture('table-desktop');
 await page.emulateMedia({reducedMotion:'reduce'});await expect(ring).toHaveCSS('animation-name','none');await page.emulateMedia({reducedMotion:'no-preference'});
 scenario='reading';await page.waitForTimeout(2200);await expect(ring).toBeVisible();
 scenario='idle';await expect(first).toHaveCount(0,{timeout:6000});
 scenario='stale';jobStatus='running';await expect(page.getByRole('img',{name:'Opération en cours sur CVT002L8',exact:true})).toBeVisible({timeout:15000});await expect(first).toHaveCount(0);
 jobStatus='completed';await expect(page.locator('.lto-media-work-ring')).toHaveCount(0,{timeout:15000});
 await page.setViewportSize({width:390,height:844});await expect(page.locator('.lto-console-brand .vaultcenter-mark-dark')).toBeVisible();assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);await capture('table-mobile');
 await page.getByRole('button',{name:'Bibliothèque',exact:true}).click();await expect(magazine).toBeAttached();assert.ok((await magazine.boundingBox()).height<285);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);await capture('library-mobile');
 assert.deepEqual(errors,[]);assert.deepEqual(writes,[]);console.log('PASS library polish: logo, bounded magazine and accessible chart history, active tape ring, idle/stale/queued/completed, running export, reduced motion and mobile.');
}finally{await browser.close();await ui.close();}
