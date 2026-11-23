import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {selectOption} from './helpers/select.mjs';
const ui=await staticUi();let browser;
const screenshots='../.local/custom-selects-20261008/screenshots';await mkdir(screenshots,{recursive:true});
try {
 browser=await chromium.launch({args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const libraries=Array.from({length:50},(_,i)=>({id:`library-${i}`,name:`Bibliothèque ${String(i).padStart(2,'0')} — archivage et restauration`,lifecycle:'ready',provisioning_revision:1}));
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'admin',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'test'}}));
 let releaseConnection;let delayConnection=false;
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:libraries});
  if(path.endsWith('/storage/connection')){
   if(delayConnection)await new Promise(resolve=>{releaseConnection=resolve});
   return r.fulfill({json:{revision:1,configured:false,configuration:null,secret_configured:false}});
  }
  if(path.endsWith('/settings/tape-storage'))return r.fulfill({json:{vtl_id:path.split('/')[3],mode:'managed',phase:'ready',revision:1,path:'/srv/tapes',capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null}});
  if(path.endsWith('/settings/s3'))return r.fulfill({json:{providers:[],configured:false,revision:0,configuration:null,secret_configured:false}});
  if(path.endsWith('/cartridges')||path.endsWith('/jobs')||path.endsWith('/storage/buckets'))return r.fulfill({json:[]});
  return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
 });
 await page.goto(ui.url+'/#jobs');
 const trigger=page.getByRole('combobox',{name:'Bibliothèque VTL'});
 // This is a button-backed, themed control; no visible OS selects remain.
 await expect(trigger).toBeVisible();assert.equal(await trigger.evaluate(e=>e.tagName),'BUTTON');
 await expect(page.locator('select:visible')).toHaveCount(0);
 for(const [width,theme]of [[1440,'light'],[390,'dark']]){
  await page.setViewportSize({width,height:844});await page.evaluate(theme=>{document.documentElement.classList.toggle('dark',theme==='dark');},theme);
  await trigger.click();const list=page.getByRole('listbox');await expect(list).toBeVisible();
  const box=await list.boundingBox();assert.ok(box.x>=0&&box.x+box.width<=width&&box.y>=0&&box.y+box.height<=844);
  await page.screenshot({path:`${screenshots}/select-${theme}-${width}.png`,fullPage:false});
  await page.keyboard.press('End');await expect(list.getByRole('option').last()).toBeFocused();await page.keyboard.press('Enter');
  await expect(trigger).toHaveAttribute('data-value','library-49');
  await trigger.press('ArrowDown');await expect(list).toBeVisible();await page.keyboard.press('Home');await expect(list.getByRole('option').nth(1)).toBeFocused();await page.keyboard.press('Escape');
  await expect(trigger).toHaveAttribute('data-value','library-49');await expect(trigger).toBeFocused();
  await selectOption(trigger,'library-0');
 }
 const language=page.getByRole('combobox',{name:'Langue de l’interface'});
 await language.focus();await language.press('e');await expect(page.getByRole('combobox',{name:'Interface language'})).toHaveAttribute('data-value','en');
 await selectOption(page.getByRole('combobox',{name:'Interface language'}),'fr');
 delayConnection=true;await page.goto(ui.url+'/#vtls?vtl=library-0&storage=s3');
 const provider=page.getByRole('dialog').getByLabel('Fournisseur',{exact:true});await expect(provider).toBeDisabled();
 await provider.dispatchEvent('pointerdown',{button:0,pointerType:'mouse'});await expect(page.getByRole('listbox')).toHaveCount(0);
 await expect.poll(()=>!!releaseConnection).toBe(true);releaseConnection();await expect(provider).toBeEnabled();
 await selectOption(provider,'ovh_cold_archive_v1');await expect(provider).toContainText('OVHcloud');
 assert.deepEqual(errors,[]);
 console.log('PASS themed selectors: keyboard, typeahead, Escape/focus, long lists, viewport, disabled fieldset, language, no visible native selects');
}finally{await browser?.close();await ui.close();}
