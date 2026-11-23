import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
const ui=await staticUi(),browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM});
const id='00000000-0000-4000-8000-000000000001';
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1360,height:1000}});page.setDefaultTimeout(5000);
 let host=null,revision=0,defer=false,pending=null,client='pbs';const writes=[];
 await page.addInitScript(()=>Object.defineProperty(navigator,'clipboard',{value:{writeText:()=>Promise.reject(new Error('HTTP clipboard unavailable'))},configurable:true}));
 await page.route('**/auth/**',r=>r.fulfill({json:{user:{id:'admin',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'csrf',local:true,oidc:false}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id,name:'VTL A',lifecycle:'ready',provisioning_revision:2}]});
  if(path.endsWith('/pbs-connection')) {
   if(defer&&r.request().method()==='GET'){pending=r;return;}
   if(r.request().method()==='PUT') {const input=r.request().postDataJSON();writes.push(input);assert.equal(r.request().headers()['x-csrf-token'],'csrf');assert.equal(input.revision,revision);host=input.host;revision++;}
   if(client==='veeam')return r.fulfill({json:{vtl_id:id,revision,client,provisioning_revision:2,phase:'ready',pbs:null,host,port:3260,iqn:'iqn.test:vtl',changer:{name:null},drive:{name:null},steps:[{id:'iscsi',title:'Connecter la cible iSCSI',explanation:'À exécuter dans PowerShell en administrateur sur le serveur Veeam (tape server). Une session existante reste connectée.',command:'Start-Service MSiSCSI'},{id:'devices',title:'Vérifier le changeur et le lecteur',explanation:'Le changeur HP MSL G3 et le lecteur HPE Ultrium 8 doivent apparaître. Le lecteur nécessite le pilote HPE StoreEver.',command:'Get-PnpDevice -Class TapeDrive'}],message:null}});
   return r.fulfill({json:{vtl_id:id,revision,client,provisioning_revision:2,phase:host?'ready':'missing_prerequisite',pbs:'root@192.0.2.2',host,port:3260,iqn:'iqn.test:vtl',changer:{name:'changer'},drive:{name:'drive'},steps:host?[{id:'verify',title:'Vérifier l’inventaire',explanation:'À exécuter sur PBS.',command:'proxmox-tape changer status changer --output-format json-pretty'}]:[],message:host?null:'Indiquer l’adresse iSCSI de VaultCenter.'}});
  }
  if(path.endsWith('/client'))return r.fulfill({json:{schema:1,vtl_id:id,revision:0,client,initiator:'192.0.2.2',editable:false,message:null}});
  if(path.endsWith('/bandwidth'))return r.fulfill({json:{vtl_id:id,revision:1,tape_write_bps:null,s3_bps:null,tape:{state:'stopped',observed_revision:null},s3:{state:'configured'},message:null}});
  if(path.endsWith('/provisioning'))return r.fulfill({json:{vtl_id:id,phase:'active',qualified:true}});
  if(path.endsWith('/status'))return r.fulfill({json:{status:'ok',cartridges:0,jobs:0}});
  return r.fulfill({json:[]});
 });
 await page.goto(`${ui.url}/#vtls`);await page.getByRole('button',{name:'Ouvrir VTL A',exact:true}).click();
 const modal=page.getByRole('dialog',{name:'VTL A',exact:true});await modal.getByRole('button',{name:'Connexion',exact:true}).click();await expect(modal).toContainText('root@192.0.2.2');await expect(modal).toContainText('Exécutez ces commandes en root sur le PBS associé à cette VTL.');
 await modal.getByLabel('Adresse iSCSI de VaultCenter',{exact:true}).fill('192.168.1.110');
 await modal.getByRole('button',{name:'Enregistrer le portail',exact:true}).click();
 await expect(modal.getByText('proxmox-tape changer status changer --output-format json-pretty',{exact:true})).toBeVisible();
 assert.deepEqual(writes,[{revision:0,host:'192.168.1.110',port:3260}]);
 // HTTP: no Clipboard API, the in-dialog fallback copies the exact command.
 await page.evaluate(()=>{window.__copied=null;document.execCommand=c=>{const f=document.activeElement;window.__copied=c==='copy'&&f instanceof HTMLTextAreaElement?f.value:null;return window.__copied!==null;};});
 const copyButton=modal.getByRole('button',{name:'Copier Vérifier l’inventaire',exact:true});await copyButton.click();
 await expect(copyButton).toContainText('Copié');assert.equal(await page.evaluate(()=>window.__copied),'proxmox-tape changer status changer --output-format json-pretty');
 await expect(copyButton).toBeFocused();await expect(modal.locator('textarea')).toHaveCount(0);
 await page.evaluate(()=>{document.execCommand=()=>false;});await copyButton.click();await expect(modal).toContainText('Sélectionnez');
 await mkdir('../.local/bandwidth-20261005/ui',{recursive:true});await page.screenshot({path:'../.local/bandwidth-20261005/ui/pbs-desktop.png',fullPage:true});
 await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);await page.screenshot({path:'../.local/bandwidth-20261005/ui/pbs-mobile.png',fullPage:true});
 assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
// Reopening the tab reads the procedure again; a failed read keeps every action disabled.
 await modal.getByRole('button',{name:'Débit',exact:true}).click();
 defer=true;await modal.getByRole('button',{name:'Connexion',exact:true}).click();await expect.poll(()=>pending!==null).toBe(true);
 await expect(modal.getByLabel('Adresse iSCSI de VaultCenter',{exact:true})).toBeDisabled();
 await expect(modal.getByRole('button',{name:'Copier Vérifier l’inventaire',exact:true})).toHaveCount(0);
 await pending.fulfill({status:503,json:{error:'Lecture indisponible'}});
 await expect(modal).toContainText('Lecture indisponible');
 await expect(modal.getByRole('button',{name:'Enregistrer le portail',exact:true})).toBeDisabled();
 await expect(modal.getByRole('button',{name:'Copier Vérifier l’inventaire',exact:true})).toHaveCount(0);
 await page.keyboard.press('Escape');defer=false;client='veeam';
 await page.getByRole('button',{name:'Ouvrir VTL A',exact:true}).click();await modal.getByRole('button',{name:'Connexion',exact:true}).click();
 await expect(modal).toContainText('Exécutez ces commandes dans PowerShell en administrateur sur le serveur Veeam.');
 await expect(modal.getByText('Get-PnpDevice -Class TapeDrive',{exact:true})).toBeVisible();
 await expect(modal).not.toContainText('PBS associé');await expect(modal).not.toContainText('root@');await expect(modal).not.toContainText('Exécutez ces commandes en root');
 await expect(modal.getByRole('button',{name:'Copier Connecter la cible iSCSI',exact:true})).toBeVisible();
 console.log('PBS connection UI passed: portal, scope, generated steps, HTTP clipboard fallback, keyboard and Veeam PowerShell variant');
} finally {await browser.close();await ui.close();}
