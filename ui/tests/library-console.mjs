import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi(),browser=await chromium.launch();
try{
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1600,height:1000}}),errors=[],writes=[];let role='admin',empty=false,telemetryReads=0,scenario='writing';
 page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/auth/**',r=>r.fulfill({json:/\/auth\/(tokens|users)$/.test(new URL(r.request().url()).pathname)?[]:{local:true,oidc:false,user:{id:'test',username:'admin',role,issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(r.request().method()!=='GET')writes.push(path);
  if(path==='/v1/vtls')return r.fulfill({json:empty?[]:[{id:demoId,name:'Bibliothèque de test',lifecycle:'ready',provisioning_revision:1,storage:null}]});
  if(path.endsWith('/telemetry')){telemetryReads++;const value=telemetryFixture(demoId,scenario);value.drives[0].write_rate_bps=270*1024**2;return r.fulfill({json:value});}
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/maintenance'))return r.fulfill({json:{configured:true,phase:'online',ready:false,blocked:false,services:{'tgt.service':'active','vtltape@11.service':'active'}}});
  if(path.endsWith('/settings/s3'))return r.fulfill({json:{providers:[],configured:false,revision:0,secret_configured:false,configuration:null,reconcile_required:0}});
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')||path.endsWith('/maintenance/events')?[]:path.endsWith('/cartridges/deletion')?{pending:null}:path.endsWith('/bandwidth')?{}:{status:'ok',cartridges:9,jobs:0}});
 });
 const capture=async name=>{if(process.env.COLD_VTL_CAPTURE_DIR)await page.screenshot({path:`${process.env.COLD_VTL_CAPTURE_DIR}/${name}.png`});};
 await page.goto(ui.url);
 const nav=page.getByRole('navigation',{name:'Navigation de la bibliothèque'});
 await expect(nav).toBeVisible();await expect(page.locator('[data-slot="sidebar"]')).toHaveCount(0);
 for(const label of ['Bibliothèque','Activité','Stockage S3','Réglages'])await expect(nav.getByRole('link',{name:label,exact:true})).toBeVisible();
 await expect(nav.getByRole('link',{name:'Bibliothèque',exact:true})).toHaveAttribute('aria-current','page');
 await expect(page.getByRole('article',{name:'Cartouche CVT001L8',exact:true})).toBeVisible();
 const status=page.getByRole('region',{name:'État des lecteurs'});
 await expect(status).toContainText('CVT001L8');await expect(status).toContainText('270 Mio/s');
 await nav.getByRole('link',{name:'Stockage S3',exact:true}).click();
 await expect(page.getByLabel('Nom du bucket',{exact:true})).toBeVisible();await expect(status).toContainText('Écriture en cours');
 await page.getByLabel('Nom du bucket',{exact:true}).fill('formulaire-en-cours');
 await page.getByRole('button',{name:'Maintenance',exact:true}).click();
 await expect(page.getByRole('dialog',{name:'Maintenance',exact:true})).toBeVisible();
 await page.getByRole('button',{name:'Fermer',exact:true}).click();
 await expect(page.getByLabel('Nom du bucket',{exact:true})).toHaveValue('formulaire-en-cours');
 await page.getByRole('link',{name:'Aller au contenu',exact:true}).focus();
 await page.keyboard.press('Enter');await expect(page.locator('#main-content')).toBeFocused();
 await expect(nav.getByRole('link',{name:'Stockage S3',exact:true})).toHaveAttribute('aria-current','page');
 await capture('storage');
 const before=telemetryReads;await expect.poll(()=>telemetryReads,{timeout:5000}).toBeGreaterThan(before);
 await nav.getByRole('link',{name:'Activité',exact:true}).click();await expect(status).toBeInViewport();
 await nav.getByRole('link',{name:'Réglages',exact:true}).click();
 const settings=page.getByRole('navigation',{name:'Rubriques des réglages'});
 for(const label of ['Notifications','Utilisateurs','Profil'])await expect(settings.getByRole('link',{name:label,exact:true})).toBeVisible();
 await expect(page.getByRole('heading',{name:'Notifications',exact:true})).toBeVisible();await capture('settings');
 await page.getByRole('link',{name:'Bibliothèques virtuelles',exact:true}).click();await expect(page.getByRole('button',{name:'Créer une VTL',exact:true})).toBeVisible();
 await page.goto(ui.url+`/#media/maintenance?vtl=${demoId}`);await expect(page.getByRole('dialog',{name:'Maintenance',exact:true})).toBeVisible();await page.getByRole('button',{name:'Fermer',exact:true}).click();
 for(const width of [1600,390]){
  await page.setViewportSize({width,height:1000});await expect(nav).toBeVisible();await expect(status).toBeInViewport();
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  await capture(`media-${width}`);
  if(width===1600){await page.getByRole('button',{name:'Changer le thème',exact:true}).click();await capture('media-theme');}
 }
 const mobileConnectionVisible=await status.locator('.lto-console-connection').isVisible();
 scenario='stale';await expect(status).toContainText('Activité inconnue',{timeout:8000});
 assert.deepEqual({mobileConnectionVisible,unknownCartridge:await status.innerText().then(text=>text.includes('Cartouche non identifiée')&&!text.includes('Sans cartouche'))},{mobileConnectionVisible:true,unknownCartridge:true});
 scenario='writing';
 await page.goto(ui.url+'/#media?vtl=00000000-0000-4000-8000-000000000099');await expect(page.getByText('Bibliothèque introuvable',{exact:true})).toBeVisible();
 await expect(page.getByRole('link',{name:'Bibliothèques virtuelles',exact:true})).toBeVisible();await expect(page.getByRole('button',{name:'Alimentation de la bibliothèque',exact:true})).toHaveCount(0);
 empty=true;await page.goto(ui.url+'/#vtls');await page.reload();await expect(page.getByRole('button',{name:'Créer une VTL',exact:true})).toBeVisible();
 empty=false;role='viewer';await page.goto(ui.url+'/#notifications');await page.reload();
 await expect(settings.getByRole('link',{name:'Notifications',exact:true})).toHaveCount(0);await expect(settings.getByRole('link',{name:'Utilisateurs',exact:true})).toHaveCount(0);
 await nav.getByRole('link',{name:'Bibliothèque',exact:true}).click();await expect(page.getByRole('button',{name:'Alimentation de la bibliothèque',exact:true})).toBeDisabled();
 assert.deepEqual(writes,[]);assert.deepEqual(errors,[]);
 console.log('PASS library console: facade navigation, default media, continuous telemetry, settings, legacy links, empty/unknown VTL, viewer and mobile.');
}finally{await browser.close();await ui.close();}
