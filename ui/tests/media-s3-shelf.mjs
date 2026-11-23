import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

// The S3 shelf of the library folds away, remembers it, and forgets tapes deleted locally.
const ui=await staticUi();let browser;
const now=new Date().toISOString();
const job=(n,status)=>({id:`export-${n}`,kind:'export',subject:`CVT00${n}L8`,destination_id:'original',configuration_revision:1,generation:1,status,progress:status==='running'?10:100,created_at:now,updated_at:now,message:null});
let jobs;
const media=()=>demoMedia().slice(0,3).map((m,i)=>({...m,barcode:`CVT00${i+1}L8`,local_deleted:i===0}));
async function routes(page){
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VEEAM',lifecycle:'ready',provisioning_revision:1}]});
  if(path==='/v1/activity')return r.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  if(path.endsWith('/storage/buckets'))return r.fulfill({json:[{id:'original',bucket:'vc-veeam-2026-10-13-0ab0',active:true,created_at:now,configuration_revision:1,copy_count:2,observed_status:null,rotation_phase:null}]});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  return r.fulfill({json:path.endsWith('/cartridges')?media():path.endsWith('/jobs')?jobs:path.endsWith('/maintenance')?{configured:false}:{status:'ok',cartridges:3,jobs:jobs.length,media_root:'/demo'}});
 });
}
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 jobs=[job(1,'completed'),job(2,'completed'),job(3,'running')];
 await routes(page);
 await page.goto(`${ui.url}/#media?vtl=${demoId}`);
 const shelf=page.getByRole('region',{name:'Stockage S3 — externalisation'});
 const toggle=shelf.getByRole('button',{name:'Stockage S3',exact:true});
 await expect(toggle).toHaveAttribute('aria-expanded','true');
 // A tape deleted locally leaves the shelf; the others stay.
 const done=shelf.getByRole('list',{name:'Terminés récemment'}).getByRole('listitem');
 await expect(done).toHaveCount(1);await expect(done).toContainText('CVT002L8');
 await expect(shelf.getByRole('article',{name:'Externalisation de CVT003L8'})).toBeVisible();
 await expect(shelf).not.toContainText('CVT001L8');
 // Folded: only the heading, its count and a running indicator remain.
 await toggle.click();
 await expect(toggle).toHaveAttribute('aria-expanded','false');
 await expect(shelf.getByRole('article')).toHaveCount(0);await expect(done).toHaveCount(0);
 await expect(shelf).toContainText('1 en cours');
 await expect(shelf.locator('.lto-s3-running')).toBeVisible();
 await page.screenshot({path:'test-results/media-s3-shelf-folded.png',fullPage:true});
 // The choice survives a reload.
 await page.reload();
 await expect(toggle).toHaveAttribute('aria-expanded','false');
 await expect(shelf.getByRole('article')).toHaveCount(0);
 // Keyboard unfolds it.
 await toggle.focus();await page.keyboard.press('Enter');
 await expect(toggle).toHaveAttribute('aria-expanded','true');
 await expect(shelf.getByRole('article')).toHaveCount(1);await expect(done).toHaveCount(1);
 // No transfer: folded shelf shows no running indicator.
 jobs=[job(2,'completed')];await page.reload();
 await toggle.click();
 await expect(toggle).toHaveAttribute('aria-expanded','false');
 await expect(shelf.locator('.lto-s3-running')).toHaveCount(0);
 await expect(shelf).toContainText('Aucun envoi en cours');
 // A tape still uploading stays even if it is marked deleted.
 jobs=[job(1,'running')];await page.reload();
 await toggle.click();
 await expect(shelf.getByRole('article',{name:'Externalisation de CVT001L8'})).toBeVisible();
 await page.setViewportSize({width:390,height:844});
 await toggle.click();await expect(toggle).toHaveAttribute('aria-expanded','false');
 await shelf.scrollIntoViewIfNeeded();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await page.screenshot({path:'test-results/media-s3-shelf-mobile.png',fullPage:true});
 assert.deepEqual(errors,[]);
 console.log('PASS media S3 shelf: deleted tapes leave it, fold and unfold, remembered, keyboard, running indicator, mobile');
} finally {await browser?.close();await ui.close();}
