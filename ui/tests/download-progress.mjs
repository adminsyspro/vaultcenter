import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {selectOption} from './helpers/select.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
const screenshots='../.local/s3-download-20261008/screenshots';await mkdir(screenshots,{recursive:true});
try{
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}}),errors=[],queries=[];
 page.on('pageerror',e=>errors.push(e.message));
 const job={id:'download-test',kind:'recovery',subject:'CVT007L8',generation:1,status:'running',progress:10,created_at:new Date().toISOString(),updated_at:new Date().toISOString(),message:'Downloading selected remote copy; PBS remains available'};
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const url=new URL(r.request().url()),path=url.pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry/history')){
   const data=emptyThroughputHistory(demoId,url.href);queries.push(url.searchParams.get('kind'));
   if(url.searchParams.get('kind')==='s3_restore'){
    data.source='s3_restore';data.status='available';
    data.series=[{drive_id:'s3_restore',barcode:'CVT007L8',last_at:Date.now(),transfer_id:'download-test'}];
    if(url.searchParams.get('barcode'))data.points=[{at:Date.now()-8000,writeRate:104857600,localGrowth:null},{at:Date.now()-3000,writeRate:125829120,localGrowth:null}];
   }
   return r.fulfill({json:data});
  }
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[job]:path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#jobs');
 const row=page.getByRole('row',{name:'Détails du job download-test'});
 await expect(row).toContainText('Récupération depuis S3');await expect(row).toContainText('Téléchargement et vérification depuis S3 en cours.');await expect(row).not.toContainText('10 %');
 for(const [phase,done,total,label,percent]of [['downloading',256,1024,'Téléchargement depuis S3',25],['verifying',1024,1024,'Vérification des données téléchargées',100],['installing',0,null,'Installation dans la bibliothèque',null]]){
  job.message=JSON.stringify({restore_progress:{phase,completed_bytes:done,total_bytes:total}});await page.reload();
  await expect(row).toContainText(label);await row.click();const dialog=page.getByRole('dialog');
  await expect(dialog.getByRole('heading',{name:'Récupération depuis S3 · CVT007L8'})).toBeVisible();await expect(dialog.getByText(label,{exact:true})).toBeVisible();
  if(percent===null)await expect(dialog.getByRole('progressbar')).toHaveCount(0);
  else await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow',String(percent));
  if(phase==='verifying')await expect(dialog).toContainText('Téléchargement terminé. La vérification et la réintégration peuvent se poursuivre.');
  if(phase==='downloading')for(const width of [1440,390]){await page.setViewportSize({width,height:900});await page.screenshot({path:`${screenshots}/download-${width}.png`,animations:'disabled'});}
  await page.keyboard.press('Escape');
 }
 job.status='failed';job.message='checksum mismatch';await page.reload();await row.click();await expect(page.getByRole('dialog')).toContainText('checksum mismatch');await expect(page.getByRole('progressbar')).toHaveCount(0);await page.keyboard.press('Escape');
 job.status='completed';job.message='{}';await page.reload();await expect(row).toContainText('100 %');await row.click();await expect(page.getByRole('dialog')).toContainText('Cartouche récupérée depuis S3 et réintégrée dans la bibliothèque.');await page.keyboard.press('Escape');
 await page.goto(ui.url+'/#media');await page.setViewportSize({width:1440,height:1000});
 await selectOption(page.getByRole('combobox',{name:'Type de transfert du graphique'}),'s3_restore');
 const chart=page.getByRole('figure',{name:'Débit depuis S3 de CVT007L8',exact:true});await expect(chart).toBeVisible();
 await expect(chart.getByRole('table',{name:'Historique du débit'})).toContainText('120 Mio/s');await expect(chart.locator('.lto-throughput-value')).toHaveText('120 Mio/s');
 assert.ok(queries.includes('s3_restore'));
 await selectOption(chart.getByRole('combobox',{name:'Type de transfert du graphique'}),'s3');await expect(page.getByRole('table',{name:'Historique du débit'})).not.toContainText('120 Mio/s');
 job.status='running';job.message=JSON.stringify({restore_progress:{phase:'downloading',completed_bytes:256,total_bytes:1024}});await page.reload();
 const lcd=page.getByRole('region',{name:'Journal d’activité de la bibliothèque'});
 await expect(lcd).toContainText('Récupération depuis S3 · CVT007L8');await expect(lcd).toContainText('25 % · 256 o / 1 Kio');
 const initialEntries=await lcd.locator('.lto-lcd-entry').count();
 job.message=JSON.stringify({restore_progress:{phase:'downloading',completed_bytes:512,total_bytes:1024}});
 await expect(lcd).toContainText('50 % · 512 o / 1 Kio',{timeout:12000});
 assert.equal(await lcd.locator('.lto-lcd-entry').count(),initialEntries,'Byte updates must not flood the LCD journal');
 for(const width of [1440,390]){await page.setViewportSize({width,height:1000});await lcd.scrollIntoViewIfNeeded();await page.screenshot({path:`${screenshots}/lcd-operations-${width}.png`,animations:'disabled'});assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));}
 job.message=JSON.stringify({restore_progress:{phase:'installing',completed_bytes:0,total_bytes:null}});
 await expect(lcd.locator('.lto-lcd-operations')).toContainText('Installation dans la bibliothèque',{timeout:12000});
 job.status='completed';job.message='{}';await expect(lcd.locator('.lto-lcd-operations')).toHaveCount(0,{timeout:12000});await expect(lcd.getByRole('log')).toContainText('Terminé');
 job.status='failed';job.message='checksum mismatch';await expect(lcd.getByRole('log')).toContainText('Échec · checksum mismatch',{timeout:12000});
 assert.deepEqual(errors,[]);
 // The journal can be cleared: its lines go and the states already seen do not come back.
 const clear=lcd.getByRole('button',{name:'Effacer le journal d’activité',exact:true});
 await clear.click();
 await expect(lcd.locator('.lto-lcd-entry')).toHaveCount(0);
 await expect(lcd.getByRole('log')).toContainText('Journal effacé');
 await expect(clear).toBeDisabled();
 await page.waitForTimeout(6000);
 await expect(lcd.locator('.lto-lcd-entry')).toHaveCount(0);
 console.log('PASS download: recovery labels, measured percentage, verification vs installation, errors, terminal state, direction-specific graph, current rate, mobile');
}finally{await browser.close();await ui.close();}
