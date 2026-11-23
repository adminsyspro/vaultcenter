import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi(),browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM});
const id='00000000-0000-4000-8000-000000000002';
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1360,height:1000}});page.setDefaultTimeout(5000);
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 let binding={schema:1,vtl_id:id,revision:0,client:null,initiator:null,editable:true,message:null},conflict=false,libraryReads=0;const writes=[];
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'admin',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'csrf',local:true,oidc:false}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls'){libraryReads++;return r.fulfill({json:[{id,name:'VTL Veeam',lifecycle:'draft',provisioning_revision:1}]});}
  if(path===`/v1/vtls/${id}/client`) {
   if(r.request().method()==='PUT') {
    assert.equal(r.request().headers()['x-csrf-token'],'csrf');const input=r.request().postDataJSON();writes.push(input);
    if(conflict)return r.fulfill({status:409,json:{error:'Révision de l’association modifiée ; recharger avant d’enregistrer.'}});
    binding={...binding,revision:binding.revision+1,client:input.client,initiator:input.initiator};
   } else assert.equal(r.request().method(),'GET');
   return r.fulfill({json:binding});
  }
  if(path.endsWith('/pbs-connection'))return r.fulfill({json:{vtl_id:id,revision:0,client:'veeam',provisioning_revision:null,phase:'missing_prerequisite',pbs:null,host:null,port:3260,iqn:null,changer:null,drive:null,steps:[],message:'Associer le client de sauvegarde et préparer cette bibliothèque avant de générer les commandes.'}});
  if(path.endsWith('/bandwidth'))return r.fulfill({json:{vtl_id:id,revision:1,tape_write_bps:null,s3_bps:null,tape:{state:'stopped',observed_revision:null},s3:{state:'configured'},message:null}});
  if(path.endsWith('/provisioning'))return r.fulfill({json:{vtl_id:id,phase:'unconfigured',qualified:true}});
  if(path.endsWith('/settings/tape-storage'))return r.fulfill({json:{vtl_id:id,mode:'managed',phase:'ready',revision:1,path:'/srv/vaultcenter/tapes/veeam',capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null}});
  if(path.endsWith('/status'))return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  return r.fulfill({json:[]});
 });
 await page.goto(`${ui.url}/#vtls`);await page.getByRole('button',{name:'Ouvrir VTL Veeam',exact:true}).click();
 const dialog=page.getByRole('dialog'),section=dialog.getByRole('region',{name:'Client de sauvegarde',exact:true});
 // Draft: three ordered steps; activation waits for the backup server.
 await expect(dialog.getByRole('list',{name:'Mise en service'})).toBeVisible();
 await expect(dialog).toContainText('/srv/vaultcenter/tapes/veeam');
 await expect(dialog.getByRole('heading',{name:'Serveur de sauvegarde',exact:true})).toBeVisible();
 await expect(dialog.getByRole('button',{name:'Activer la bibliothèque',exact:true})).toBeDisabled();
 await expect(dialog).toContainText('Enregistrez d’abord l’adresse du serveur de sauvegarde.');
 for(const gone of ['Révision','Identifiant','Prérequis'])await expect(dialog).not.toContainText(gone);
 const field=section.getByLabel('Adresse IP du serveur Veeam',{exact:true});
 await expect(field).toHaveAttribute('placeholder','Ex. 192.168.1.50');await expect(field).toBeEditable();
 const save=section.getByRole('button',{name:'Enregistrer',exact:true});await expect(save).toBeDisabled();
 // A stale revision is reported and the typed address is kept.
 conflict=true;await field.fill(' 192.168.1.105 ');await save.click();
 await expect(section.getByRole('alert')).toContainText('Révision de l’association modifiée');await expect(field).toHaveValue(' 192.168.1.105 ');
 conflict=false;const readsBefore=libraryReads;await save.click();
 await expect.poll(()=>writes.length).toBe(2);
 assert.deepEqual(writes[1],{revision:0,client:'veeam',initiator:'192.168.1.105'});
 // A finished step collapses into one green line with « Modifier ».
 await expect(section).toContainText('192.168.1.105');await expect(section.getByRole('textbox')).toHaveCount(0);
 await expect(section.getByRole('button',{name:'Modifier',exact:true})).toBeVisible();await expect(section.getByRole('alert')).toHaveCount(0);
 // « Modifier » opens the field again and can be cancelled without saving.
 await section.getByRole('button',{name:'Modifier',exact:true}).click();await expect(field).toHaveValue('192.168.1.105');
 await section.getByRole('button',{name:'Annuler',exact:true}).click();await expect(section.getByRole('textbox')).toHaveCount(0);await expect(section).toContainText('192.168.1.105');assert.equal(writes.length,2);
 await expect.poll(()=>libraryReads).toBeGreaterThan(readsBefore);
 await expect(dialog.getByRole('heading',{name:'Serveur Veeam',exact:true})).toBeVisible();
 await expect(dialog.getByRole('button',{name:'Activer la bibliothèque',exact:true})).toBeEnabled();
 await page.keyboard.press('Escape');
 // Frozen Veeam binding: read only, no save button.
 binding={...binding,editable:false,message:'Association figée : la bibliothèque est déjà préparée.'};
 await page.getByRole('button',{name:'Ouvrir VTL Veeam',exact:true}).click();
 await expect(section).toContainText('192.168.1.105');await expect(section.getByRole('textbox')).toHaveCount(0);
 await expect(section.getByRole('button',{name:'Enregistrer',exact:true})).toHaveCount(0);await expect(section.getByRole('button',{name:'Modifier',exact:true})).toHaveCount(0);
 await page.keyboard.press('Escape');
 // PBS binding: managed by the appliance, nothing editable.
 binding={schema:1,vtl_id:id,revision:0,client:'pbs',initiator:'192.168.1.104',editable:false,message:'Association PBS gérée par la configuration de l’appliance.'};
 await page.getByRole('button',{name:'Ouvrir VTL Veeam',exact:true}).click();
 await expect(section).toContainText('192.168.1.104');await expect(dialog.getByRole('heading',{name:'Serveur PBS',exact:true})).toBeVisible();
 await expect(section.getByRole('textbox')).toHaveCount(0);await expect(section.getByRole('button')).toHaveCount(0);
 // Wrong VTL in the response is rejected.
 await page.keyboard.press('Escape');binding={...binding,vtl_id:'00000000-0000-4000-8000-000000000009'};
 await page.getByRole('button',{name:'Ouvrir VTL Veeam',exact:true}).click();
 await expect(section).toContainText('Association du client invalide ou associée à une autre VTL.');await expect(section).not.toContainText('192.168.1.104');
 assert.equal(writes.length,2);assert.deepEqual(errors,[]);
 console.log('PASS client binding UI: Veeam initiator save with CSRF and revision, conflict, frozen binding, read-only PBS binding and scope check');
} finally {await browser.close();await ui.close();}
