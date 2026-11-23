import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
const ui=await staticUi();let browser;
const bucket=(id,name,active=false)=>({id,bucket:name,active,created_at:'2026-10-13',configuration_revision:1,copy_count:0,observed_status:null,rotation_phase:null});
let buckets,retired,retireFails,role;
function reset(){buckets=[bucket('b','vc-b',true),bucket('gone','vc-gone'),bucket('denied','vc-denied')];retired=[];retireFails=false;role='admin';}
async function routes(page){
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role,issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const url=new URL(route.request().url());const path=url.pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[{id:demoId,name:'Veeam',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-b',prefix:'vtl'}}]});
  if(path==='/v1/activity')return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  const retire=path.match(/\/storage\/buckets\/([^/]+)\/retire$/);
  if(retire){
   assert.equal(route.request().method(),'POST');
   if(retireFails)return route.fulfill({status:409,json:{error:'OVH ne confirme pas que le bucket vc-gone n’existe plus. Il n’est pas retiré.'}});
   retired.push(retire[1]);buckets=buckets.filter(value=>value.id!==retire[1]);
   return route.fulfill({json:{destination_id:retire[1],bucket:'vc-gone'}});
  }
  if(path.endsWith('/storage/buckets'))return route.fulfill({json:buckets});
  if(path.endsWith('/storage/recovery')){
   const id=url.searchParams.get('destination_id');
   if(id==='gone')return route.fulfill({status:410,json:{error:'Ce bucket n’existe pas chez OVH.'}});
   if(id==='denied')return route.fulfill({status:409,json:{error:'Error: Request("service error: AccessDenied")'}});
   return route.fulfill({json:{destination_id:'b',bucket:'vc-b',complete:true,errors:[],access:{readable:true,writable:true,raw_status:'None',supported:true},copies:[]}});
  }
  if(path.endsWith('/storage/history'))return route.fulfill({json:[]});
  if(path.endsWith('/storage/archive'))return route.fulfill({json:{configured:true,operations:[]}});
  if(path.endsWith('/storage'))return route.fulfill({json:{destination_id:'b',active:true,bucket:'vc-b',raw_status:'None',supported:true,inventory:null,catalogue_generations:[]}});
  if(path.endsWith('/cartridges')||path.endsWith('/jobs'))return route.fulfill({json:[]});
  return route.fulfill({json:{status:'ok',cartridges:0,jobs:0,media_root:'/fixture'}});
 });
}
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',timezoneId:'Europe/Paris',viewport:{width:1280,height:900}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 reset();await routes(page);
 await page.goto(`${ui.url}/#storage?vtl=${demoId}`);
 const list=page.getByRole('list',{name:'Buckets',exact:true});
 const gone=list.getByRole('listitem').filter({hasText:'vc-gone'});
 const denied=list.getByRole('listitem').filter({hasText:'vc-denied'});
 await expect(gone).toContainText('Introuvable chez OVH');
 await expect(denied).toContainText('Lecture impossible');
 // Problems stay on their row: no block of raw errors below the list.
 await expect(page.getByText('AccessDenied')).toBeHidden();
 await expect(page.getByText('Ce bucket n’existe pas chez OVH.')).toBeHidden();
 await expect(page.getByText('Inventaire partiel')).toHaveCount(1);
 await denied.getByText('Détails',{exact:true}).click();
 await expect(denied.getByText(/AccessDenied/)).toBeVisible();
 await gone.getByText('Détails',{exact:true}).click();
 await expect(gone.getByText('Ce bucket n’existe pas chez OVH.')).toBeVisible();
 // Only the bucket OVH no longer knows can be retired.
 await expect(denied.getByRole('button',{name:'Retirer',exact:true})).toHaveCount(0);
 await expect(list.getByRole('listitem').filter({hasText:'vc-b'}).getByRole('button',{name:'Retirer',exact:true})).toHaveCount(0);
 await gone.getByRole('button',{name:'Retirer',exact:true}).click();
 await expect(gone).toContainText('Retirer vc-gone du catalogue ? Aucune copie ni historique n’est supprimé.');
 await gone.getByRole('button',{name:'Annuler',exact:true}).click();
 await expect(gone.getByText(/du catalogue \?/)).toHaveCount(0);
 assert.deepEqual(retired,[]);
 // A refusal is shown on the row.
 retireFails=true;
 await gone.getByRole('button',{name:'Retirer',exact:true}).click();
 await gone.getByRole('button',{name:'Retirer vc-gone',exact:true}).click();
 await expect(gone.getByRole('alert')).toContainText('OVH ne confirme pas');
 // The confirmation stays open with the refusal; trying again from it works.
 retireFails=false;
 await gone.getByRole('button',{name:'Retirer vc-gone',exact:true}).click();
 await expect(list.getByRole('listitem').filter({hasText:'vc-gone'})).toHaveCount(0);
 assert.deepEqual(retired,['gone']);
 await expect(list.getByRole('listitem')).toHaveCount(2);
 await page.setViewportSize({width:390,height:844});
 await denied.scrollIntoViewIfNeeded();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await page.screenshot({path:'test-results/bucket-problems-mobile.png',fullPage:true});
 await page.setViewportSize({width:1280,height:900});
 await list.screenshot({path:'test-results/bucket-problems-desktop.png'});
 // A read-only account sees the problem but cannot retire.
 const viewer=await browser.newPage({locale:'fr-FR',timezoneId:'Europe/Paris',viewport:{width:1280,height:900}});
 viewer.on('pageerror',error=>errors.push(error.message));
 reset();role='viewer';await routes(viewer);
 await viewer.goto(`${ui.url}/#storage?vtl=${demoId}`);
 const viewerGone=viewer.getByRole('list',{name:'Buckets',exact:true}).getByRole('listitem').filter({hasText:'vc-gone'});
 await expect(viewerGone).toContainText('Introuvable chez OVH');
 await expect(viewerGone.getByRole('button',{name:'Retirer',exact:true})).toHaveCount(0);
 assert.deepEqual(errors,[]);
 console.log('PASS bucket problems: badges on each row, details on demand, no error block, retire with confirmation, refusal shown, admin only, mobile');
} finally {await browser?.close();await ui.close();}
