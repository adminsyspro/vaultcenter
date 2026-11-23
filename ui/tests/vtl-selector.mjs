import {selectOption} from './helpers/select.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi();
let browser;
try {
browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});page.setDefaultTimeout(8000);
 const a='00000000-0000-4000-8000-000000000001',b='00000000-0000-4000-8000-000000000003';
 const errors=[],requests=[],mutations=[];let delayed=false;
 page.on('pageerror',e=>errors.push(e.message));
 const libs=[{id:a,name:'VTL A',lifecycle:'ready',provisioning_revision:1},{id:b,name:'VTL B',lifecycle:'ready',provisioning_revision:1}];
 const cartridges={};for(const [i,id] of [a,b].entries())cartridges[id]=[{barcode:'TAPE01',capacity_bytes:10485760,bytes_written:1048576*(i+1),generation:1,state:'writable'}];
 await page.route('**/auth/**',route=>route.fulfill({json:{user:{id:'admin',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'csrf',local:true,oidc:false}}));
 await page.route('**/v1/**',async route=>{

  const path=new URL(route.request().url()).pathname;requests.push(path);
  if(path==='/v1/vtls')return route.fulfill({json:libs});
  // The header bell lists every VTL, so its route is the one global read.
  if(path==='/v1/activity')return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  const match=path.match(/^\/v1\/vtls\/([^/]+)\/(.+)$/);
  assert.ok(match,`Unscoped request: ${path}`);const [,id,action]=match;assert.ok([a,b].includes(id));
  if(action==='telemetry/history')return route.fulfill({json:emptyThroughputHistory(id,route.request().url())});
  if(action==='telemetry') {
   const value=telemetryFixture(id,id===a?'writing':'reading'); value.drives[0].barcode='TAPE01';
   if(id===a&&delayed)await new Promise(r=>setTimeout(r,1500));
   return route.fulfill({json:value}).catch(()=>{});
  }
  if(route.request().method()==='POST'){
   assert.equal(action,'cartridges/TAPE01/export');assert.equal(route.request().headers()['x-csrf-token'],'csrf');mutations.push(id);cartridges[id][0].state='uploading';return route.fulfill({json:cartridges[id][0]});
  }
  if(action==='cartridges'&&id===a&&delayed)await new Promise(r=>setTimeout(r,700));
  const data={client:{schema:1,vtl_id:id,revision:0,client:'pbs',initiator:'10.0.0.1',editable:false,message:null},status:{status:'ok',cartridges:1,jobs:0,media_root:`/media/${id}`},cartridges:cartridges[id],jobs:[],'cartridges/deletion':{ready:false,pending:null},'cartridges/options':{storage:null},'storage/connection':{configured:false,revision:0,configuration:null,secret_configured:false},'storage/buckets':[],'storage/schedule':{revision:0,rotation:'manual',weekday:null,month_day:null,archive_after_days:null,archive_terms_accepted:false,rotation_supported:false,rotation_due:false,period_start:null,active_bucket:null},'settings/s3':{providers:[],configured:false,revision:0,secret_configured:false,configuration:null,reconcile_required:0},'settings/tape-storage':{vtl_id:id,mode:'managed',phase:'ready',revision:1,path:'/srv/tapes',capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null},maintenance:{configured:false},'maintenance/events':[],reintegrations:{configured:false,operations:[],eligible:[],pending:null}};
  assert.ok(action in data,`Unexpected request: ${path}`);await route.fulfill({json:data[action]}).catch(()=>{});
 });
 await page.goto(`${ui.url}/#media?vtl=${a}`);
 const selector=page.getByRole('combobox',{name:'Bibliothèque VTL',exact:true});
 await expect(selector).toHaveAttribute('data-value', a);
 const row=()=>page.getByRole('article',{name:'Cartouche TAPE01',exact:true});
 await expect(row()).toContainText('1 Mio');
 await page.getByRole('checkbox',{name:'Sélectionner TAPE01',exact:true}).check();
 await page.getByRole('button',{name:'Envoyer vers S3 (1)',exact:true}).click();
 await expect(page.getByRole('dialog')).toBeVisible();
 await page.evaluate(id=>{location.hash=`media?vtl=${id}`;},b);
 await expect(selector).toHaveAttribute('data-value', b);await expect(page.getByRole('dialog')).toHaveCount(0);
 await expect(row()).toContainText('2 Mio');
 await expect(page.getByRole('checkbox',{name:'Sélectionner TAPE01',exact:true})).not.toBeChecked();
 await page.getByRole('checkbox',{name:'Sélectionner TAPE01',exact:true}).check();
 await page.getByRole('button',{name:'Envoyer vers S3 (1)',exact:true}).click();
 await page.getByRole('dialog').getByRole('checkbox').check();
 await page.getByRole('dialog').getByRole('button',{name:'Envoyer vers S3',exact:true}).click();
 await expect.poll(()=>mutations.length).toBe(1);assert.deepEqual(mutations,[b]);
 await page.keyboard.press('Escape');
 delayed=true;await selectOption(selector, a);await selectOption(selector, b);
 await expect(row()).toContainText('2 Mio');await page.waitForTimeout(1900);await expect(row()).toContainText('2 Mio');await expect(page.locator('.lto-drive').getByText('Lecture en cours',{exact:true})).toBeVisible();await expect(page.locator('.lto-drive').getByText('Écriture en cours',{exact:true})).toHaveCount(0);
 await page.getByRole('link',{name:'Activité',exact:true}).click();await expect(page).toHaveURL(new RegExp(`jobs\\?vtl=${b}`));
 await page.reload();await expect(selector).toHaveAttribute('data-value', b);
 await page.goBack();await expect(selector).toHaveAttribute('data-value', b);
 await page.evaluate(()=>{location.hash='media?vtl=00000000-0000-4000-8000-000000000099';});
 await expect(page.getByText('Bibliothèque introuvable',{exact:true})).toBeVisible();await expect(row()).toHaveCount(0);
 await selectOption(selector, a);await expect(row()).toContainText('1 Mio');
 // The former S3 settings route opens the selected library's Stockage dialog on its S3 tab.
 await page.goto(`${ui.url}/#s3settings?vtl=${b}`);
 await expect(page).toHaveURL(new RegExp(`#vtls\\?vtl=${b}$`));
 const storageDialog=page.getByRole('dialog',{name:'VTL B',exact:true});
 await expect(storageDialog.getByRole('button',{name:'S3',exact:true})).toHaveAttribute('aria-pressed','true');
 await storageDialog.getByText('Options avancées',{exact:true}).click();await expect(storageDialog.getByLabel('Bucket existant',{exact:true})).toBeVisible();
 assert.ok(requests.includes(`/v1/vtls/${b}/settings/s3`));
 await expect(page.getByRole('link',{name:'Configuration S3',exact:true})).toHaveCount(0);
 await page.keyboard.press('Escape');await expect(storageDialog).toHaveCount(0);
 await expect(selector).toHaveAttribute('data-value', b);
 await page.goto(`${ui.url}/#maintenance?vtl=${b}`);
 await expect(page).toHaveURL(new RegExp(`#media/maintenance\\?vtl=${b}$`));
 await expect(page.getByRole('dialog',{name:'Maintenance',exact:true})).toBeVisible();
 await page.getByRole('button',{name:'Fermer',exact:true}).click();
 await expect(selector).toHaveAttribute('data-value', b);
 await expect(row()).toContainText('2 Mio');
 await page.goto(`${ui.url}/#media?vtl=${a}`);
 await expect(row()).toContainText('1 Mio');
 await expect(selector).toBeVisible();
 await page.setViewportSize({width:390,height:844});
 await expect(selector).toBeVisible();
 await selectOption(selector, b);
 await expect(row()).toContainText('2 Mio');
 await expect(selector).toHaveAttribute('data-value', b);
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
 assert.deepEqual(errors,[]);console.log('VTL selector passed: scope, mutations, dialog/selection reset, delayed response, navigation, reload, unknown ID and mobile.');
}finally{await browser?.close();await ui.close();}
