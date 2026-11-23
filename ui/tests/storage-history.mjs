import {selectOption} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
const ui=await staticUi();let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',timezoneId:'Europe/Paris',viewport:{width:1280,height:900}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 const base={bytes:12*1024**4,removed_at:null,removal:null,archived_at:null,recalled_at:null,readable_until:null,local_deleted_at:null};
 const copies=[
  {...base,barcode:'CVT001L8',generation:1,exported_at:'2026-10-21 18:07:12',destination_id:'a',bucket:'vc-a',bucket_state:'archived',archived_at:'2026-10-23 08:00:00',local_deleted_at:'2026-10-24 09:00:00'},
  {...base,barcode:'CVT002L8',generation:1,exported_at:'2026-10-21 19:00:00',destination_id:'b',bucket:'vc-b',bucket_state:'available'},
  {...base,barcode:'CVT003L8',generation:2,exported_at:'2026-10-20 10:00:00',destination_id:'a',bucket:'vc-a',bucket_state:'archived',removed_at:'2026-10-22 10:00:00',removal:'s3_deleted'},
 ];
 let history=copies,failing=false,historyCalls=0;
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'admin',issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const url=new URL(route.request().url());const path=url.pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[{id:demoId,name:'Veeam',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-b',prefix:'vtl'}}]});
  if(path.endsWith('/storage/history')){historyCalls++;return failing?route.fulfill({status:500,json:{error:'Catalogue indisponible'}}):route.fulfill({json:history});}
  if(path.endsWith('/storage/buckets'))return route.fulfill({json:[{id:'b',bucket:'vc-b',active:true,created_at:'2026-10-13',configuration_revision:1,copy_count:1,observed_status:null,rotation_phase:null}]});
  if(path.endsWith('/storage/recovery'))return route.fulfill({json:{destination_id:'b',bucket:'vc-b',complete:true,errors:[],access:{readable:true,writable:true,raw_status:'None',supported:true},copies:[]}});
  if(path.endsWith('/storage/archive'))return route.fulfill({json:{configured:true,operations:[]}});
  if(path.endsWith('/storage'))return route.fulfill({json:{destination_id:'b',active:true,bucket:'vc-b',raw_status:'None',supported:true,inventory:null,catalogue_generations:[]}});
  if(path.endsWith('/cartridges'))return route.fulfill({json:[]});
  if(path.endsWith('/jobs'))return route.fulfill({json:[]});
  return route.fulfill({json:{status:'ok',cartridges:0,jobs:0,media_root:'/fixture'}});
 });
 await page.goto(ui.url+'/#storage');
 const sections=page.getByRole('group',{name:'Rubriques de Stockage S3',exact:true});
 await expect(sections.getByRole('button',{name:'Buckets et copies',exact:true})).toHaveAttribute('aria-pressed','true');
 await sections.getByRole('button',{name:'Historique',exact:true}).click();
 const region=page.getByRole('region',{name:'Historique des copies',exact:true});
 const rows=region.locator('tbody tr');
 await expect(rows).toHaveCount(3);
 const first=rows.filter({hasText:'CVT001L8'});
 await expect(first).toContainText('Archivé · lecture seule');
 // 2026-10-23 08:00 UTC is 10:00 in Paris: dates are SQLite UTC.
 await expect(first).toContainText(/archivé le .*10:00/);
 await expect(first).toContainText(/24 oct\..*11:00/);
 await expect(region).toContainText('Dernier état connu. L’état en direct est dans la fenêtre du bucket.');
 const csv=region.getByRole('link',{name:'Exporter CSV',exact:true});
 await expect(csv).toHaveAttribute('href',`/v1/vtls/${demoId}/storage/history?format=csv`);
 await expect(csv).toHaveAttribute('download','');
 await selectOption(region.getByLabel('Copies',{exact:true}),'cold');
 await expect(rows).toHaveCount(1);await expect(rows.first()).toContainText('CVT001L8');
 await selectOption(region.getByLabel('Copies',{exact:true}),'removed');
 await expect(rows).toHaveCount(1);await expect(rows.first()).toContainText('CVT003L8');await expect(rows.first()).toContainText('Supprimée de S3 le');
 await selectOption(region.getByLabel('Copies',{exact:true}),'all');
 await region.getByLabel('Rechercher une cartouche ou un bucket',{exact:true}).fill('vc-b');
 await expect(rows).toHaveCount(1);await expect(rows.first()).toContainText('CVT002L8');
 await rows.first().getByRole('button',{name:'vc-b',exact:true}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog.getByRole('heading',{name:'vc-b',exact:true})).toBeVisible();
 await page.keyboard.press('Escape');await expect(dialog).toHaveCount(0);
 await region.getByLabel('Rechercher une cartouche ou un bucket',{exact:true}).fill('');
 await page.setViewportSize({width:390,height:844});
 await expect(rows).toHaveCount(3);
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await page.screenshot({path:'test-results/storage-history-mobile.png',fullPage:true});
 await page.setViewportSize({width:1280,height:900});
 await region.screenshot({path:'test-results/storage-history-desktop.png'});
 history=[];await sections.getByRole('button',{name:'Buckets et copies',exact:true}).click();await sections.getByRole('button',{name:'Historique',exact:true}).click();
 await expect(region).toContainText('Aucune copie enregistrée pour cette VTL.');
 failing=true;await sections.getByRole('button',{name:'Buckets et copies',exact:true}).click();await sections.getByRole('button',{name:'Historique',exact:true}).click();
 await expect(region.getByRole('alert')).toContainText('Catalogue indisponible');
 failing=false;history=copies;const before=historyCalls;
 await region.getByRole('button',{name:'Réessayer',exact:true}).click();
 await expect(rows).toHaveCount(3);assert.ok(historyCalls>before);
 assert.deepEqual(errors,[]);
 console.log('PASS storage history: sections, Cold Archive and removed filters, search, local UTC dates, CSV link, bucket window, empty, error and retry, mobile');
} finally {await browser?.close();await ui.close();}
