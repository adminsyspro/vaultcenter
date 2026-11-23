import assert from 'node:assert/strict';
import {mkdir,readFile} from 'node:fs/promises';
const previewHtml=await readFile('../.local/notifications/email-preview.html','utf8').catch(()=>'<html><body><h1>Export S3 terminé</h1><p>VaultCenter demo</p></body></html>');
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi();let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM||undefined,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});page.setDefaultTimeout(5000);
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 let role='admin',saved=null,tests=0,conflict=false;
 const settings={enabled:false,installation:'VaultCenter demo',host:'smtp.example.test',port:587,tls:'starttls',username:'',sender:'vaultcenter@example.test',sender_name:'VaultCenter',recipients:['ops@example.test'],events:Object.fromEntries(['export','restore','reintegrate','archive','recall'].map(k=>[k,{success:true,failure:true}]))};
 let revision=1;
 await page.route('**/auth/tokens',r=>r.fulfill({json:[]}));
 await page.route('**/auth/**',r=>r.fulfill({json:new URL(r.request().url()).pathname==='/auth/tokens'?[]:new URL(r.request().url()).pathname==='/auth/me'?{user:{id:'admin',username:'admin',role,disabled:false,issuer:null},csrf_token:'csrf'}:{local:true,oidc:false}}));
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/notifications/settings'){
   if(r.request().method()==='PUT'){
    assert.equal(r.request().headers()['x-csrf-token'],'csrf');
    if(conflict)return r.fulfill({status:409,json:{error:'Configuration modifiée ailleurs. Recharger avant d’enregistrer.'}});
    saved=r.request().postDataJSON();Object.assign(settings,saved.settings);revision++;
   }
   return r.fulfill({json:{revision,settings,has_password:true}});
  }
  if(path==='/v1/notifications/preview')return r.fulfill({json:{subject:'[VaultCenter] Export S3 terminé',html:previewHtml,text:'Export S3 terminé'}});
  if(path==='/v1/notifications/test'){assert.equal(r.request().headers()['x-csrf-token'],'csrf');tests++;return r.fulfill({json:{status:'queued',id:'test'}});}
  if(path==='/v1/notifications/deliveries')return r.fulfill({json:{items:[],has_more:false,offset:0}});
  if(path==='/v1/vtls')return r.fulfill({json:[{id:'vtl',name:'VTL principale',lifecycle:'ready'}]});
  if(path.endsWith('/jobs/historic'))return r.fulfill({json:{id:'historic',kind:'export',subject:'OLD001L8',status:'completed',progress:100,message:null,created_at:'2026-10-01 10:00:00',updated_at:'2026-10-01 10:01:00'}});
  if(path.endsWith('/status'))return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  return r.fulfill({json:[]});
 });
 await page.goto(`${ui.url}/#notifications`);
 await expect(page.getByLabel('Serveur SMTP',{exact:true})).toBeVisible();
 await page.getByLabel('Activer les notifications').check();
 await page.getByRole('button',{name:'Enregistrer',exact:true}).click();
 await expect.poll(()=>saved?.settings.enabled).toBe(true);assert.equal(saved.password,null);
 await page.getByRole('button',{name:'Envoyer un email de test',exact:true}).click();
 await expect.poll(()=>tests).toBe(1);await expect(page.getByText('Email de test mis en file d’envoi.',{exact:true})).toBeVisible();
 await expect(page.locator('iframe[title="Aperçu de l’email"]')).toHaveAttribute('sandbox','');
 await mkdir('../.local/notifications',{recursive:true});
 await page.screenshot({path:'../.local/notifications/desktop.png',fullPage:true});
 await page.setViewportSize({width:390,height:844});
 await page.locator('iframe').scrollIntoViewIfNeeded();
 await expect(page.frameLocator('iframe').locator('h1')).toBeVisible();
 await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
 await page.screenshot({path:'../.local/notifications/mobile-preview.png'});
 await page.evaluate(()=>window.scrollTo(0,0));
 await page.screenshot({path:'../.local/notifications/mobile.png',fullPage:true});
 const emailPage=await browser.newPage({locale:'fr-FR',viewport:{width:390,height:844}});await emailPage.setContent(previewHtml);await emailPage.screenshot({path:'../.local/notifications/email-mobile.png',fullPage:true});await emailPage.close();
 assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 conflict=true;await page.getByLabel('Nom de l’installation').fill('Autre site');await page.getByRole('button',{name:'Enregistrer',exact:true}).click();await expect(page.getByRole('alert').filter({hasText:'Action non effectuée'})).toContainText('Configuration modifiée ailleurs');
 await page.goto(`${ui.url}/#jobs?vtl=vtl&job=historic`);await expect(page.getByRole('dialog')).toContainText('OLD001L8');
 role='viewer';await page.goto(`${ui.url}/#notifications`);await page.reload();await expect(page.getByRole('link',{name:'Notifications',exact:true})).toHaveCount(0);await expect(page.getByLabel('Serveur SMTP',{exact:true})).toHaveCount(0);
 assert.deepEqual(errors,[]);console.log('Notifications browser checks passed');
}finally{await browser?.close();await ui.close();}
