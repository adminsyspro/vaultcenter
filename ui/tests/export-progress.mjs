import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try{
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 const job={id:'export-test',kind:'export',subject:'CVT007L8',generation:1,status:'running',progress:10,created_at:new Date().toISOString(),updated_at:new Date().toISOString(),message:'upload and deep verification started'};
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'writing')});
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[job]:path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#jobs');
 await page.getByRole('row',{name:'Détails du job export-test'}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog.getByText('10%',{exact:true})).not.toBeVisible();
 await expect(dialog.getByRole('heading',{name:'Envoi vers S3 · CVT007L8'})).toBeVisible();
 await expect(dialog.getByText('export-test',{exact:true})).not.toBeVisible();
 await expect(dialog.getByText('upload and deep verification started',{exact:true})).not.toBeVisible();
 await expect(dialog.getByText('Transfert et vérification en cours.',{exact:true})).toBeVisible();
 for(const [phase,done,label,percent] of [['uploading',256,'Envoi vers S3',25],['verifying',1024,'Vérification de la copie S3',100]]){
  job.message=JSON.stringify({export_progress:{phase,completed_bytes:done,total_bytes:1024}});
  await page.reload();await page.getByRole('row',{name:'Détails du job export-test'}).click();
  await expect(dialog.getByText(label,{exact:true})).toBeVisible();
  await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow',String(percent));
  await expect(dialog.getByText(`${done===256?'256 o':'1 Kio'} / 1 Kio`,{exact:true})).toBeVisible();
  await page.keyboard.press('Escape');await page.goto(ui.url+'/#media');
  const cloud=page.getByRole('progressbar',{name:'Envoi de CVT007L8 vers S3',exact:true});
  await expect(cloud).toHaveAttribute('aria-valuenow',String(percent));
  await page.goto(ui.url+'/#jobs');
 }
 job.message='upload and deep verification started';
 await page.goto(ui.url+'/#media');await page.reload();
 await expect(page.getByRole('progressbar',{name:'Envoi de CVT007L8 vers S3',exact:true})).not.toHaveAttribute('aria-valuenow');
 for(const width of [1440,390]){
  job.message=JSON.stringify({export_progress:{phase:'uploading',completed_bytes:256,total_bytes:1024}});
  await page.setViewportSize({width,height:900});await page.goto(ui.url+'/#jobs');await page.reload();
  await page.getByRole('row',{name:'Détails du job export-test'}).click();
  const box=await dialog.boundingBox();expect(box.height).toBeLessThan(650);expect(box.x).toBeGreaterThanOrEqual(0);expect(box.x+box.width).toBeLessThanOrEqual(width);
  if(process.env.COLD_VTL_SCREENSHOT_DIR)await page.screenshot({path:`${process.env.COLD_VTL_SCREENSHOT_DIR}/export-${width}.png`});
  await page.keyboard.press('Escape');
 }
 console.log('PASS truthful export progress, compact modal, hidden technical detail, cartridge cloud, desktop/mobile');
}finally{await browser.close();await ui.close();}
