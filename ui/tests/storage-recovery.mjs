import {selectOption} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
const ui=await staticUi();let browser;
const screenshots=process.env.COLD_VTL_SCREENSHOT_DIR??'../.local/s3-copy-details-20261008/screenshots';await mkdir(screenshots,{recursive:true});
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1000}});
 let shortage=false;let many=false;let role='admin',partial=false,conflict=false,disconnected=false,archived=false,attached=false,configured=true,bootstrap=false,historicalOnly=false,creationPhase=null,connectionRevision=1,connectionSaved=false;let backgroundJobs=[];const discoveryRevisions=[];const writes=[];const errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 const job={id:'recovery-1',kind:'recovery',subject:'OLD001L8',status:'queued',progress:0,message:null,created_at:'2026-10-08',updated_at:'2026-10-08'};
 const buckets=[{id:'current',bucket:'vault-2026-10',active:true,created_at:'2026-10-01',configuration_revision:1,copy_count:0,observed_status:null,rotation_phase:null},{id:'historic',bucket:'vault-2026-09',active:false,created_at:'2026-09-01',configuration_revision:1,copy_count:1,observed_status:null,rotation_phase:null}];
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role,issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const req=route.request();const url=new URL(req.url());const path=url.pathname;
  if(req.method()!=='GET')writes.push({path,body:req.postDataJSON()});
  if(path==='/v1/vtls')return route.fulfill({json:[{id:demoId,name:'Bibliothèque vide',lifecycle:'ready',provisioning_revision:1,storage:configured?{provider:'s3_compatible',bucket:'vault-2026-10',prefix:'cold-vtl'}:null},{id:'target',name:'Bibliothèque cible PBS',lifecycle:'ready',provisioning_revision:1,storage:null}]});
  if(path.endsWith('/storage/schedule'))return route.fulfill({json:{revision:0,rotation:'manual',weekday:null,month_day:null,archive_after_days:null,archive_terms_accepted:false,rotation_supported:false,rotation_due:false,period_start:null,active_bucket:null}});
  if(path.endsWith('/settings/s3'))return route.fulfill({json:{providers:[],configured,revision:1,secret_configured:true,configuration:null,reconcile_required:0}});
  if(path.endsWith('/storage/buckets'))return route.fulfill({json:bootstrap?[]:historicalOnly?[buckets[1]]:buckets});
  // The connection falls back to the active destination, as on the appliance.
  if(path.endsWith('/storage/connection')){if(req.method()==='PUT'){connectionSaved=true;return route.fulfill({json:{status:'saved',revision:2}});}const saved=configured||connectionSaved;return route.fulfill({json:{revision:1,configured:saved,configuration:saved?{provider:'s3_compatible',endpoint:'https://s3.example.test',region:'region-1',prefix:'cold-vtl',access_key_id:'fixture-access'}:null,secret_configured:saved}});}
  if(path.endsWith('/settings/tape-storage'))return route.fulfill({json:{vtl_id:path.split('/')[3],mode:'managed',phase:'ready',revision:1,path:'/srv/tapes',capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null}});
  if(path.endsWith('/storage/buckets/discover')){discoveryRevisions.push(connectionRevision);return route.fulfill({json:{buckets:[{name:'older-2025-01',attached,active:false,destination_id:attached?'older':null}],complete:!partial,error:partial?'Accès de découverte refusé':null,revision:connectionRevision,suggested_bucket:'vc-vault-2026-11',creation_supported:true,creations:creationPhase?[{job_id:'uncertain-create',bucket:'vc-vault-2026-11',phase:creationPhase,message:null}]:[]}});}
  if(path.endsWith('/storage/buckets/attach')){attached=true;return route.fulfill({json:{status:'attached',destination_id:'older',active:false,revision:1}});}
  if(path.endsWith('/storage/buckets/create')){creationPhase='creating';backgroundJobs=[{...job,id:'uncertain-create',kind:'bucket_create',status:'running'}];return route.fulfill({status:202,json:{...job,kind:'bucket_create'}});}
  if(path.endsWith('/storage/recovery/check')&&shortage)return route.fulfill({json:{can_restore:false,conflict:false,local_identity:'absent',space_ok:false,free_slots:5,required_bytes:28*1024**3,available_bytes:13*1024**4,installation_required_bytes:28*1024**3+16*1024**2,download_available_bytes:20*1024**3,download_required_bytes:28*1024**3+16*1024**2,shared_volume:false,reason:'Espace insuffisant dans le dossier temporaire de téléchargement.'}});
  if(path.endsWith('/storage/recovery/check'))return route.fulfill({json:{can_restore:!conflict,conflict,local_identity:conflict?'conflict':'absent',space_ok:true,free_slots:5,required_bytes:2048,available_bytes:1000000,reason:null}});
  if(path.endsWith('/storage/recovery')&&req.method()==='POST')return route.fulfill({status:202,json:job});
  if(path.endsWith('/storage/recovery')){
   const id=url.searchParams.get('destination_id');
   if(partial&&id==='current')return route.fulfill({status:503,json:{error:'Bucket inaccessible'}});
   return route.fulfill({json:{destination_id:id,bucket:id==='historic'?'vault-2026-09':'vault-2026-10',complete:true,errors:[],access:{readable:!archived,writable:false,raw_status:archived?'Archived':null,supported:archived},copies:id==='current'&&many?Array.from({length:12},(_,i)=>({barcode:`NEW${String(i+1).padStart(3,'0')}L8`,generation:1,manifest_sha256:'b'.repeat(64),logical_size_bytes:1024**2,file_count:1,segment_count:1,format_version:2,manifest_key:`cold-vtl/cartridges/NEW${i}/manifests/1.json`})):id==='historic'?[{barcode:'OLD001L8',generation:3,manifest_sha256:archived?null:'a'.repeat(64),logical_size_bytes:archived?null:2048,file_count:archived?null:5,segment_count:archived?null:12,format_version:2,manifest_key:'cold-vtl/cartridges/OLD001L8/manifests/3.json'}]:[]}});
  }
  if(path.endsWith('/cartridges'))return route.fulfill({json:[]});
  if(path.endsWith('/jobs'))return route.fulfill({json:backgroundJobs});
  if(path.endsWith('/storage'))return route.fulfill({json:{destination_id:'historic',active:false,bucket:'vault-2026-09',prefix:'cold-vtl',region:'test',provider:'s3_compatible',supported:false,raw_status:null,state_label:'Disponible',object_access:'lecture',inventory:null,checks:[],catalogue_generations:[]}});
  if(path.endsWith('/status')&&disconnected)return route.fulfill({status:503,json:{error:'Disconnected fixture'}});
  return route.fulfill({json:{status:'ok',cartridges:0,jobs:0,media_root:'/fixture'}});
 });
 await page.goto(ui.url+'/#storage');
 const region=page.getByRole('region',{name:'Copies S3',exact:true});
 await expect(region.getByRole('rowheader',{name:'OLD001L8'})).toBeVisible();
 await expect(region).not.toContainText('Identique');
 const detailTitle='Copie S3 · OLD001L8';
 const copyRow=region.getByRole('row').filter({has:page.getByRole('rowheader',{name:'OLD001L8',exact:true})});
 for(const action of ['click','Enter','Space']){
  const before=writes.length;
  if(action==='click')await copyRow.getByRole('rowheader').click();else await copyRow.press(action);
  const detail=page.getByRole('dialog');
  await expect(detail.getByRole('heading',{name:detailTitle,exact:true})).toBeVisible();
  await expect(detail).toContainText('vault-2026-09');await expect(detail).toContainText('2 Kio');
  await expect(detail.getByText('Fichiers de la cartouche virtuelle',{exact:true})).toBeHidden();
  await expect(detail.getByText('Manifeste lu',{exact:true})).toBeVisible();
  await expect(detail.getByText('a'.repeat(64),{exact:true})).not.toBeVisible();
  if(action==='click'){
   await detail.getByText('Informations techniques',{exact:true}).click();
   await expect(detail.getByText('Fichiers de la cartouche virtuelle',{exact:true})).toBeVisible();
   await expect(detail.getByText('a'.repeat(64),{exact:true})).toBeVisible();
   await expect(detail).toContainText('cold-vtl/cartridges/OLD001L8/manifests/3.json');
  }
  assert.equal(writes.length,before,'Reading a copy must not start a recovery or a check');
  await page.keyboard.press('Escape');await expect(detail).toHaveCount(0);
 }
 await copyRow.click();await page.getByRole('dialog').getByRole('button',{name:'Réintégrer…',exact:true}).click();
 await expect(page.getByRole('dialog')).toHaveCount(1);
 await expect(page.getByRole('dialog').getByRole('heading',{name:'Réintégrer OLD001L8',exact:true})).toBeVisible();
 await page.getByRole('dialog').getByRole('button',{name:'Fermer',exact:true}).last().click();
 for(const width of [1440,390]){
  await page.setViewportSize({width,height:1000});await copyRow.getByRole('rowheader').click();
  const detail=page.getByRole('dialog');await expect(detail.getByRole('heading',{name:detailTitle,exact:true})).toBeVisible();
  const box=await detail.boundingBox();assert.ok(box.x>=0&&box.x+box.width<=width);
  await page.screenshot({path:`${screenshots}/copy-details-${width}.png`,fullPage:true});
  await page.keyboard.press('Escape');
 }
 await page.setViewportSize({width:1440,height:1000});
 await page.getByLabel('Rechercher une cartouche').fill('unknown');await expect(region).toContainText('Aucune copie ne correspond aux filtres.');
 await page.getByLabel('Rechercher une cartouche').fill('OLD001');await expect(region.getByRole('rowheader')).toHaveCount(1);
 await page.getByLabel('Rechercher une cartouche').fill('');
 // Each bucket is a block on its own surface: its state and totals once, then its copies.
 const blocks=region.getByRole('list',{name:'Buckets'}).getByRole('listitem');
 const historic=blocks.filter({hasText:'vault-2026-09'}),current=blocks.filter({hasText:'vault-2026-10'});
 await expect(historic).toContainText('1 copie · 2 Kio');await expect(current).toContainText('Aucune copie');
 await expect(historic.getByRole('rowheader',{name:'OLD001L8',exact:true})).toBeVisible();
 await expect(current.getByRole('table')).toHaveCount(0);
 for(const column of ['Bucket','Accès S3'])await expect(region.getByRole('columnheader',{name:column,exact:true})).toHaveCount(0);
 await expect(page.getByLabel('Bucket',{exact:true})).toHaveCount(0);
 await expect(historic.getByRole('row').filter({has:page.getByRole('rowheader')})).not.toContainText('Disponible');
 const surfaces=await historic.evaluate(el=>({block:getComputedStyle(el).backgroundColor,screen:getComputedStyle(el.closest('.lto-console-screen')).backgroundColor,search:getComputedStyle(document.querySelector('#storage-search')).backgroundColor}));
 assert.notEqual(surfaces.block,surfaces.screen,'a bucket block must stand out from the page');assert.equal(surfaces.search,surfaces.block);
 // A block folds; searching unfolds what matches.
 const fold=historic.getByRole('button',{name:'vault-2026-09',exact:true});
 await expect(fold).toHaveAttribute('aria-expanded','true');await fold.click();
 await expect(fold).toHaveAttribute('aria-expanded','false');await expect(region.getByRole('rowheader')).toHaveCount(0);
 await page.getByLabel('Rechercher une cartouche').fill('old001');await expect(region.getByRole('rowheader')).toHaveCount(1);await expect(current).toHaveCount(0);
 await page.getByLabel('Rechercher une cartouche').fill('');await fold.click();await expect(region.getByRole('rowheader')).toHaveCount(1);
 // Ten copies per page in a bucket.
 many=true;await region.getByRole('button',{name:'Actualiser le stockage'}).click();
 await expect(current).toContainText('12 copies · 12 Mio');await expect(current.getByRole('rowheader')).toHaveCount(10);await expect(current).toContainText('Page 1 sur 2');
 await current.getByRole('button',{name:'Page suivante des copies de vault-2026-10'}).click();await expect(current.getByRole('rowheader')).toHaveCount(2);await expect(current.getByRole('rowheader',{name:'NEW012L8'})).toBeVisible();
 await page.screenshot({path:'test-results/storage-recovery-blocks.png',fullPage:true});
 many=false;await region.getByRole('button',{name:'Actualiser le stockage'}).click();await expect(current).toContainText('Aucune copie');
 await page.screenshot({path:`${screenshots}/storage-recovery-desktop.png`,fullPage:true});
 // The target library is checked as soon as the window opens.
 shortage=true;await region.getByRole('button',{name:'Réintégrer',exact:true}).click();const dialog=page.getByRole('dialog');
 await expect(dialog).toContainText('Espace insuffisant dans le dossier temporaire de téléchargement.');
 await expect(dialog).toContainText('Téléchargement temporaire');await expect(dialog).toContainText('20 Gio');await expect(dialog).toContainText('13 Tio');
 await expect(dialog.getByRole('button',{name:'Réintégrer',exact:true})).toBeDisabled();
 await expect(dialog.getByRole('button',{name:'Vérifier la destination'})).toHaveCount(0);
 await page.screenshot({path:`${screenshots}/recovery-space-error.png`,fullPage:true});
 await dialog.getByRole('button',{name:'Fermer',exact:true}).last().click();shortage=false;
 await region.getByRole('button',{name:'Réintégrer',exact:true}).click();
 const targetSelect=dialog.getByLabel('Bibliothèque cible');
 for(const [width,theme] of [[1440,'light'],[390,'dark']]){
  await page.setViewportSize({width,height:844});await page.evaluate(theme=>document.documentElement.classList.toggle('dark',theme==='dark'),theme);
  await targetSelect.click();const list=page.getByRole('listbox');await expect(list).toBeVisible();
  const box=await list.boundingBox();assert.ok(box.x>=0&&box.x+box.width<=width&&box.y+box.height<=844);
  await page.screenshot({path:`${screenshots}/select-modal-${theme}-${width}.png`,fullPage:false});
  await page.keyboard.press('Escape');await expect(dialog).toBeVisible();await expect(targetSelect).toBeFocused();
 }
 await page.setViewportSize({width:1440,height:1000});
 const checks=()=>writes.filter(value=>value.path.endsWith('/storage/recovery/check')).length;const before=checks();
 // Changing the library checks it again.
 await selectOption(targetSelect, 'target');
 await expect.poll(checks).toBe(before+1);await expect(dialog).toContainText('Contrôles réussis');
 await page.screenshot({path:`${screenshots}/recovery-check-success.png`,fullPage:true});
 await page.setViewportSize({width:390,height:844});await page.screenshot({path:`${screenshots}/recovery-check-success-mobile.png`,fullPage:true});await page.setViewportSize({width:1440,height:1000});
 await dialog.getByRole('button',{name:'Réintégrer',exact:true}).click();await expect(dialog).toHaveCount(0);
 assert.equal(writes.find(value=>value.path.endsWith('/storage/recovery')).body.target_vtl_id,'target');assert.equal(writes.find(value=>value.path.endsWith('/storage/recovery')).body.replace,false);
 // Existing barcode: one frame with the real risk, one checkbox, nothing to retype.
 conflict=true;await region.getByRole('button',{name:'Réintégrer',exact:true}).click();
 await expect(dialog).toContainText('OLD001L8 existe déjà dans Bibliothèque vide. La cartouche actuelle sera sauvegardée, puis remplacée. Vous pourrez revenir en arrière depuis la maintenance.');
 await expect(dialog.getByRole('textbox')).toHaveCount(0);
 const replace=dialog.getByRole('button',{name:'Remplacer',exact:true});await expect(replace).toBeDisabled();
 await dialog.getByLabel('Je confirme le remplacement de OLD001L8.').check();
 await page.screenshot({path:`${screenshots}/storage-recovery-conflict.png`,fullPage:true});await replace.click();assert.equal(writes.filter(value=>value.path.endsWith('/storage/recovery')).at(-1).body.replace,true);
 partial=true;await region.getByRole('button',{name:'Actualiser le stockage'}).click();await expect(region).toContainText('Inventaire partiel');await expect(region.getByRole('rowheader',{name:'OLD001L8'})).toBeVisible();
 // Bucket operations live in the VTL window (Stockage › S3).
 await expect(region.getByRole('button',{name:'Gérer les buckets'})).toHaveCount(0);await expect(page.getByRole('button',{name:'Configurer le stockage de Bibliothèque vide',exact:true})).toHaveCount(0);
 // The active bucket already holds a copy, so step ② offers « Changer ».
 buckets[0].copy_count=1;
 await page.goto(ui.url+`/#vtls?vtl=${demoId}&storage=s3`);const storageDialog=page.getByRole('dialog',{name:'Bibliothèque vide',exact:true});const destination=storageDialog.getByRole('region',{name:'Bucket des exports',exact:true});const attach=storageDialog.getByRole('region',{name:'Utiliser un bucket existant',exact:true});
 await expect(destination).toContainText('Bucket actif : vault-2026-10');await storageDialog.getByText('Options avancées',{exact:true}).click();
 await expect(attach).toContainText('La liste des buckets est incomplète.');await attach.getByLabel('Bucket existant').fill('older');await page.getByRole('option',{name:'older-2025-01'}).click();await expect(attach.getByLabel('Bucket existant')).toHaveValue('older-2025-01');await attach.getByRole('button',{name:'Rattacher un ancien bucket',exact:true}).click();await expect(attach).toContainText('Bucket rattaché. Ses copies apparaissent dans Copies S3.');
 await expect(destination.getByLabel('Prochain bucket')).toHaveValue('vault-2026-11');await expect(destination.getByRole('checkbox')).toHaveCount(0);await destination.getByRole('button',{name:'Changer',exact:true}).click();await expect(destination).toContainText('Création de vc-vault-2026-11 en cours.');await expect.poll(()=>writes.filter(value=>value.path.endsWith('/storage/buckets/create')).at(-1)?.body.bucket).toBe('vc-vault-2026-11');
 creationPhase='uncertain';backgroundJobs[0]={...backgroundJobs[0],message:'Vérification de la création en cours. Le fournisseur n’a pas encore répondu. VaultCenter réessaiera dans 5 minutes.'};
 await expect(destination).toContainText('Vérification de la création en cours.',{timeout:20000});await expect(destination.getByRole('button',{name:'Changer',exact:true})).toBeDisabled();await expect(storageDialog.getByRole('button',{name:'Abandonner cette tentative'})).toHaveCount(0);
 connectionRevision=2;creationPhase=null;backgroundJobs=[{...job,id:'completed-create',kind:'bucket_create',status:'completed'}];await expect.poll(()=>discoveryRevisions.at(-1),{timeout:20000}).toBe(2);await attach.getByLabel('Bucket existant').fill('after-rotation');await attach.getByRole('button',{name:'Rattacher un ancien bucket',exact:true}).click();await expect.poll(()=>writes.filter(value=>value.path.endsWith('/storage/buckets/attach')).at(-1)?.body.revision).toBe(2);
 await page.setViewportSize({width:390,height:844});await page.screenshot({path:`${screenshots}/vtl-s3-buckets-mobile.png`,fullPage:false});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);await page.keyboard.press('Escape');await expect(storageDialog).toHaveCount(0);
 await page.goto(ui.url+'/#storage');await page.setViewportSize({width:390,height:844});await region.getByLabel('Rechercher une cartouche').scrollIntoViewIfNeeded();await page.screenshot({path:`${screenshots}/storage-recovery-mobile.png`,fullPage:true});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
 role='viewer';partial=false;await page.reload();await expect(region.getByRole('button',{name:'Réintégrer',exact:true})).toBeDisabled();await expect(region.getByRole('button',{name:'Gérer les buckets'})).toHaveCount(0);await expect(page.getByRole('button',{name:'Configurer le stockage de Bibliothèque vide',exact:true})).toHaveCount(0);
 await copyRow.getByRole('rowheader').click();await expect(page.getByRole('dialog').getByRole('button',{name:'Réintégrer…',exact:true})).toBeDisabled();await page.keyboard.press('Escape');
 archived=true;await region.getByRole('button',{name:'Actualiser le stockage'}).click();await expect(region.getByRole('button',{name:'Réintégrer',exact:true})).toHaveCount(0);await expect(copyRow.getByRole('button',{name:'Rappeler le bucket'})).toBeVisible();await expect(region).toContainText('Archivé · lecture seule');await copyRow.getByRole('rowheader').click();await expect(page.getByRole('dialog')).toContainText('Non confirmé');await expect(page.getByRole('dialog').getByRole('button',{name:'Rappeler le bucket',exact:true})).toBeVisible();await page.keyboard.press('Escape');
 archived=false;role='admin';await page.reload();await expect(region.getByRole('button',{name:'Réintégrer',exact:true})).toBeEnabled();disconnected=true;await expect(region.getByRole('button',{name:'Réintégrer',exact:true})).toBeDisabled({timeout:20000});
 disconnected=false;configured=false;bootstrap=true;await page.goto(ui.url+'/#storage');await page.reload();
 // Without connection, the S3 page points to the library's Stockage dialog instead of a configuration tab.
 await expect(page.getByText('Stockage S3 non configuré',{exact:true})).toBeVisible();await expect(page.getByRole('button',{name:'Configuration',exact:true})).toHaveCount(0);
 await page.getByRole('button',{name:'Configurer le stockage de Bibliothèque vide',exact:true}).click();await expect(storageDialog.getByRole('button',{name:'S3',exact:true})).toHaveAttribute('aria-pressed','true');
 await expect(destination).toContainText('Enregistrez d’abord la connexion S3.');
 const s3connection=storageDialog.getByRole('region',{name:'Connexion S3',exact:true});await selectOption(s3connection.getByLabel('Fournisseur',{exact:true}),'s3_compatible');await s3connection.getByLabel('Endpoint HTTPS',{exact:true}).fill('https://s3.example.test');await s3connection.getByLabel('Région S3',{exact:true}).fill('region-1');await s3connection.getByLabel('Access key',{exact:true}).fill('fixture-access');await s3connection.getByLabel('Secret key',{exact:true}).fill('fixture-secret');await s3connection.getByRole('button',{name:'Vérifier et enregistrer'}).click();await expect(s3connection).toContainText('fixt…cess');
 await expect(destination.getByLabel('Nom du bucket')).toBeVisible();await storageDialog.getByText('Options avancées',{exact:true}).click();await expect(attach.getByLabel('Bucket existant')).toBeVisible();
 await page.keyboard.press('Escape');await page.goto(ui.url+'/#storage');await expect(page.getByText('Aucune destination S3 active',{exact:true})).toBeVisible();await expect(region).toContainText('Aucune copie S3 repérée.');
 bootstrap=false;historicalOnly=true;configured=true;archived=true;await page.goto(ui.url+'/#storage');await page.reload();await expect(region.getByRole('rowheader',{name:'OLD001L8'})).toBeVisible();await copyRow.getByRole('button',{name:'Rappeler le bucket',exact:true}).click();await expect(page.getByRole('dialog')).toBeVisible();await expect(page.getByText('Ce bucket n’appartient pas à la VTL sélectionnée.',{exact:true})).toHaveCount(0);await page.getByRole('dialog').getByRole('button',{name:'Fermer',exact:true}).last().click();
 assert.deepEqual(errors,[]);console.log('PASS S3 recovery: empty local library, historical copies, filters, target check, explicit conflict replacement, partial inventory, attach, monthly create, archived, viewer, disconnected, mobile.');
} finally {await browser?.close();await ui.close();}
