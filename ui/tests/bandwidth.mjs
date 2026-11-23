import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi(),browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM});
const a='00000000-0000-4000-8000-000000000001',b='00000000-0000-4000-8000-000000000002';
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1360,height:1000}});page.setDefaultTimeout(5000);
 let role='admin',clientDown=false,conflict=false,revision=1;const writes=[],errors=[];page.on('pageerror',e=>errors.push(e.message));
 let limits={tape_write_bps:null,s3_bps:null};
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'admin',username:'admin',role,issuer:null,disabled:false},csrf_token:'csrf',local:true,oidc:false}}));
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname,id=path.includes(b)?b:a;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:a,name:'VTL A',lifecycle:'ready',provisioning_revision:2},{id:b,name:'VTL B',lifecycle:'ready',provisioning_revision:2}]});
  if(path.endsWith('/bandwidth')) {
   if(r.request().method()==='PUT') {
    assert.equal(r.request().headers()['x-csrf-token'],'csrf');const value=r.request().postDataJSON();writes.push([id,value]);
    if(conflict)return r.fulfill({status:409,json:{error:'Révision de débit modifiée ; recharger avant d’enregistrer.'}});
    assert.equal(value.revision,revision);revision++;limits={tape_write_bps:value.tape_write_bps,s3_bps:value.s3_bps};
   }
   return r.fulfill({json:{vtl_id:id,revision,...(id===a?limits:{tape_write_bps:null,s3_bps:null}),tape:{state:'pending',observed_revision:null},s3:{state:'configured'},message:null}});
  }
  if(path.endsWith('/client')&&clientDown)return r.fulfill({status:503,json:{error:'Lecture indisponible'}});
  if(path.endsWith('/client'))return r.fulfill({json:{schema:1,vtl_id:id,revision:0,client:'pbs',initiator:'192.168.1.104',editable:false,message:null}});
  if(path.endsWith('/provisioning'))return r.fulfill({json:{vtl_id:id,phase:'active',qualified:true}});
  if(path.endsWith('/status'))return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  if(path.endsWith('/telemetry'))return r.fulfill({status:503,json:{error:'indisponible'}});
  return r.fulfill({json:[]});
 });
 await page.goto(`${ui.url}/#vtls`);await page.getByRole('button',{name:'Ouvrir VTL A',exact:true}).click();
 const dialog=page.getByRole('dialog',{name:'VTL A',exact:true});await dialog.getByRole('button',{name:'Débit',exact:true}).click();await mkdir('../.local/bandwidth-20261005/ui',{recursive:true});

 await expect(dialog.getByRole('heading',{name:'Limites de débit',exact:true})).toBeVisible();
 await dialog.getByLabel('Limiter PBS → bandes',{exact:true}).check();await dialog.getByLabel('Limite PBS → bandes (Mo/s)',{exact:true}).fill('50');
 await dialog.getByLabel('Limiter S3',{exact:true}).check();await dialog.getByLabel('Limite S3 (Mo/s)',{exact:true}).fill('100');
 await dialog.getByRole('button',{name:'Enregistrer les limites',exact:true}).click();await expect.poll(()=>writes.length).toBe(1);
 assert.deepEqual(writes[0],[a,{revision:1,tape_write_bps:50_000_000,s3_bps:100_000_000}]);await expect(dialog).toContainText('confirmation du lecteur');
 conflict=true;await dialog.getByLabel('Limite S3 (Mo/s)',{exact:true}).fill('25');await dialog.getByRole('button',{name:'Enregistrer les limites',exact:true}).click();
 await expect(dialog).toContainText('Révision de débit modifiée');await expect(dialog.getByLabel('Limite S3 (Mo/s)',{exact:true})).toHaveValue('25');
 conflict=false;await dialog.getByLabel('Limiter S3',{exact:true}).uncheck();await dialog.getByRole('button',{name:'Enregistrer les limites',exact:true}).click();
 await expect.poll(()=>writes.length).toBe(3);assert.equal(writes[2][1].s3_bps,null);await expect(dialog.getByRole('button',{name:'Enregistrer les limites',exact:true})).toBeEnabled();
 await page.screenshot({path:'../.local/bandwidth-20261005/ui/limits-desktop.png',fullPage:true});await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);
 await page.screenshot({path:'../.local/bandwidth-20261005/ui/limits-mobile.png',fullPage:true});assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 await page.keyboard.press('Escape');await page.getByRole('button',{name:'Ouvrir VTL B',exact:true}).click();await page.getByRole('dialog',{name:'VTL B',exact:true}).getByRole('button',{name:'Débit',exact:true}).click();await expect(page.getByLabel('Limiter PBS → bandes',{exact:true})).not.toBeChecked();
 // An unreadable client type keeps the neutral wording instead of guessing PBS.
 clientDown=true;await page.keyboard.press('Escape');await page.getByRole('button',{name:'Ouvrir VTL B',exact:true}).click();await page.getByRole('dialog',{name:'VTL B',exact:true}).getByRole('button',{name:'Débit',exact:true}).click();
 await expect(page.getByLabel('Limiter Sauvegarde → bandes',{exact:true})).toBeVisible();await expect(page.getByLabel('Limiter PBS → bandes',{exact:true})).toHaveCount(0);clientDown=false;
 await page.keyboard.press('Escape');role='viewer';await page.reload();await expect(page.getByRole('button',{name:'Ouvrir VTL A',exact:true})).toHaveCount(0);
 await expect(page.getByRole('heading',{name:'Limites de débit',exact:true})).toHaveCount(0);assert.deepEqual(errors,[]);
 console.log('Bandwidth UI passed: scope, units, save, conflict, unlimited, layout and permissions');
} finally {await browser.close();await ui.close();}
