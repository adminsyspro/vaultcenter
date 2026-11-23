import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi();let browser;
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const media=demoMedia();const [first,second,third]=media;
 Object.assign(first,{state:'writable',local_deleted:false,last_copy:{bucket:'vc-vtl-2026-10-06-ab12',generation:4,checked_at:'2026-10-06 21:00:00'}});
 Object.assign(second,{state:'writable',local_deleted:false,reconcile_pending:true,last_copy:null});
 let ready=false;const posts=[];let jobs=[];
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'t',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'csrf'}}));
 await page.route('**/v1/**',async r=>{
  const url=new URL(r.request().url());const path=url.pathname;const method=r.request().method();
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
  if(method==='POST'){assert.equal(r.request().headers()['x-csrf-token'],'csrf');posts.push({path,body:r.request().postData()?r.request().postDataJSON():null});}
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  if(path.endsWith('/storage/schedule'))return r.fulfill({json:{revision:1,rotation:'weekly',weekday:1,month_day:null,archive_after_days:null,archive_terms_accepted:false,rotation_supported:true,rotation_due:true,period_start:'2026-10-12',active_bucket:'vc-vtl-2026-10-06-ab12'}});
  if(path.endsWith('/client'))return r.fulfill({json:{schema:1,vtl_id:demoId,revision:0,client:'veeam',initiator:'10.0.0.5',editable:false,message:null}});
  if(path.endsWith('/maintenance/enter')){ready=true;return r.fulfill({json:{configured:true,phase:'maintenance',ready:true,client:'veeam'}});}
  if(path.endsWith('/maintenance'))return r.fulfill({json:{configured:true,phase:ready?'maintenance':'online',ready,client:'veeam',services:{'tgt.service':ready?'inactive':'active'}}});
  if(path.endsWith('/cartridges/deletion')){
   const body=method==='POST'?r.request().postDataJSON():null;
   return r.fulfill({json:{ready,pending:null,reason:ready?null:'Entrer en maintenance avant cette opération.',cartridges:body?.barcodes.map(b=>({barcode:b,exports:b===first.barcode?[{destination_id:'d1',bucket:'vc-vtl-2026-10-06-ab12',generation:4,checked_at:'2026-10-06 21:00:00'}]:[]}))}});
  }
  if(path.endsWith('/cartridges'))return r.fulfill({json:media});
  if(path.endsWith('/jobs'))return r.fulfill({json:jobs});
  return r.fulfill({json:{status:'ok',cartridges:media.length,jobs:0,media_root:'/demo'}});
 });
 await page.goto(ui.url+'/#media');
 // Details: one state line, the last S3 copy, the send action; history folded.
 await page.getByRole('button',{name:`Détails de ${first.barcode}`,exact:true}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog.getByRole('heading',{name:`Cartouche ${first.barcode}`,exact:true})).toBeVisible();
 await expect(dialog).toContainText('Inscriptible ·');
 await expect(dialog.locator('dl[aria-label="Détails de la cartouche"]')).toContainText('Copie S3vc-vtl-2026-10-06-ab12 · version 4');
 await expect(dialog.getByRole('figure')).toBeHidden();
 await dialog.getByText('Historique des envois',{exact:true}).click();
 await expect(dialog.getByRole('figure')).toBeVisible();
 await dialog.getByRole('button',{name:'Envoyer vers S3',exact:true}).click();
 // Send: cartridges as pills, the destination the schedule will create, Veeam wording.
 await expect(dialog.getByRole('heading',{name:'Envoyer vers S3',exact:true})).toBeVisible();
 await expect(dialog.getByRole('list',{name:'Cartouches concernées'}).getByRole('listitem')).toHaveCount(1);
 await expect(dialog).toContainText('Destination : nouveau bucket de la période, créé à l’envoi');
 await expect(dialog.getByRole('checkbox')).toHaveAccessibleName('Cette cartouche est terminée et déchargée dans Veeam.');
 await dialog.getByRole('button',{name:'Annuler',exact:true}).click();
 // A cartridge being synchronised is the only one held back, in one line.
 for(const item of [first,second])await page.getByRole('checkbox',{name:`Sélectionner ${item.barcode}`,exact:true}).check();
 await page.getByRole('button',{name:'Envoyer vers S3 (1)',exact:true}).click();
 await expect(dialog).toContainText(`${second.barcode} se synchronise avec le bucket. Son envoi sera possible à la fin de la synchronisation.`);
 await dialog.getByRole('button',{name:'Annuler',exact:true}).click();
 // Local deletion: one line per cartridge; maintenance from the same window, Veeam checkbox first.
 await page.getByRole('button',{name:'Supprimer (2)',exact:true}).click();
 const deletion=page.getByRole('dialog',{name:'Supprimer localement',exact:true});
 await expect(deletion).toContainText(`${first.barcode} · ✓ copie S3 · vc-vtl-2026-10-06-ab12 v4`);
 await expect(deletion).toContainText(`${second.barcode} · ⚠ aucune copie S3 : données perdues`);
 await expect(deletion).toContainText('Avant de passer en maintenance, attendez la fin des sauvegardes et retirez la cartouche du lecteur depuis Veeam.');
 await expect(deletion.getByRole('button',{name:'Supprimer',exact:true})).toBeDisabled();
 // Never confirm_idle on a bare click: the Veeam checkbox comes first.
 await expect(deletion.getByRole('checkbox')).toHaveCount(1);
 await expect(deletion.getByRole('button',{name:'Passer en maintenance',exact:true})).toBeDisabled();
 await deletion.getByRole('checkbox',{name:'Aucun job Veeam n’utilise cette bibliothèque',exact:true}).check();
 await deletion.getByRole('button',{name:'Passer en maintenance',exact:true}).click();
 await expect.poll(()=>posts.find(p=>p.path.endsWith('/maintenance/enter'))?.body).toEqual({confirm_idle:true});
 await expect(deletion.getByRole('button',{name:'Passer en maintenance',exact:true})).toHaveCount(0);
 await expect(deletion.getByRole('checkbox')).toHaveCount(1);
 await expect(deletion.getByRole('checkbox',{name:'Aucun job Veeam n’utilise cette bibliothèque',exact:true})).toHaveCount(0);
 await deletion.getByRole('checkbox').check();
 await expect(deletion.getByRole('button',{name:'Supprimer',exact:true})).toBeEnabled();
 assert.ok(!posts.some(p=>p.path.endsWith('/delete-local')));
 await page.setViewportSize({width:390,height:844});assert.ok(await deletion.evaluate(e=>e.getBoundingClientRect().right<=innerWidth));
 // Its own synchronisation is reported before "an operation is running", and only
 // cartridges otherwise ready to send are counted as synchronising.
 await page.keyboard.press('Escape');
 await page.setViewportSize({width:1440,height:1000});
 ready=false;
 Object.assign(third,{state:'failed',export_resumable:false,reconcile_pending:true});
 jobs=[{id:'sync',kind:'reconcile',subject:second.barcode,status:'running',progress:10,message:null,created_at:'2026-10-13 10:00:00',updated_at:'2026-10-13 10:00:00'}];
 await page.reload();
 await page.getByRole('button',{name:`Détails de ${second.barcode}`,exact:true}).click();
 await expect(dialog).toContainText('La cartouche se synchronise avec le bucket. L’envoi sera possible à la fin de la synchronisation.');
 await expect(dialog).not.toContainText('Une opération est en cours sur cette cartouche.');
 await page.keyboard.press('Escape');
 await expect(dialog).toHaveCount(0);
 for(const item of [first,second,third])await page.getByRole('checkbox',{name:`Sélectionner ${item.barcode}`,exact:true}).check();
 await page.getByRole('button',{name:'Envoyer vers S3 (1)',exact:true}).click();
 await expect(dialog).toContainText(`${second.barcode} se synchronise avec le bucket. Son envoi sera possible à la fin de la synchronisation.`);
 await expect(dialog).not.toContainText('se synchronisent');
 await expect(dialog).toContainText('La sélection exclut 1 cartouche(s), car leur état ne le permet pas ou une opération est déjà en cours.');
 assert.deepEqual(errors,[]);
 console.log('PASS cartridge windows: state and last-copy lines, folded history, send pills with the scheduled destination and Veeam wording, synchronisation line, deletion lines with inline maintenance.');
} finally {await browser?.close();await ui.close();}
