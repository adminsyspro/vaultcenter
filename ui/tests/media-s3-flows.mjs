import {chromium,expect} from '@playwright/test';
import {mkdir} from 'node:fs/promises';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

// Catches merged transfers, wrong destination after rotation, stale animation,
// and a success badge inferred from completion without checksum evidence.
const ui=await staticUi(),browser=await chromium.launch();
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
 await page.addInitScript(()=>localStorage.setItem('theme','light'));
 let stale=false,unavailable=false;
 const jobs=[1,2].map(n=>({id:`upload-${n}`,kind:'export',subject:`CVT00${n}L8`,destination_id:'original',configuration_revision:1,generation:1,status:'running',progress:10,created_at:new Date().toISOString(),updated_at:new Date().toISOString(),message:JSON.stringify({export_progress:{phase:'uploading',completed_bytes:n*5*1024**3,total_bytes:1000*1024**3}})}));
 const bucket=(id,active)=>({id,bucket:id==='original'?'vc-veeam-2026-10-13-0ab0':'vc-veeam-next',active,created_at:new Date().toISOString(),configuration_revision:1,copy_count:0,observed_status:null,rotation_phase:null});
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const url=new URL(r.request().url()),path=url.pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VEEAM',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/storage/buckets'))return unavailable?r.fulfill({status:503,json:{error:'unavailable'}}):r.fulfill({json:[bucket('original',false),bucket('next',true)]});
  if(path.endsWith('/telemetry/history')){
   const result=emptyThroughputHistory(demoId,r.request().url()),id=url.searchParams.get('transfer_id'),barcode=url.searchParams.get('barcode');
   const job=jobs.find(j=>j.id===id&&j.subject===barcode);
   if(job){result.status='available';result.series=[{drive_id:'s3',barcode,transfer_id:id,last_at:Date.now()}];result.points=[{at:Date.now()-(stale?60000:0),writeRate:(id==='upload-1'?51:102)*1024**2,localGrowth:null}];}
   return r.fulfill({json:result});
  }
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia().slice(0,2):path.endsWith('/jobs')?jobs:path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#media');
 const shelf=page.getByRole('region',{name:'Stockage S3 — externalisation'});
 await expect(shelf).toBeVisible();
 const first=shelf.getByRole('article',{name:'Externalisation de CVT001L8'}),second=shelf.getByRole('article',{name:'Externalisation de CVT002L8'});
 await expect(first).toContainText('51 Mio/s');await expect(second).toContainText('102 Mio/s');
 await expect(first).toContainText('0,5 %');await expect(second).toContainText('1,0 %');
 await expect(first).toContainText(/5 Gio \/ 1\s000 Gio/);
 const original=shelf.getByRole('group',{name:'Destination vc-veeam-2026-10-13-0ab0'});
 await expect(original.getByRole('article')).toHaveCount(2);
 await expect(shelf.getByRole('group',{name:'Destination vc-veeam-next'}).getByRole('article')).toHaveCount(0);
 await expect(first).toHaveAttribute('data-flowing','true');
 const dir=process.env.COLD_VTL_SCREENSHOT_DIR;
 const capture=async name=>{if(dir){await shelf.evaluate(el=>el.scrollIntoView({block:'start'}));await page.screenshot({path:`${dir}/${name}.png`,fullPage:true});}};
 if(dir){await mkdir(dir,{recursive:true});await capture('s3-desktop-light');}
 await page.emulateMedia({reducedMotion:'reduce'});
 expect(await first.locator('.lto-s3-packet').evaluate(e=>getComputedStyle(e).animationName)).toBe('none');
 if(dir){await page.evaluate(()=>document.documentElement.classList.add('dark'));await capture('s3-desktop-dark');await page.evaluate(()=>document.documentElement.classList.remove('dark'));await page.setViewportSize({width:1818,height:884});await capture('s3-user-1818');}
 await page.setViewportSize({width:390,height:1300});
 for(const region of [shelf,first,second]){const box=await region.boundingBox();expect(box.x).toBeGreaterThanOrEqual(0);expect(box.x+box.width).toBeLessThanOrEqual(390);}
 await capture('s3-mobile');
 stale=true;await page.reload();await expect(first).toContainText('Débit indisponible');await expect(first).toHaveAttribute('data-flowing','false');
 jobs[0].status='completed';jobs[0].message=JSON.stringify({status:'verified'});
 // A finished send leaves the wide rows for the compact list.
 const done=shelf.getByRole('list',{name:'Terminés récemment'}).getByRole('listitem').filter({hasText:'CVT001L8'});
 await page.reload();await expect(done).toContainText('Empreinte non confirmée');await expect(done).not.toContainText('Empreinte validée');await expect(first).toHaveCount(0);
 jobs[0].message=JSON.stringify({status:'verified',receipt:{scope:{job_id:'upload-1',vtl_id:demoId,destination_id:'original',configuration_revision:1},generation:{barcode:'CVT001L8',generation:1,logical_size_bytes:1000*1024**3,segment_count:1},integrity:{status:'verified',method:'s3_sha256',verified_at:Math.floor(Date.now()/1000),verified_segments:1,verified_bytes:1000*1024**3}}});
 await page.reload();await expect(done).toContainText('Empreinte validée');await expect(done).toContainText(/1\s000 Gio/);await expect(done.locator('.lto-s3-track')).toHaveCount(0);
 jobs[1].status='failed';jobs[1].message='S3 request failed';await page.reload();await expect(second).toContainText('Échec');await expect(second.locator('.lto-s3-track')).toHaveCount(0);
 await capture('s3-results-mobile');
 unavailable=true;await page.reload();await expect(shelf).toContainText('Destinations S3 indisponibles');await expect(second).toBeVisible();await expect(done).toBeVisible();
 console.log('PASS independent S3 flows, pinned destinations, fractional progress, stale rates, checksum evidence, responsive and reduced motion');
} finally {await browser.close();await ui.close();}
