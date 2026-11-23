import {selectOption} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
const ui=await staticUi(); let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 // Production appliances can use HTTP: randomUUID requires a secure context.
 await page.addInitScript(() => Object.defineProperty(crypto,'randomUUID',{value:undefined}));
 const media=demoMedia(), posts=[]; let failure=false,pending=null,ready=true,role='admin';
 const barcode=media[0].barcode;
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role,issuer:null,disabled:false},csrf_token:'test-token'}}));
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/cartridges/deletion')){
   const body=r.request().method()==='POST'?r.request().postDataJSON():null;
   return r.fulfill({json:{ready,pending,reason:ready?null:'Entrer en maintenance avant cette opération.',cartridges:body?.barcodes.map(b=>({barcode:b,exports:[{destination_id:'old-bucket',bucket:'archives-2025',generation:7,checked_at:'2026-10-01 10:00:00'}]}))}});
  }
  if(path.endsWith('/cartridges/delete-local')){
   const body=r.request().postDataJSON(); posts.push(body); pending=body;
   assert.equal(r.request().headers()['x-csrf-token'],'test-token');
   if(failure)return r.fulfill({status:409,json:{error:'Suppression interrompue : réessayer.'}});
   media.find(m=>m.barcode===barcode).local_deleted=true; pending=null;
   return r.fulfill({json:{phase:'completed',request_id:body.request_id,barcodes:body.barcodes}});
  }
  if(path.endsWith('/maintenance'))return r.fulfill({json:{configured:true,phase:ready?'maintenance':'online',ready,client:'pbs'}});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture()});
  return r.fulfill({json:path.endsWith('/cartridges')?media:path.endsWith('/jobs')?[]:{status:'ok',cartridges:media.length,jobs:0,media_root:'/demo'}});
 });
 await page.goto(ui.url+'/#media');
 await page.getByRole('checkbox',{name:`Sélectionner ${barcode}`,exact:true}).check();
 await page.getByRole('button',{name:'Supprimer (1)',exact:true}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog).toContainText(`${barcode} · ✓ copie S3 · archives-2025 v7`);
 await expect(dialog).toContainText('conservées');
 await expect(dialog.getByRole('button',{name:'Supprimer',exact:true})).toBeDisabled();
 await dialog.getByRole('button',{name:'Annuler',exact:true}).click();assert.equal(posts.length,0);
 ready=false;
 await page.getByRole('button',{name:'Supprimer (1)',exact:true}).click();
 await expect(dialog).toContainText('Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis PBS.');
 await expect(dialog.getByRole('button',{name:'Supprimer',exact:true})).toBeDisabled();
 await expect(dialog.getByRole('button',{name:'Passer en maintenance',exact:true})).toBeVisible();
 await dialog.getByRole('button',{name:'Annuler',exact:true}).click();
 await expect(page.getByRole('checkbox',{name:`Sélectionner ${barcode}`,exact:true})).toBeChecked();
 ready=true; failure=true;
 await page.getByRole('button',{name:'Supprimer (1)',exact:true}).click();
 await dialog.getByRole('checkbox').check();
 await page.screenshot({path:process.env.DELETION_SCREENSHOT||'/tmp/vaultcenter-deletion-desktop.png',fullPage:true});
 await dialog.getByRole('button',{name:'Supprimer',exact:true}).click();
 await expect(dialog).toContainText('Suppression interrompue');
 assert.deepEqual(posts[0].barcodes,[barcode]);assert.equal(posts[0].confirmation,'delete-local');
 await page.reload();
 await page.getByRole('button',{name:'Reprendre la suppression',exact:true}).click();
 await expect(dialog).toContainText(barcode);
 await page.setViewportSize({width:390,height:844});
 await page.screenshot({path:'/tmp/vaultcenter-deletion-mobile.png',fullPage:true});
 assert.ok(await dialog.evaluate(e=>e.getBoundingClientRect().right<=innerWidth));
 failure=false; await dialog.getByRole('checkbox').check();
 await dialog.getByRole('button',{name:'Supprimer',exact:true}).click();
 await expect(dialog).toHaveCount(0); assert.deepEqual(posts[1],posts[0]);
 await expect(page.getByRole('article',{name:`Cartouche ${barcode}`,exact:true})).toHaveCount(0);
 await selectOption(page.getByLabel('Filtrer les cartouches par état'), 'deleted');
 await expect(page.getByRole('button',{name:`Détails de ${barcode}`,exact:true})).toBeVisible();
 role='viewer';await page.reload();
 await expect(page.getByRole('button',{name:/Supprimer \(/})).toHaveCount(0);
 console.log('cartridge deletion: preserved S3 links, maintenance, cancel, recovery, local filter, responsive modal, viewer passed');
} finally {await browser?.close();await ui.close();}
