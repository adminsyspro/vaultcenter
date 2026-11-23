import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi();let browser;
const a='00000000-0000-4000-8000-000000000001',b='00000000-0000-4000-8000-000000000003';
try{
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM||undefined,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});page.setDefaultTimeout(5000);
 let role='admin',locked=false,phase='unconfigured',fail=false,delay=false;
 const requests=[],errors=[];page.on('pageerror',e=>errors.push(e.message));
 const status=id=>({vtl_id:id,mode:'managed',phase,revision:0,path:locked||phase==='ready'?'/mnt/bandes/'+(id===a?'a':'b'):null,capacity:null,operation:null,message:null,data_locked:locked,change_allowed:!locked,change_lock_reason:locked?'data_locked':null});
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'admin',username:'admin',role,disabled:false,issuer:null},csrf_token:'csrf',local:true,oidc:false}}));
 await page.route('**/v1/**',async r=>{
  const p=new URL(r.request().url()).pathname;requests.push([p,r.request().method()]);
  if(p==='/v1/vtls')return r.fulfill({json:[{id:a,name:'VTL A',lifecycle:'draft',provisioning_revision:1},{id:b,name:'VTL B',lifecycle:'ready',provisioning_revision:1}]});
  const id=p.includes(a)?a:b;
  if(p.endsWith('/settings/tape-storage')) {const value=status(id);if(delay&&id===a)await new Promise(resolve=>setTimeout(resolve,1200));return r.fulfill({json:value}).catch(()=>{});}
  if(p.endsWith('/tape-storage/check')){assert.equal(r.request().headers()['x-csrf-token'],'csrf');return r.fulfill({json:{vtl_id:id,path:r.request().postDataJSON().path,filesystem:'ext4',total_bytes:1024**4,available_bytes:800*1024**3,check_id:'checked'}});}
  if(p.endsWith('/tape-storage/apply')){assert.equal(id,a);if(fail){locked=true;phase='ready';return r.fulfill({status:409,json:{error:'Chemin verrouillé : présence de données.'}});}phase='applying';return r.fulfill({json:status(id)});}
  if(p.endsWith('/telemetry'))return r.fulfill({status:503,json:{error:'Télémétrie indisponible'}});
  if(p.endsWith('/status'))return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  return r.fulfill({json:[]});
 });
await page.goto(`${ui.url}/#vtls?vtl=${a}&tab=storage&storage=local`);
 const dialog=page.getByRole('dialog',{name:'VTL A',exact:true});await expect(dialog).toBeVisible();
 await expect(dialog.getByRole('button',{name:'Local',exact:true})).toHaveAttribute('aria-pressed','true');
 const input=dialog.getByLabel('Chemin de stockage des bandes',{exact:true});
 await input.fill('/mnt/bandes/a');const save=dialog.getByRole('button',{name:'Enregistrer',exact:true});
 await expect(save).toBeDisabled();await dialog.getByRole('button',{name:'Vérifier le chemin',exact:true}).click();await expect(save).toBeEnabled();
 await input.fill('/mnt/autre');await expect(save).toBeDisabled();
 await input.fill('/mnt/bandes/a');await dialog.getByRole('button',{name:'Vérifier le chemin',exact:true}).click();await expect(save).toBeEnabled();
 fail=true;await save.click();await expect(dialog).toContainText('Chemin verrouillé');await expect(input).toHaveAttribute('readonly','');await expect(save).toBeDisabled();
 const before=requests.filter(([p,m])=>p.endsWith('/tape-storage/check')&&m==='POST').length;
 await dialog.getByRole('button',{name:'Vérifier le stockage',exact:true}).click();assert.equal(requests.filter(([p,m])=>p.endsWith('/tape-storage/check')&&m==='POST').length,before);
 await mkdir('../.local/vtl-storage',{recursive:true});await page.screenshot({path:'../.local/vtl-storage/modal-desktop.png',fullPage:true});
 await page.setViewportSize({width:390,height:844});await page.screenshot({path:'../.local/vtl-storage/modal-mobile.png',fullPage:true});assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 await page.keyboard.press('Escape');await expect(dialog).toHaveCount(0);
 // A late answer for A never shows in B's window.
 await page.setViewportSize({width:1440,height:1000});locked=false;delay=true;
 await page.goto(`${ui.url}/#vtls?vtl=${a}&tab=storage&storage=local`);await expect(dialog).toBeVisible();await page.keyboard.press('Escape');
 await page.goto(`${ui.url}/#vtls?vtl=${b}&tab=storage&storage=local`);const other=page.getByRole('dialog',{name:'VTL B',exact:true});await expect(other).toBeVisible();
 await page.waitForTimeout(1400);await expect(other).not.toContainText('VTL A');
 await page.keyboard.press('Escape');
 // The former library settings page opens the same window.
 await page.goto(`${ui.url}/#settings?vtl=${b}`);await expect(page.getByRole('dialog',{name:'VTL B',exact:true})).toBeVisible();await page.keyboard.press('Escape');
 await page.goto(`${ui.url}/#tape-storage?vtl=${b}`);await expect(page).toHaveURL(new RegExp(`#media\\?vtl=${b}`));
 await expect(page.getByRole('link',{name:'Stockage des bandes',exact:true})).toHaveCount(0);
 role='viewer';await page.reload();await expect(page.getByRole('dialog')).toHaveCount(0);
 assert.ok(requests.filter(([p])=>p.includes('/tape-storage')).every(([p])=>p.startsWith('/v1/vtls/')));assert.deepEqual(errors,[]);
 console.log('Tape storage modal passed: drafts, isolated scope, locked path, conflict refresh, late response, keyboard, mobile and permissions.');
}finally{await browser?.close();await ui.close();}
