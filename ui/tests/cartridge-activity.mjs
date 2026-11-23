import {selectOption} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try{
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}}),errors=[],requests=[],storageRequests=[];
 const latest=Date.now()-3*86400000;
 let unavailable=false;
 const job={id:'past-export',kind:'export',subject:'CVT004L8',status:'completed',generation:2,progress:100,message:JSON.stringify({logical_size_bytes:1048576}),created_at:new Date(latest-3600000).toISOString(),updated_at:new Date(latest).toISOString()};
 page.on('pageerror',error=>errors.push(error.message));
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const url=new URL(r.request().url()),path=url.pathname,q=url.searchParams;
  if(path.includes('/storage/') || path.endsWith('/generations')) storageRequests.push(path);
  assert.equal(r.request().method(),'GET');
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VTL test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry/history')){
   requests.push(Object.fromEntries(q));
   if(unavailable)return r.fulfill({status:503,json:{error:'History unavailable'}});
   const h=emptyThroughputHistory(demoId,r.request().url());
   h.end_ms=q.has('end_ms')?Number(q.get('end_ms')):Date.now();h.start_ms=h.end_ms-Number(q.get('window_seconds'))*1000;
   const s3=q.get('kind')==='s3';h.status='available';
   h.series=[{barcode:'CVT004L8',drive_id:s3?'s3':'21',last_at:latest,...(s3?{transfer_id:'past-export'}:{})},{barcode:'CVT005L8',drive_id:'21',last_at:Date.now()}].filter(s=>s.last_at<=h.end_ms);
   h.points=q.get('barcode')==='CVT004L8'?[{at:latest-120000,writeRate:(s3?21:12)*1024**2,localGrowth:null},{at:latest-60000,writeRate:null,localGrowth:null},{at:latest,writeRate:0,localGrowth:null},{at:latest-86400000,writeRate:7*1024**2,localGrowth:null}].filter(p=>p.at>=h.start_ms&&p.at<=h.end_ms):[];
   return r.fulfill({json:h});
  }
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  if(path.endsWith('/storage/buckets'))return r.fulfill({status:503,json:{error:'Buckets indisponibles'}});
  if(path.endsWith('/jobs'))return r.fulfill({json:q.get('barcode')==='CVT004L8'&&q.get('kind')==='export'?[job]:[{...job,id:'other-tape',subject:'CVT005L8'}]});
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#media');
 await expect(page.getByText('Emplacements observés',{exact:true})).toHaveCount(0);
 // Isolate the details dialog: the library now has its own S3 destination shelf.
 await page.getByRole('button',{name:'Tableau',exact:true}).click();
 await expect(page.getByRole('region',{name:'Stockage S3 — externalisation'})).toHaveCount(0);
 storageRequests.length=0;
 await page.getByRole('button',{name:'Détails de CVT004L8',exact:true}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog.getByRole('heading',{name:'Cartouche CVT004L8',exact:true})).toBeVisible();
 await expect(dialog.getByRole('group',{name:'Actions sur la copie'})).toHaveCount(0);
 for(const name of ['Restaurer','Copies S3','Lancer la restauration','Rechercher les copies existantes']) {
  await expect(dialog.getByRole('button',{name,exact:true,includeHidden:true})).toHaveCount(0);
 }
 await expect(dialog.getByRole('button',{name:'Envoyer vers S3',exact:true})).toBeDisabled();
 await dialog.getByText('Historique des envois',{exact:true}).click();
 const chart=dialog.getByRole('figure');
 await expect(chart).toContainText('12 Mio/s');
 await expect(chart.getByRole('button',{name:'24 h',exact:true})).toHaveAttribute('aria-pressed','true');
 assert.ok(requests.some(q=>q.barcode==='CVT004L8'&&Number(q.end_ms)===latest+1000),'Opening an old tape centers its last recorded measurements');
 await chart.getByRole('combobox',{name:'Copie du graphique'}).click();await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(1);await page.keyboard.press('Escape');
 await expect(dialog.getByRole('combobox',{name:'Bucket',exact:true})).toHaveCount(0);
 await expect(dialog.getByRole('region',{name:'Exports S3 de la cartouche'}).getByRole('listitem')).toHaveCount(1);
 await chart.getByRole('button',{name:'Période précédente'}).click();
 await expect(chart).toContainText('7 Mio/s');
 await chart.getByRole('button',{name:'Dernières mesures'}).click();
 await expect(chart).toContainText('12 Mio/s');
 await chart.getByRole('button',{name:'Période précédente'}).click();
 await chart.getByRole('button',{name:'Période suivante'}).click();
 await expect(chart).toContainText('12 Mio/s');
 await chart.getByRole('button',{name:'Revenir à maintenant'}).click();
 await expect(chart.getByText('Aucune mesure enregistrée sur cette période',{exact:true})).toBeVisible();
 await chart.getByRole('button',{name:'Dernières mesures'}).click();
 await expect(chart).toContainText('12 Mio/s');
 await dialog.getByRole('button',{name:/Graphique de l’export du/}).click();
 await expect(chart.getByRole('combobox',{name:'Type de transfert du graphique'})).toHaveAttribute('data-value', 's3');
 await expect(chart).toContainText('21 Mio/s');
 assert.ok(requests.some(q=>q.kind==='s3'&&q.barcode==='CVT004L8'&&q.transfer_id==='past-export'));
 await selectOption(chart.getByRole('combobox',{name:'Type de transfert du graphique'}), 'pbs');
 await expect(chart).toContainText('12 Mio/s');
 assert.ok(requests.some(q=>q.kind==='pbs'&&q.barcode==='CVT004L8'&&!q.transfer_id));
 await dialog.getByRole('button',{name:/Graphique de l’export du/}).click();
 await expect(chart).toContainText('21 Mio/s');
 for(const [width,theme] of [[1440,'dark'],[1440,'light'],[390,'light']]){
  await page.setViewportSize({width,height:1000});
  await page.evaluate(theme=>document.documentElement.classList.toggle('dark',theme==='dark'),theme);
  await expect(chart.locator('svg.recharts-surface')).toBeVisible();
  assert.equal(await dialog.evaluate(e=>e.scrollWidth<=e.clientWidth),true);
  if(process.env.COLD_VTL_CAPTURE_DIR)await page.screenshot({path:`${process.env.COLD_VTL_CAPTURE_DIR}/activity-${width}-${theme}.png`,fullPage:true});
 }
 unavailable=true;
 await chart.getByRole('button',{name:'7 jours',exact:true}).click();
 await expect(chart.getByText('Historique indisponible',{exact:true})).toBeVisible();
 assert.deepEqual(errors,[]);
 assert.deepEqual(storageRequests,[],'Cartridge details must not load S3 buckets or generations');
 console.log('PASS cartridge details: no restore/copy tabs or S3 catalogue requests, send permission, past PBS/S3 curves, scoped export jobs, error state, themes/mobile.');
}finally{await browser.close();await ui.close();}
