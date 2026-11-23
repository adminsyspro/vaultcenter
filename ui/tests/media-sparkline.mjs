import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}}),errors=[],seen=[];page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'admin',username:'admin',role:'admin'},csrf_token:'csrf'}}));
 await page.route('**/v1/**',r=>{
  const url=new URL(r.request().url()),path=url.pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VTL principale',lifecycle:'ready',provisioning_revision:2}]});
  assert.ok(path.startsWith(`/v1/vtls/${demoId}/`));
  let value=[];
  if(path.endsWith("/status"))value={status:"ok",cartridges:9,jobs:0};
  if(path.endsWith("/maintenance"))value={configured:true,phase:"online",ready:false,blocked:false,services:{},original:{}};
  if(path.endsWith('/cartridges'))value=demoMedia();
  if(path.endsWith('/telemetry')){value=telemetryFixture();value.drives[0].write_rate_bps=2*1024**2;}
  if(path.endsWith('/telemetry/history')){value=emptyThroughputHistory(demoId,url.href);const barcode=url.searchParams.get('barcode');seen.push(barcode);if(barcode==='CVT001L8'){const now=Date.now();value.status='available';value.points=[{at:now-10000,writeRate:1024**2,localGrowth:null},{at:now-8000,writeRate:null,localGrowth:null},{at:now-6000,writeRate:0,localGrowth:null},{at:now-4000,writeRate:2*1024**2,localGrowth:null}];}}
  return r.fulfill({json:value});
 });
 await page.goto(ui.url+'/#media');await page.getByRole('button',{name:'Tableau',exact:true}).click();
 const heads=await page.locator('thead th').allTextContents();assert.ok(heads.findIndex(x=>x.startsWith('Débit'))===heads.indexOf('Utilisation')+1);assert.ok(heads.at(-1)==='Actions');
 const graph=page.getByRole('figure',{name:'Débit Backup Client → VAULTCENTER de CVT001L8',exact:true});await expect(graph.locator('svg')).toBeVisible();await expect(graph.locator('circle')).toHaveCount(1);await expect(graph).toContainText('2 Mio/s');
 await expect(page.getByRole('figure',{name:'Débit Backup Client → VAULTCENTER de CVT002L8',exact:true})).toContainText('Aucune mesure');
 assert.ok(seen.includes('CVT001L8')&&seen.includes('CVT002L8'));
 await page.screenshot({path:'../.local/bandwidth-20261005/ui/table-sparklines.png',fullPage:true});await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 assert.deepEqual(errors,[]);console.log('Media sparklines: column order, scoped real history, gaps, zero rates, live units, empty state and mobile passed');
}finally{await browser.close();await ui.close()}
