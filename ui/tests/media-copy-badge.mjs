import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

// The tape itself says where its latest verified copy lives, and whether that bucket went cold.
const ui=await staticUi();let browser;
const MiB=1024**2;
const copy=(bucket_state,bytes)=>({bucket:'vc-veeam-2026-10-13-0ab0',generation:1,checked_at:'2026-10-21T16:07:12Z',bucket_state,bytes});
// Telemetry observes each tape at its catalogue size (CVT001L8 slightly more, as it is being written);
// CVT003L8 holds 40 Mio locally but only 30 Mio were copied.
const media=()=>demoMedia().slice(0,6).map((m,i)=>({...m,barcode:`CVT00${i+1}L8`,last_copy:[copy('archived',2048*MiB),copy('available',600*MiB),copy('available',30*MiB),copy('archiving',0),undefined,{...copy('available'),bucket_state:undefined,bytes:undefined}][i]}));
async function routes(page){
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VEEAM',lifecycle:'ready',provisioning_revision:1}]});
  if(path==='/v1/activity')return r.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  if(path.endsWith('/storage/buckets'))return r.fulfill({json:[]});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  return r.fulfill({json:path.endsWith('/cartridges')?media():path.endsWith('/jobs')?[]:path.endsWith('/maintenance')?{configured:false}:{status:'ok',cartridges:6,jobs:0,media_root:'/demo'}});
 });
}
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 await routes(page);
 await page.goto(`${ui.url}/#media?vtl=${demoId}`);
 const tape=barcode=>page.getByRole('article',{name:`Cartouche ${barcode}`}).first();
 const badge=barcode=>tape(barcode).getByRole('img',{name:/^Copie sur S3/});
 await expect(badge('CVT001L8')).toHaveAccessibleName('Copie sur S3 · archivée cold · vc-veeam-2026-10-13-0ab0 · génération 1 · vérifiée le 21/10/2026');
 await expect(badge('CVT001L8')).toHaveAttribute('data-tone','cold');
 await expect(badge('CVT002L8')).toHaveAccessibleName(/^Copie sur S3 · vc-veeam/);
 await expect(badge('CVT002L8')).toHaveAttribute('data-tone','available');
 // Written again after its copy (observed local volume above the copied size): the copy is older.
 await expect(badge('CVT003L8')).toHaveAttribute('data-stale','true');
 await expect(badge('CVT003L8')).toHaveAccessibleName(/antérieure aux dernières écritures/);
 await expect(badge('CVT004L8')).toHaveAttribute('data-busy','true');
 await expect(badge('CVT004L8')).toHaveAccessibleName(/archivage cold en cours/);
 // No verified copy, no sticker.
 await expect(tape('CVT005L8')).toBeVisible();
 await expect(badge('CVT005L8')).toHaveCount(0);
 // An API without bucket_state (older appliance) still shows the copy as available.
 await expect(badge('CVT006L8')).toHaveAttribute('data-tone','available');
 for(const barcode of ['CVT001L8','CVT002L8','CVT004L8','CVT006L8'])await expect(badge(barcode)).toHaveAttribute('data-stale','false');
 assert.deepEqual(errors,[]);
 console.log('PASS media copy badge: cold archive, available, stale, archiving, absent, older API');
} finally { await browser?.close(); await ui.close(); }
