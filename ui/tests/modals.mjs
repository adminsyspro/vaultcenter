import assert from 'node:assert/strict';
import { chromium, expect } from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi();
// The dialog heading is the activity-log LCD of the current theme, inside its 6 px bezel.
async function assertLcd(page,dialog){
 const dark=await page.evaluate(()=>document.documentElement.classList.contains('dark'));
 const lcd=await dialog.locator('[data-slot="dialog-heading"]').evaluate(el=>{const title=el.querySelector('[data-slot="dialog-title"]');const style=getComputedStyle(title);return {background:getComputedStyle(el).backgroundColor,transform:style.textTransform,prompt:el.querySelector('.lto-dialog-prompt[aria-hidden="true"]')?.textContent,bezel:getComputedStyle(el.closest('[data-slot="dialog-content"]')).borderTopWidth};});
 assert.deepEqual(lcd,{background:dark?'rgb(183, 206, 141)':'rgb(222, 235, 189)',transform:'uppercase',prompt:'>',bezel:'6px'});
 return dark;
}
const browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:['--no-sandbox']});
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 let message=JSON.stringify({status:'restored',logical_size_bytes:1048576,verified:true,files:Array.from({length:25},(_,i)=>({path:`/restore/file-${i}`,sha256:'a'.repeat(64)}))});
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'test',username:'test',role:'viewer',issuer:null,disabled:false},csrf_token:'test',local:true,oidc:false}}));
 await page.route('**/v1/**',r=>{
    if (new URL(r.request().url()).pathname.endsWith("/telemetry")) return r.fulfill({json:{version:1,vtl_id:new URL(r.request().url()).pathname.split("/")[3],generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
  const path=new URL(r.request().url()).pathname;
  let json=[];
  if(path==='/v1/vtls')json=[{id:'test-vtl',name:'Test',lifecycle:'ready',provisioning_revision:1}];
  if(path.endsWith('/status'))json={status:'ok',cartridges:0,jobs:1,media_root:'/test'};
  if(path.endsWith('/jobs'))json=[{id:'test-job',kind:'export',subject:'TEST01',status:'completed',progress:100,created_at:'2026-09-20T10:00:00Z',updated_at:'2026-09-20T10:01:00Z',message}];
  return r.fulfill({json});
 });
 await page.goto(`${process.env.COLD_VTL_UI_URL || ui.url}/#jobs`);
 for(const width of [1440,390]){
  await page.setViewportSize({width,height:1000});
  await expect(page.getByRole('button',{name:'Détails du job test-job'})).toHaveCount(0);
  await page.getByRole('row',{name:'Détails du job test-job'}).press(width === 390 ? 'Space' : 'Enter');
  const dialog=page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('Volume traité : 1 Mio.',{exact:true})).toBeVisible();
  await expect(dialog.getByText('25 fichier(s) référencé(s) dans le résultat.',{exact:true})).toBeVisible();
  await expect(dialog.getByText('Volume logique',{exact:true})).not.toBeVisible();
  const header=dialog.locator('[data-slot="dialog-header"]');
  // The heading is the LCD screen of the appliance: no icon, a prompt and the title in mono capitals.
  await expect(header.locator('svg')).toHaveCount(0);
  expect((await header.boundingBox()).height).toBe(56);
  assert.equal(await assertLcd(page,dialog),true);
  await dialog.getByText('Informations techniques',{exact:true}).click();
  await expect(dialog).toContainText('Volume logique');
  await expect(dialog).toContainText('1 Mio');
  await expect(dialog).toContainText('Restauré');
  await expect(dialog).toContainText('Oui');
  await expect(dialog.locator('pre')).toHaveCount(0);
  const heading=dialog.locator('[data-slot="dialog-heading"]');
  const body=dialog.locator('[data-slot="dialog-body"]');
  const before=await heading.boundingBox();
  await body.evaluate(el=>el.scrollTop=el.scrollHeight);
  expect(await body.evaluate(el=>el.scrollTop)).toBeGreaterThan(0);
  expect((await heading.boundingBox()).y).toBe(before.y);
  await expect(page.getByRole('button',{name:'Fermer les détails'})).toBeVisible();
  await expect.poll(async()=>{const b=await dialog.boundingBox();return Math.abs(b.x+b.width/2-width/2)<2;}).toBe(true);
  await expect.poll(async()=>{const b=await dialog.boundingBox();return b.x>=0 && b.x+b.width<=width;}).toBe(true);
  await page.getByRole('button',{name:'Fermer les détails'}).click();
  await expect(dialog).toHaveCount(0);
 }
 for (const value of ['Erreur de lecture', '{JSON invalide', null]) {
  message=value;
  await page.reload();
  await page.getByRole('row',{name:'Détails du job test-job'}).click();
  await expect(page.getByRole('dialog')).toContainText(value || 'Aucun compte rendu détaillé n’a été enregistré.');
  await page.keyboard.press('Escape');
 }
 await page.getByRole('button',{name:'Changer le thème'}).click();
 await page.getByRole('row',{name:'Détails du job test-job'}).click();
 await expect(page.getByRole('dialog')).toBeVisible();
 assert.equal(await assertLcd(page,page.getByRole('dialog')),false);
 console.log('PASS centered modal, fixed header while scrolling, structured/nested worker results, plain text, invalid JSON, empty result, dark theme and mobile');
}finally{await browser.close();await ui.close();}
