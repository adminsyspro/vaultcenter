import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 let role='admin',reject=false,patches=0;
 const item={id:'00000000-0000-4000-8000-000000000001',name:'VTL principale',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'archive-2026-10',prefix:'vc'}};
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,user:{id:'test',username:'test',role,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',async route=>{
  const req=route.request(),path=new URL(req.url()).pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[item]});
  if(path===`/v1/vtls/${item.id}`&&req.method()==='PATCH'){
   patches++;assert.equal(req.headers()['x-csrf-token'],'test');assert.equal(req.postDataJSON().expected_name,item.name);
   await new Promise(resolve=>setTimeout(resolve,200));
   if(reject)return route.fulfill({status:409,json:{error:'Une bibliothèque porte déjà ce nom.'}});
   item.name=req.postDataJSON().name;return route.fulfill({json:item});
  }
  if(path.endsWith('/cartridges')||path.endsWith('/jobs'))return route.fulfill({json:[]});
  return route.fulfill({json:{status:'ok',configured:false,cartridges:0,jobs:0}});
 });
 await page.goto(ui.url+'/#vtls');
 await page.getByRole('button',{name:'Renommer VTL principale',exact:true}).click();
 const dialog=page.getByRole('dialog'),input=dialog.getByLabel('Nom de la bibliothèque'),submit=dialog.getByRole('button',{name:'Enregistrer',exact:true});
 await expect(input).toHaveValue('VTL principale');await expect(submit).toBeDisabled();
 await input.fill('  ');await expect(submit).toBeDisabled();
 await input.fill(' Sauvegardes production ');reject=true;await submit.dblclick();
 await expect(dialog).toContainText('Une bibliothèque porte déjà ce nom.');assert.equal(patches,1);
 await expect(page.getByRole('heading',{name:'VTL principale',exact:true,includeHidden:true})).toBeVisible();
 reject=false;await submit.click();await expect(dialog).toHaveCount(0);
 await expect(page.getByRole('heading',{name:'Sauvegardes production',exact:true})).toBeVisible();
 await expect(page.getByRole('combobox').first()).toContainText('Sauvegardes production');
 assert.equal(item.id,'00000000-0000-4000-8000-000000000001');assert.equal(item.storage.bucket,'archive-2026-10');
 await page.reload();await expect(page.getByRole('heading',{name:item.name,exact:true})).toBeVisible();
 await mkdir('../.local/vtl-rename-20261009/screenshots',{recursive:true});
 for(const width of [1440,390]){
  await page.setViewportSize({width,height:900});await page.getByRole('button',{name:`Renommer ${item.name}`,exact:true}).click();
  await input.fill('Archives mensuelles');const box=await dialog.boundingBox();assert.ok(box.x>=0&&box.x+box.width<=width);
  await page.screenshot({path:`../.local/vtl-rename-20261009/screenshots/rename-${width}.png`,animations:'disabled'});
  await dialog.getByRole('button',{name:'Annuler',exact:true}).click();assert.equal(patches,2);
 }
 role='viewer';await page.reload();await expect(page.getByRole('heading',{name:item.name,exact:true})).toBeVisible();await expect(page.getByRole('button',{name:/^Renommer /})).toHaveCount(0);
 console.log('PASS VTL rename: prefilled name, validation, duplicate rejection, single submission, persisted list and selector, cancel, mobile and viewer.');
} finally {await browser.close();await ui.close();}
