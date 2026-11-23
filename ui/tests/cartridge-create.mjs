import {selectOption} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
const ui=await staticUi();let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});let role='admin',ready=true,online=true,failure=false,pending=null;const media=demoMedia();const posts=[];let batch=null,free=32*1024**3;
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role,issuer:null,disabled:false},csrf_token:'test-token'}}));
 await page.route('**/v1/**',async r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/cartridges/options'))return r.fulfill({json:{configured:true,online_creation:online,pending_creation:pending,pending_batch:batch?.phase==='creating'?batch:null,storage:{free_bytes:free,reserved_bytes:0,available_bytes:free,overhead_per_cartridge:16*1024**2},ready,reason:ready?null:'Entrer en maintenance avant de créer une cartouche.',media_types:['LTO8'],default_media_type:'LTO8',default_capacity_mib:2048,default_segment_size_mib:1048576,default_multipart_part_size_mib:256,min_multipart_part_size_mib:16,max_multipart_part_size_mib:1024,min_capacity_mib:1,max_capacity_mib:17179869184,min_segment_size_mib:16,max_segment_size_mib:1048576,free_slots:pending?0:7}});
  if(path.endsWith('/cartridges/batch')&&r.request().method()==='POST'){
   assert.equal(r.request().headers()['x-csrf-token'],'test-token');const body=r.request().postDataJSON();posts.push(body);
   await new Promise(resolve=>setTimeout(resolve,100));
   if(!batch || batch.request.request_id!==body.request_id)batch={request:body,barcodes:Array.from({length:body.count},(_,i)=>`CVT${String(10+i).padStart(3,'0')}L8`),completed:0,phase:'creating'};
   if(failure && batch.completed===1)return r.fulfill({status:409,json:{error:'Création interrompue, reprendre le lot.'}});
   const barcode=batch.barcodes[batch.completed++];media.push({...body,barcode,bytes_written:0,generation:1,state:'writable'});
   if(batch.completed===body.count)batch.phase='completed';
   return r.fulfill({json:batch});
  }
  if(path.endsWith('/cartridges')&&r.request().method()==='POST'){
   assert.equal(r.request().headers()['x-csrf-token'],'test-token');const body=r.request().postDataJSON();posts.push(body);
   await new Promise(resolve=>setTimeout(resolve,150));
   if(failure)return r.fulfill({status:409,json:{error:'Emplacement occupé, actualisez les options.'}});
   const item={...body,bytes_written:0,generation:1,state:'writable'};media.push(item);return r.fulfill({status:201,json:item});
  }
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture()});
  return r.fulfill({json:path.endsWith('/cartridges')?media:path.endsWith('/jobs')?[]:{status:'ok',cartridges:media.length,jobs:0,media_root:'/demo'}});
 });
 await page.goto(ui.url+'/#media');await page.getByRole('button',{name:'Créer des cartouches',exact:true}).click();
 const dialog=page.getByRole('dialog');const submit=dialog.getByRole('button',{name:'Créer 3 cartouches',exact:true});
 await expect(dialog).toContainText('Ajout à chaud disponible');
 await expect(dialog.getByLabel('Code-barres',{exact:true})).toHaveCount(0);
 await dialog.getByLabel('Nombre de cartouches',{exact:true}).fill('3');
 await expect(dialog).toContainText('Identifiants attribués automatiquement');
 await expect(submit).toBeEnabled();
 const capacityInput=dialog.getByLabel('Capacité virtuelle',{exact:true});
 const capacitySlider=dialog.getByRole('slider',{name:'Régler la capacité virtuelle',exact:true});
 await expect(capacitySlider).toHaveAttribute('aria-valuetext','2 Gio');
 await capacitySlider.press('ArrowRight');await expect(capacityInput).toHaveValue('4');
 await capacityInput.fill('3');await expect(capacitySlider).toHaveAttribute('aria-valuetext','3 Gio');
 await selectOption(dialog.getByLabel('Unité de capacité',{exact:true}), '1');
 await expect(capacityInput).toHaveValue('3072');await expect(capacitySlider).toHaveAttribute('aria-valuetext','3 Gio');
 await selectOption(dialog.getByLabel('Unité de capacité',{exact:true}), '1024');
 await capacitySlider.press('ArrowRight');await expect(capacityInput).toHaveValue('4');
 const segmentSlider=dialog.getByRole('slider',{name:'Régler la taille des segments',exact:true});
 await segmentSlider.press('Home');await expect(segmentSlider).toHaveAttribute('aria-valuetext','16 Mio');
 await expect(dialog.getByLabel('Taille des segments de la tape',{exact:true})).toHaveValue('16');
 await expect(dialog.getByLabel('Unité des segments',{exact:true})).toHaveAttribute('data-value', '1');
 await segmentSlider.press('End');await expect(segmentSlider).toHaveAttribute('aria-valuetext','1 Tio');
 const partsSlider=dialog.getByRole('slider',{name:'Régler la taille des parts multipart',exact:true});
 await partsSlider.press('ArrowRight');await expect(dialog.getByLabel('Taille des parts multipart (Mio)',{exact:true})).toHaveValue('512');

 await expect(dialog.getByLabel('Taille des segments de la tape',{exact:true})).toHaveValue('1');
 await expect(dialog.getByLabel('Unité des segments',{exact:true})).toHaveAttribute('data-value', '1048576');
 await dialog.getByLabel('Taille des parts multipart (Mio)',{exact:true}).fill('16');
 await expect(submit).toBeDisabled();await expect(dialog).toContainText('10 000');
 await dialog.getByLabel('Taille des parts multipart (Mio)',{exact:true}).fill('128');

 failure=true;await submit.click();await expect(dialog).toContainText('Création interrompue');
 await expect(dialog).toContainText('1 / 3');
 assert.equal(posts.length,2);assert.equal(posts[0].barcode,undefined);
 const id=posts[0].request_id;
 await dialog.locator('form').getByRole('button',{name:'Fermer',exact:true}).click();
 await page.getByRole('button',{name:'Créer des cartouches',exact:true}).click();
 await expect(dialog).toContainText('Lot à reprendre');
 await expect(dialog.getByLabel('Nombre de cartouches',{exact:true})).toBeDisabled();
 failure=false;await dialog.getByRole('button',{name:'Reprendre le lot',exact:true}).click();
 await expect(dialog).toHaveCount(0);assert.equal(posts.length,4);assert.ok(posts.every(p=>p.request_id===id));
 assert.deepEqual(posts[0],{request_id:id,count:3,media_type:'LTO8',capacity_bytes:4*1024**3,segment_size_mib:1048576,multipart_part_size_mib:128});
 await page.getByRole('textbox',{name:'Rechercher une cartouche'}).fill('CVT012L8');
 await expect(page.getByRole('article',{name:'Cartouche CVT012L8',exact:true})).toBeVisible();
 await page.getByRole('textbox',{name:'Rechercher une cartouche'}).fill('');
 free=1024**3;batch=null;
 await page.getByRole('button',{name:'Créer des cartouches',exact:true}).click();
 await expect(dialog).toContainText('Espace insuffisant');
 await expect(dialog.getByRole('button',{name:'Créer 1 cartouche',exact:true})).toBeDisabled();
 await dialog.getByLabel('Nombre de cartouches',{exact:true}).fill('8');
 await expect(dialog).toContainText('emplacements');
 await page.setViewportSize({width:390,height:844});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
 await dialog.locator('form').getByRole('button',{name:'Fermer',exact:true}).click();
 free=32*1024**3;ready=false;online=false;
 await page.getByRole('button',{name:'Créer des cartouches',exact:true}).click();await expect(dialog).toContainText('Entrer en maintenance');
 await expect(dialog.getByRole('link',{name:'Ouvrir la maintenance'})).toBeVisible();
 await expect(dialog.getByRole('button',{name:'Créer 1 cartouche',exact:true})).toBeDisabled();
 await dialog.locator('form').getByRole('button',{name:'Fermer',exact:true}).click();
 ready=true;pending={barcode:'NEW001L8',media_type:'LTO8',capacity_bytes:2*1024**3,segment_size_mib:256,multipart_part_size_mib:256};
 await page.getByRole('button',{name:'Créer des cartouches',exact:true}).click();await expect(dialog).toContainText('Création à reprendre');
 await dialog.getByRole('button',{name:'Reprendre la création',exact:true}).click();await expect(dialog).toHaveCount(0);
 assert.equal(posts.at(-1).barcode,'NEW001L8');pending=null;
 role='viewer';await page.reload();await expect(page.getByRole('button',{name:'Créer des cartouches',exact:true})).toHaveCount(0);
 console.log('PASS batches: automatic IDs, count, capacity budget, geometry, CSRF, progress, interrupted batch reopen/resume, legacy resume, viewer, mobile');
}finally{await browser?.close();await ui.close();}
