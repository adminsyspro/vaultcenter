import {selectOption} from './helpers/select.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {appendRateHistory,mergeRateHistory} from '../src/lib/telemetry.ts';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
const separated=mergeRateHistory([{at:10000,writeRate:1,localGrowth:null},{at:15000,writeRate:null,localGrowth:null}], [{at:10000,writeRate:1,localGrowth:null},{at:40000,writeRate:2,localGrowth:null}]);
assert.equal(separated[1].writeRate,null,'A live fetch outage must not become a continuous curve');
assert.equal(separated.at(-1).writeRate,2);
// Server measurements must fill a browser polling pause, not disappear behind it.
const measured=at=>({at,writeRate:270,localGrowth:260});
const persisted=Array.from({length:9},(_,index)=>measured(index*5000));
assert.deepEqual(mergeRateHistory(persisted,[measured(0),measured(40000)]),persisted);
const interrupted=persisted.map(point=>point.at===20000?{...point,writeRate:null,localGrowth:null}:point);
assert.deepEqual(mergeRateHistory(interrupted,[measured(0),measured(40000)]),interrupted,'Real server gaps must survive a browser pause');
const missingBoth=mergeRateHistory([], [measured(0),measured(40000)]);
assert.equal(missingBoth[1].writeRate,null,'Do not interpolate when neither source measured the interval');
const dense=[measured(1000),{at:3000,writeRate:null,localGrowth:0},measured(5000)];
assert.deepEqual(mergeRateHistory([{at:2000,writeRate:null,localGrowth:null}],dense),dense,'Continuous live sampling supersedes sparse server buckets and preserves independent metrics');
assert.deepEqual(mergeRateHistory(persisted,[]),persisted);
assert.deepEqual(mergeRateHistory(persisted,[{at:40000,writeRate:0,localGrowth:0}]).at(-1),{at:40000,writeRate:0,localGrowth:0},'Zero is a measurement, not a gap');
assert.deepEqual(mergeRateHistory(persisted,[measured(0)]),persisted,'Keep server measurements newer than the last browser poll');
let samples={};
for(let i=0;i<80;i++){const value=telemetryFixture(demoId,'writing',100000+i*2000);value.drives[0].write_rate_bps=i;samples=appendRateHistory(samples,value);}
assert.equal(samples['11'].points.length,60);
const switched=telemetryFixture(demoId,'writing',270000);switched.drives[0].barcode='CVT002L8';switched.drives[0].write_rate_bps=-5;
samples=appendRateHistory(samples,switched);
assert.equal(samples['11'].points.length,1);assert.equal(samples['11'].points[0].writeRate,null);
assert.deepEqual(appendRateHistory(samples,{...switched,drives:[]}),{});
const ui=await staticUi();let browser;
try{
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});let rate=12*1024**2;let physical=false;let scenario='writing';let persistent=false;const windows=[];const recordedAt=Date.now();
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer',issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry/history')){
   const result=emptyThroughputHistory(demoId,r.request().url());
   const query=new URL(r.request().url()).searchParams;windows.push(Number(query.get('window_seconds')));
   if(persistent){result.status='available';result.series=[{drive_id:'11',barcode:'CVT001L8',last_at:recordedAt}];result.points=query.get('barcode')||query.get('aggregate')?[
    {at:recordedAt-20*86400000,writeRate:7*1024**2,localGrowth:null},
    {at:recordedAt-2700000,writeRate:9*1024**2,localGrowth:null},
    {at:recordedAt-1800000,writeRate:null,localGrowth:null},
    {at:recordedAt-300000,writeRate:15*1024**2,localGrowth:null},
   ].filter(p=>p.at>=result.start_ms):[];}
   if(persistent&&query.get('kind')==='s3'){
    assert.notEqual(query.get('drive_id'),'11','PBS drive scope must not leak into S3 history');
    result.source='demo';result.series=[{drive_id:'s3',barcode:'CVT001L8',transfer_id:'past-transfer',last_at:recordedAt},{drive_id:'s3',barcode:'CVT002L8',transfer_id:'second-transfer',last_at:recordedAt-1000}]
     .filter(s=>(!query.has('barcode')||s.barcode===query.get('barcode'))&&(!query.has('transfer_id')||s.transfer_id===query.get('transfer_id')));
    result.points=query.get('aggregate')?[{at:recordedAt-600000,writeRate:63*1024**2,localGrowth:null}]:query.get('barcode')?[{at:recordedAt-600000,writeRate:(query.get('barcode')==='CVT002L8'?42:21)*1024**2,localGrowth:null}]:[];
   }
   return r.fulfill({json:result});
  }
  if(path.endsWith('/telemetry')){const value=telemetryFixture(demoId,scenario);value.drives[0].write_rate_bps=physical?null:rate;return r.fulfill({json:value});}
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[]:{status:'ok',cartridges:9,jobs:0,media_root:'/demo'}});
 });
 await page.goto(ui.url+'/#media');
 const chart=page.getByRole('figure',{name:'Débit de copie de CVT001L8',exact:true});
 await expect(chart).toBeVisible();await expect(chart).toContainText('12 Mio/s');
 const drive=page.locator('.lto-drive');
 const cartridge=drive.getByRole('article',{name:'Cartouche CVT001L8'});
 const body=drive.locator('.lto-drive-body');
 const reel=cartridge.locator('.lto-reel-spin');
 await expect(reel).toHaveCSS('animation-name','lto-reel-turn');
 const initialRotation=await reel.evaluate(node=>getComputedStyle(node).transform);
 await page.waitForTimeout(120);
 assert.notEqual(await reel.evaluate(node=>getComputedStyle(node).transform),initialRotation,'The reel must actually rotate');
 const cassetteBounds=await cartridge.boundingBox();const bodyBounds=await body.boundingBox();
 assert.ok(bodyBounds.height<=cassetteBounds.height+1,'Reader content must fit the cartridge height');
 assert.ok(bodyBounds.y>=cassetteBounds.y-1&&bodyBounds.y+bodyBounds.height<=cassetteBounds.y+cassetteBounds.height+1,'Reader content must align within cartridge bounds');
 await page.emulateMedia({reducedMotion:'reduce'});await expect(reel).toHaveCSS('animation-name','none');
 await page.emulateMedia({reducedMotion:'no-preference'});
 rate=18*1024**2;await expect(chart).toContainText('18 Mio/s',{timeout:8000});
 await expect(chart.getByRole('table',{name:'Historique du débit'}).getByRole('row')).toHaveCount(3);
 rate=null;await expect(chart).toContainText('Débit indisponible',{timeout:8000});
 scenario='idle';await expect(reel).toHaveCSS('animation-name','none',{timeout:8000});
 scenario='writing';await expect(reel).toHaveCSS('animation-name','lto-reel-turn',{timeout:8000});
 scenario='stale';await expect(page.locator('.lto-cartridge[data-running="true"]')).toHaveCount(0,{timeout:8000});
 scenario='writing';
 // A different source is a different series; physical growth is never labelled copy throughput.
 physical=true;await page.reload();
 const growth=page.getByRole('figure',{name:'Croissance du volume local de CVT001L8',exact:true});
 await expect(growth).toBeVisible();await expect(growth).toContainText('1 Mio/s');
 await expect(page.getByRole('figure',{name:'Débit de copie de CVT001L8',exact:true})).toHaveCount(0);
 // Persisted data is served independently of the live collector and survives reload.
 persistent=true;physical=false;scenario='empty';await page.reload();
 // With no tape in the drive the panel opens on the sum of every tape.
 const history=page.getByRole('figure',{name:'Débit de copie de toutes les cartouches',exact:true});
 // Choose a historical range even before the inventory resolves.
 await page.getByRole('button',{name:'1 h',exact:true}).click();
 await expect(history).toBeVisible();await expect(history).toContainText('9 Mio/s');
 await expect(history).toContainText('Indisponible');assert.ok(windows.includes(3600));
 await page.reload();
 await expect(page.getByRole('button',{name:'1 h',exact:true})).toHaveAttribute('aria-pressed','true');
 await expect(history).toContainText('9 Mio/s');
 await history.getByRole('button',{name:'30 jours',exact:true}).click();
 await expect(history).toContainText('7 Mio/s');assert.ok(windows.includes(2592000));
 // The direction selector shows its whole label, whatever the option.
 const directionText=async trigger=>trigger.evaluate(e=>{const label=e.querySelector('span');return {fits:label.scrollWidth<=label.clientWidth+1,text:e.textContent.trim()};});
 { const trigger=history.getByRole('combobox',{name:'Type de transfert du graphique'});const shown=await directionText(trigger);
   assert.equal(shown.text,'Backup Client → VAULTCENTER');assert.ok(shown.fits,'direction label clipped: '+shown.text); }
 await selectOption(history.getByRole('combobox',{name:'Type de transfert du graphique'}), 's3');
 // Changing the transfer kind applies the default again: every S3 send summed.
 const s3All=page.getByRole('figure',{name:'Débit vers S3 de toutes les cartouches',exact:true});
 { const shown=await directionText(s3All.getByRole('combobox',{name:'Type de transfert du graphique'}));
   assert.equal(shown.text,'VAULTCENTER → S3');assert.ok(shown.fits,'direction label clipped: '+shown.text); }
 await expect(s3All).toContainText('63 Mio/s');
 const s3History=page.getByRole('figure',{name:'Débit vers S3 de CVT001L8',exact:true});
 // Selecting one export must never filter the inventory down to that export.
 const selector=()=>page.getByRole('combobox',{name:'Cartouche du graphique'});
 await selector().click();
 await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(3);
 await expect(page.getByRole('listbox').getByRole('option').first()).toHaveText('Toutes les cartouches');
 await page.keyboard.press('Escape');
 await selectOption(selector(),JSON.stringify({barcode:'CVT001L8',drive_id:'s3',transfer_id:'past-transfer'}));
 await expect(s3History).toContainText('21 Mio/s');
 await selectOption(selector(),JSON.stringify({barcode:'CVT002L8',drive_id:'s3',transfer_id:'second-transfer'}));
 await expect(page.getByRole('figure',{name:'Débit vers S3 de CVT002L8',exact:true})).toContainText('42 Mio/s');
 await selector().click();
 await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(3);
 await page.keyboard.press('Escape');
 await selectOption(selector(),JSON.stringify({barcode:'CVT001L8',drive_id:'s3',transfer_id:'past-transfer'}));
 await expect(s3History).toContainText('21 Mio/s');
 await expect(page.getByRole('region',{name:'Externalisation S3 simulée'})).toHaveCount(0);
 await selectOption(s3History.getByRole('combobox',{name:'Type de transfert du graphique'}), 'pbs');
 await history.getByRole('button',{name:'Instantané',exact:true}).click();
 await expect(page.getByRole('button',{name:'Instantané',exact:true})).toHaveAttribute('aria-pressed','true');
 // Simulate a hidden tab while the server records a distinct measurement and a real gap.
 persistent=false;physical=false;scenario='writing';rate=18*1024**2;
 await page.reload();await expect(chart).toContainText('18 Mio/s');
 const pauseAt=Date.now();
 await page.route('**/telemetry/history?*',r=>{
  const result=emptyThroughputHistory(demoId,r.request().url());
  result.status='available';result.series=[{drive_id:'11',barcode:'CVT001L8',last_at:pauseAt+10000}];
  result.points=[{at:pauseAt+5000,writeRate:73*1024**2,localGrowth:null},{at:pauseAt+10000,writeRate:null,localGrowth:null}].filter(p=>p.at<=Date.now());
  return r.fulfill({json:result});
 });
 await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});document.dispatchEvent(new Event('visibilitychange'));});
 await page.waitForTimeout(11000);
 await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>false});document.dispatchEvent(new Event('visibilitychange'));});
 await expect(chart).toContainText('73 Mio/s',{timeout:10000});
 await expect(chart).toContainText('Indisponible');
 await page.setViewportSize({width:390,height:844});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
 console.log('PASS throughput graph: hidden-tab resume preserves server measurements and genuine gaps, time history, measured values, missing counter, explicit local-growth fallback, compact height, active/idle/stale reel, reduced motion, mobile');
}finally{await browser?.close();await ui.close();}
