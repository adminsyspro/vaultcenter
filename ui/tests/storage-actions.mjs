import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
import {mkdir} from 'node:fs/promises';
const ui=await staticUi(); const browser=await chromium.launch({args:['--no-sandbox']});
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 let status='None',role='admin',deleted=false,jobs=[],withOthers=false; const writes=[];
 const copy={barcode:'CVT007L8',generation:1,manifest_sha256:'a'.repeat(64),logical_size_bytes:30738569427,manifest_key:'vc/cartridges/CVT007L8/manifests/1.json'};
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,user:{id:'test',username:'test',role,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const request=route.request(),url=new URL(request.url()),path=url.pathname;
  if(request.method()!=='GET')writes.push({path,body:request.postDataJSON()});
  if(path==='/v1/vtls')return route.fulfill({json:[{id:demoId,name:'VTL principale',lifecycle:'ready',storage:{provider:'ovh_cold_archive_v1',bucket:'vault-2026-10',prefix:'vc'}}]});
  if(path.endsWith('/settings/s3'))return route.fulfill({json:{configured:true,providers:[],revision:1}});
  if(path.endsWith('/storage/buckets'))return route.fulfill({json:[{id:'bucket-id',bucket:'vault-2026-10',active:true,configuration_revision:1},...(withOthers?[{id:'bucket-b',bucket:'vault-2026-09',active:false,configuration_revision:1},{id:'bucket-c',bucket:'vault-2026-08',active:false,configuration_revision:1}]:[])]}); 
  if(path.endsWith('/storage/recovery')&&url.searchParams.get('destination_id')==='bucket-b')return route.fulfill({json:{destination_id:'bucket-b',configuration_revision:1,bucket:'vault-2026-09',complete:true,errors:[],access:{readable:true,writable:true,supported:true,raw_status:'None'},copies:[{...copy,generation:2,manifest_sha256:'b'.repeat(64)}]}});
  if(path.endsWith('/storage/recovery')&&url.searchParams.get('destination_id')==='bucket-c')return route.fulfill({json:{destination_id:'bucket-c',configuration_revision:1,bucket:'vault-2026-08',complete:true,errors:[],access:{readable:false,writable:false,supported:true,raw_status:'Archived'},copies:[{...copy,generation:3,manifest_sha256:null}]}});
  if(path.endsWith('/storage/recovery'))return route.fulfill({json:{destination_id:'bucket-id',configuration_revision:1,bucket:'vault-2026-10',complete:true,errors:[],access:{readable:['None','Restored'].includes(status),writable:status==='None',supported:true,raw_status:status},copies:deleted?[]:[{...copy,manifest_sha256:['None','Restored'].includes(status)?copy.manifest_sha256:null}]}});
  if(path.endsWith('/storage/copies/delete')){const job={id:'delete-job',kind:'s3_delete',subject:copy.barcode,status:'queued',progress:0,destination_id:'bucket-id',generation:1};jobs=[job];return route.fulfill({status:202,json:job});}
  if(path.endsWith('/jobs'))return route.fulfill({json:jobs});
  if(path.endsWith('/cartridges'))return route.fulfill({json:[]});
  if(path.endsWith('/storage'))return route.fulfill({json:{destination_id:'bucket-id',bucket:'vault-2026-10',provider:'ovh_cold_archive_v1',supported:true,raw_status:status,state_label:'Archivé',object_access:'Liste',inventory:null,checks:[]}});
  if(path.endsWith('/storage/archive'))return route.fulfill({json:{configured:true,operations:[]}});
  return route.fulfill({json:{status:'ok',cartridges:0,jobs:jobs.length}});
 });
 await page.clock.install();
 await page.goto(ui.url+'/#storage');
 const region=page.getByRole('region',{name:'Copies S3',exact:true});
 const bucket=region.getByRole('list',{name:'Buckets',exact:true}).getByRole('listitem').filter({hasText:'vault-2026-10'});
 const row=region.getByRole('row').filter({has:page.getByRole('rowheader',{name:copy.barcode})});
 // Another usable copy is listed; an archived bucket's copy is not; the request carries the shown copy's bucket and barcode.
 withOthers=true;await page.reload();
 // Copies are grouped under their bucket: the row is looked up in that bucket's table.
 const mainRow=region.getByRole('table',{name:'Copies S3 de vault-2026-10',exact:true}).getByRole('row').filter({has:page.getByRole('rowheader',{name:copy.barcode})});
 await mainRow.getByRole('button',{name:'Supprimer la copie S3',exact:true}).click();
 const withDialog=page.getByRole('dialog');
 await expect(withDialog).toContainText('Autres copies de CVT007L8');await expect(withDialog).toContainText('vault-2026-09 · génération 2');
 await expect(withDialog).not.toContainText('vault-2026-08');await expect(withDialog).not.toContainText('aucune autre copie');
 await withDialog.getByRole('button',{name:'Supprimer définitivement',exact:true}).click();await expect(withDialog).toHaveCount(0);
 assert.equal(writes.length,1);assert.equal(writes[0].body.confirm_bucket,'vault-2026-10');assert.equal(writes[0].body.confirm_barcode,'CVT007L8');assert.equal(writes[0].body.destination_id,'bucket-id');
 writes.length=0;jobs=[];withOthers=false;await page.reload();
 await row.getByRole('button',{name:'Supprimer la copie S3',exact:true}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog).toContainText('Dans vault-2026-10 · génération 1');await expect(dialog).toContainText('⚠ aucune autre copie');
 await expect(dialog.getByRole('textbox')).toHaveCount(0);
 const submit=dialog.getByRole('button',{name:'Supprimer définitivement',exact:true});
 await expect(submit).toBeEnabled();
 await dialog.getByRole('button',{name:'Annuler',exact:true}).click();assert.equal(writes.length,0);
 await row.getByRole('button',{name:'Supprimer la copie S3',exact:true}).click();
 await submit.click();
 await expect(dialog).toHaveCount(0);assert.equal(writes.length,1);assert.equal(writes[0].body.manifest_sha256,copy.manifest_sha256);assert.equal(writes[0].body.generation,1);assert.equal(writes[0].body.confirm_barcode,'CVT007L8');
 await expect(region).toContainText('Suppression en cours');await expect(row.getByRole('button',{name:'Réintégrer',exact:true})).toBeDisabled();
 jobs=[{...jobs[0],status:'failed',message:'Suppression interrompue. Actualisez puis réessayez.'}];
 await page.reload();await expect(row).toContainText('Suppression interrompue');
 jobs=[];
 for(const [state,label] of [['Archived','Archivé · lecture seule'],['Restoring','Rappel en cours'],['Restored','Disponible · lecture seule'],['Archiving','Archivage en cours'],['Unknown','Accès à vérifier']]){
  // The access state is the bucket's: it shows on the bucket heading, above its copies.
  status=state;await page.reload();await expect(bucket).toContainText(label);
  await expect(row.getByRole('button',{name:'Supprimer la copie S3',exact:true})).toBeDisabled();
  if(state==='Archived'){
   await row.getByRole('button',{name:'Rappeler le bucket',exact:true}).click();
   await expect(page.getByRole('dialog')).toContainText('Archivé sur bande');await expect(page.getByRole('dialog').getByRole('button',{name:'Rappeler le bucket…',exact:true})).toBeVisible();
   assert.equal(writes.length,1,'Opening recall does not submit a charged request');await page.keyboard.press('Escape');
  }
 }
 status='Restoring';await page.reload();await row.getByRole('rowheader').click();await expect(dialog).toContainText('Rappel en cours');
 status='Restored';await page.clock.runFor(31000);await expect(dialog).toContainText('Disponible · lecture seule');await expect(dialog.getByRole('button',{name:'Réintégrer…',exact:true})).toBeEnabled();await page.keyboard.press('Escape');
 status='None';role='viewer';await page.reload();await expect(row.getByRole('button',{name:'Supprimer la copie S3',exact:true})).toHaveCount(0);
 role='admin';status='Archived';await page.reload();
 await mkdir('../.local/s3-actions-20261008/screenshots',{recursive:true});
 for(const width of [1440,390]){await page.setViewportSize({width,height:1000});await page.screenshot({path:`../.local/s3-actions-20261008/screenshots/storage-${width}.png`,fullPage:true});}
 console.log('S3 deletion confirmation, active/failed feedback, cold/read-only states, recall entry and viewer access passed.');
} finally {await browser.close();await ui.close();}
