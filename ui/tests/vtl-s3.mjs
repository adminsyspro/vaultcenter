import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { chromium, expect } from '@playwright/test';
const browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:['--no-sandbox']});
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1400,height:1000}});
 const errors=[]; page.on('pageerror',e=>errors.push(e.message));
 const ids=['00000000-0000-4000-8000-000000000003','00000000-0000-4000-8000-000000000004'];
 const libraries=ids.map((id,i)=>({id,name:i?'Third':'Second',lifecycle:'ready',provisioning_revision:1,storage:null}));
 const providers=[{id:'ovh_cold_archive_v1',label:'OVHcloud Cold Archive v1',default_endpoint:'https://s3.rbx-archive.io.cloud.ovh.net',default_region:'rbx-archive',description:'',managed_archive:true},{id:'s3_compatible',label:'S3 compatible',default_endpoint:'',default_region:'',description:'',managed_archive:false}];
 const connections={},writes=[],jobs={},creations={},schedules={};let credentialFailure=false,jobsFailure=false,keys='ok',drift=0,holdBuckets=null,bucketsFailure=false;const copies={};
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'admin',role:'admin',issuer:null,disabled:false},csrf_token:'csrf-test'}}));
 await page.route('**/v1/**',async route=>{
  const path=new URL(route.request().url()).pathname;const method=route.request().method();
  if(path==='/v1/vtls')return route.fulfill({json:libraries});
  const id=path.split('/')[3];
  if(method!=='GET'){assert.equal(route.request().headers()['x-csrf-token'],'csrf-test');writes.push({id,path:path.split('/').slice(4).join('/'),body:route.request().postDataJSON()});}
  if(path.endsWith('/telemetry'))return route.fulfill({json:{version:1,vtl_id:id,generated_at:null,observed_at:null,source:'mhvtl',status:'unavailable',reason:'not_configured',drives:[],slots:[],media:[]}});
  if(path.endsWith('/settings/s3'))return route.fulfill({json:{providers,configured:false,revision:0,secret_configured:false,configuration:null,reconcile_required:0}});
  if(path.endsWith('/storage/connection')){
   if(method==='GET'){const value=connections[id];return route.fulfill({json:{revision:value?1:0,configured:!!value,secret_configured:!!value,configuration:value??null,credentials:value?{status:keys,checked_at:'2026-10-13T08:00:00Z'}:null}});}
   if(credentialFailure)return route.fulfill({status:400,json:{error:'Identifiants refusés par le fournisseur : vérifiez l’access key et la secret key (l’access key n’est pas l’identifiant de l’utilisateur).'}});
   const {secret_access_key,revision,...value}=route.request().postDataJSON();connections[id]=value;return route.fulfill({json:{status:'saved',revision:1,warning:null}});
  }
  if(path.endsWith('/storage/buckets/discover'))return route.fulfill({json:{buckets:[{name:'legacy-third',attached:false,active:false,destination_id:null}],complete:true,error:null,revision:1,suggested_bucket:drift?`vc-third-2026-10-13-${++drift}`:'vc-third-2026-10-13',creation_supported:connections[id]?.provider==='ovh_cold_archive_v1',creations:creations[id]??[]}});
  if(path.endsWith('/storage/buckets/create')){const body=route.request().postDataJSON();creations[id]=[{job_id:'create-1',bucket:body.bucket,phase:'creating',message:null}];jobs[id]=[{id:'create-1',kind:'bucket_create',subject:body.bucket,status:'running',progress:10,message:null,created_at:'2026-10-13',updated_at:'2026-10-13'}];return route.fulfill({status:202,json:jobs[id][0]});}
  if(path.endsWith('/storage/buckets')){await holdBuckets;if(bucketsFailure)return route.fulfill({status:500,json:{error:'Destinations indisponibles pour le moment.'}});const active=libraries.find(l=>l.id===id).storage?.bucket;return route.fulfill({json:active?[{id:'d-'+active,bucket:active,active:true,created_at:'2026-10-13',configuration_revision:1,copy_count:copies[active]??0,observed_status:null,rotation_phase:null}]:[]});}
  if(path.endsWith('/storage/buckets/attach'))return route.fulfill({json:{status:'activated',destination_id:'existing',revision:2,active:true}});
  if(path.endsWith('/storage/schedule')){
   const saved=schedules[id]??{revision:0,rotation:'manual',weekday:null,month_day:null,archive_after_days:null,archive_terms_accepted:false};
   if(method==='PUT'){const body=route.request().postDataJSON();if(body.archive_after_days&&!body.accept_archive_terms&&!saved.archive_terms_accepted)return route.fulfill({status:400,json:{error:'Acceptez les conditions OVH pour activer l’archivage automatique.'}});schedules[id]={revision:saved.revision+1,rotation:body.rotation,weekday:body.weekday??null,month_day:body.month_day??null,archive_after_days:body.archive_after_days??null,archive_terms_accepted:saved.archive_terms_accepted||!!body.accept_archive_terms};}
   return route.fulfill({json:{...(schedules[id]??saved),rotation_supported:connections[id]?.provider==='ovh_cold_archive_v1',rotation_due:false,period_start:null,active_bucket:libraries.find(l=>l.id===id).storage?.bucket??null}});
  }
  // The header bell lists every VTL, so its route is the one global read.
  if(path==='/v1/activity')return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  const scoped=path.replace(/^\/v1\/vtls\/[^/]+(?=\/)/,'/v1');
  if(scoped==='/v1/jobs'&&jobsFailure)return route.fulfill({status:500,json:{error:'Suivi indisponible pour le moment.'}});
  if(scoped==='/v1/jobs'){if(drift)jobs[id]=(jobs[id]??[]).map(job=>job.kind==='bucket_create'?{...job,message:`Bucket créé et activé (${drift}).`}:job);return route.fulfill({json:jobs[id]??[]});}
  const data={'/v1/status':{status:'ok',cartridges:0,jobs:0,media_root:'/test'},'/v1/cartridges':[],'/v1/cartridges/options':{storage:null},'/v1/maintenance':{configured:false},'/v1/maintenance/events':[],'/v1/reintegrations':{configured:false,operations:[],eligible:[],pending:null},'/v1/client':{schema:1,vtl_id:id,revision:0,client:'pbs',initiator:'10.0.0.1',editable:false,message:null},'/v1/provisioning':{vtl_id:id,phase:'active'},'/v1/settings/tape-storage':{vtl_id:id,mode:'managed',phase:'ready',revision:1,path:'/srv/tapes',capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null}};
  assert.ok(scoped in data,`Unexpected route ${path}`);return route.fulfill({json:data[scoped]});
 });
 const base=process.env.COLD_VTL_UI_URL||'http://localhost:3000';
 await page.goto(`${base}/#vtls?vtl=${ids[1]}&tab=storage&storage=s3`);
 const dialog=page.getByRole('dialog',{name:'Third',exact:true});
 await expect(dialog.getByRole('button',{name:'S3',exact:true})).toHaveAttribute('aria-pressed','true');
 const connection=dialog.getByRole('region',{name:'Connexion S3',exact:true});
 const bucketStep=dialog.getByRole('region',{name:'Bucket des exports',exact:true});
 await expect(bucketStep).toContainText('Enregistrez d’abord la connexion S3.');
 // ① Provider defaults fill the endpoint; refused keys save nothing and say why in one red line.
 await expect(connection.getByText('https://s3.rbx-archive.io.cloud.ovh.net · rbx-archive')).toBeVisible();
 await expect(connection.getByRole('button',{name:'Annuler',exact:true})).toHaveCount(0);
 await connection.getByRole('button',{name:'Modifier l’endpoint',exact:true}).click();await expect(connection.getByLabel('Endpoint HTTPS',{exact:true})).toHaveValue('https://s3.rbx-archive.io.cloud.ovh.net');await expect(connection.getByLabel('Région S3',{exact:true})).toHaveValue('rbx-archive');
 await connection.getByLabel('Access key',{exact:true}).fill('da7b0000111122223333d8cd');
 await connection.getByLabel('Secret key',{exact:true}).fill('secret-third');
 credentialFailure=true;await connection.getByRole('button',{name:'Vérifier et enregistrer',exact:true}).click();
 await expect(connection.getByRole('alert')).toContainText('Identifiants refusés par le fournisseur');
 assert.equal(connections[ids[1]],undefined);
 credentialFailure=false;await connection.getByRole('button',{name:'Vérifier et enregistrer',exact:true}).click();
 await expect(connection).toContainText('da7b…d8cd');
 // Keys accepted by OVH: one green line, nothing else to read.
 await expect(connection.getByRole('alert')).toHaveCount(0);await expect(connection.getByRole('status')).toHaveCount(0);
 assert.deepEqual(await page.evaluate(()=>JSON.stringify([{...localStorage},{...sessionStorage}]).includes('secret-third')),false);
 await connection.getByRole('button',{name:'Modifier',exact:true}).click();await connection.getByRole('button',{name:'Annuler',exact:true}).click();await expect(connection.getByLabel('Secret key',{exact:true})).toHaveCount(0);await expect(connection.getByRole('button',{name:'Modifier',exact:true})).toBeVisible();
 assert.deepEqual(writes.at(-1),{id:ids[1],path:'storage/connection',body:{provider:'ovh_cold_archive_v1',endpoint:'https://s3.rbx-archive.io.cloud.ovh.net',region:'rbx-archive',prefix:'cold-vtl',access_key_id:'da7b0000111122223333d8cd',revision:0,secret_access_key:'secret-third'}});
 // ② The proposed name follows the fixed vc- prefix; « Créer » sits in the field, without a checkbox.
 await expect(bucketStep.getByLabel('Nom du bucket',{exact:true})).toHaveValue('third-2026-10-13');
 await expect(bucketStep.getByRole('checkbox')).toHaveCount(0);
 {const field=await bucketStep.getByLabel('Nom du bucket',{exact:true}).boundingBox(),button=await bucketStep.getByRole('button',{name:'Créer',exact:true}).boundingBox();assert.ok(button.x>=field.x&&button.y>=field.y&&button.x+button.width<=field.x+field.width&&button.y+button.height<=field.y+field.height,'Créer lies inside the field');}
 await mkdir('test-results',{recursive:true});await page.setViewportSize({width:390,height:844});await bucketStep.scrollIntoViewIfNeeded();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);await page.screenshot({path:'test-results/vtl-s3-steps-390.png'});await page.setViewportSize({width:1400,height:1000});
 await bucketStep.getByRole('button',{name:'Créer',exact:true}).click();
 assert.deepEqual(writes.at(-1),{id:ids[1],path:'storage/buckets/create',body:{revision:1,bucket:'vc-third-2026-10-13'}});
 await expect(bucketStep).toContainText('Création de vc-third-2026-10-13 en cours.');
 // A lost provider answer is checked by the application: one line, nothing to retype.
 creations[ids[1]][0].phase='uncertain';jobs[ids[1]][0]={...jobs[ids[1]][0],message:'Vérification de la création en cours. Le fournisseur n’a pas encore répondu. VaultCenter réessaiera dans 5 minutes.'};
 await expect(bucketStep).toContainText('Vérification de la création en cours.',{timeout:15000});
 await expect(bucketStep.getByRole('button',{name:'Créer',exact:true})).toBeDisabled();
 // The bucket exists with data: never adopted, « Utiliser un autre nom ».
 creations[ids[1]][0].phase='occupied';jobs[ids[1]][0]={...jobs[ids[1]][0],status:'failed'};
 await expect(bucketStep.getByRole('alert')).toContainText('Le bucket vc-third-2026-10-13 existe déjà et contient des données.',{timeout:15000});
 await bucketStep.getByRole('button',{name:'Utiliser un autre nom',exact:true}).click();
 await expect(bucketStep.getByRole('alert')).toHaveCount(0);
 await bucketStep.getByRole('button',{name:'Créer',exact:true}).click();
 // Success: one line names the bucket just created; an empty active bucket offers no « Changer ».
 let releaseBuckets;holdBuckets=new Promise(resolve=>releaseBuckets=resolve);
 creations[ids[1]]=[{job_id:'create-2',bucket:'vc-third-2026-10-13',phase:'switched',message:'Bucket créé et activé.'}];jobs[ids[1]]=[{...jobs[ids[1]][0],id:'create-2',status:'completed'}];
 libraries[1].storage={provider:'ovh_cold_archive_v1',bucket:'vc-third-2026-10-13',prefix:'cold-vtl'};
 await expect(bucketStep).toContainText('Bucket actif : vc-third-2026-10-13',{timeout:15000});
 // Until its copies are counted, the new active bucket cannot be changed.
 await expect(bucketStep).toContainText('Lecture des copies du bucket…');
 await expect(bucketStep.getByRole('button',{name:'Changer',exact:true})).toHaveCount(0);await expect(bucketStep.getByLabel('Prochain bucket',{exact:true})).toHaveCount(0);
 releaseBuckets();holdBuckets=null;await expect(bucketStep).not.toContainText('Lecture des copies du bucket…');
 await expect(bucketStep.getByRole('status').filter({hasText:'Le bucket vc-third-2026-10-13 a été créé et activé.'})).toBeVisible();
 await expect(bucketStep).toContainText('Le bucket actif est encore vide : les prochains exports iront dedans.');
 await expect(bucketStep.getByRole('button',{name:'Changer',exact:true})).toHaveCount(0);await expect(bucketStep.getByLabel('Prochain bucket',{exact:true})).toHaveCount(0);
 // A failed count is unknown, not empty: the error is shown instead of the empty sentence.
 bucketsFailure=true;jobs[ids[1]]=[...jobs[ids[1]],{id:'export-0',kind:'export',subject:'TAPE01',status:'completed',progress:100,message:null,created_at:'2026-10-13',updated_at:'2026-10-13'}];
 await expect(bucketStep.getByRole('alert')).toContainText('Destinations indisponibles pour le moment.',{timeout:15000});
 await expect(bucketStep).not.toContainText('Le bucket actif est encore vide');bucketsFailure=false;
 // The first copy lands in it: « Changer » comes back.
 copies['vc-third-2026-10-13']=2;jobs[ids[1]]=[...jobs[ids[1]],{id:'export-1',kind:'export',subject:'TAPE01',status:'completed',progress:100,message:null,created_at:'2026-10-13',updated_at:'2026-10-13'}];
 await expect(bucketStep.getByRole('button',{name:'Changer',exact:true})).toBeVisible({timeout:15000});
 await expect(bucketStep).not.toContainText('Le bucket actif est encore vide');await expect(bucketStep).not.toContainText('Destinations indisponibles pour le moment.',{timeout:15000});
 await expect(dialog.getByRole('region',{name:'Prêt à exporter',exact:true})).toContainText('Prêt à exporter vers vc-third-2026-10-13');
 // Schedule: every Monday, then automatic archiving once the OVH conditions are accepted.
 await bucketStep.getByLabel('Chaque semaine, le').check();
 await expect.poll(()=>schedules[ids[1]]?.rotation).toBe('weekly');assert.equal(schedules[ids[1]].weekday,1);
 await bucketStep.getByLabel('Chaque mois, le').check();
 await expect.poll(()=>schedules[ids[1]]?.rotation).toBe('monthly');assert.equal(schedules[ids[1]].month_day,1);assert.equal(schedules[ids[1]].weekday,null);
 await bucketStep.getByLabel('Chaque semaine, le').check();await expect.poll(()=>schedules[ids[1]]?.rotation).toBe('weekly');
 await bucketStep.getByLabel('Archiver automatiquement').check();
 await expect(bucketStep.getByRole('alert')).toContainText('OVH facture au minimum 1 Tio pendant 180 jours');
 await expect(bucketStep.getByRole('alert')).toContainText('Seuls les buckets qui cesseront d’être actifs à partir de maintenant seront archivés automatiquement.');
 assert.equal(schedules[ids[1]].archive_after_days,null);
 await bucketStep.getByRole('button',{name:'Accepter et activer',exact:true}).click();
 await expect.poll(()=>schedules[ids[1]]?.archive_after_days).toBe(7);assert.equal(schedules[ids[1]].archive_terms_accepted,true);
 // Advanced options reuse connection ①: no second credential form.
 await dialog.getByText('Options avancées',{exact:true}).click();
 const existing=dialog.getByRole('region',{name:'Utiliser un bucket existant',exact:true});
 await expect(existing.getByLabel('Secret key')).toHaveCount(0);
 await existing.getByLabel('Bucket existant',{exact:true}).fill('legacy-third');
 await existing.getByRole('button',{name:'Utiliser comme bucket actif',exact:true}).click();
 assert.deepEqual(writes.at(-1),{id:ids[1],path:'storage/buckets/attach',body:{revision:1,bucket:'legacy-third',activate:true}});
 await expect(existing).toContainText('Les cartouches se synchronisent avec lui en arrière-plan.');
 // A failing jobs poll is reported in one line.
 jobsFailure=true;await expect(bucketStep.getByRole('alert')).toContainText('Suivi indisponible pour le moment.',{timeout:15000});jobsFailure=false;
 // Saved keys that OVH now rejects: a red line in ①, ② disabled with its sentence instead of the check line, ③ not ready.
 const reopen=async()=>{await page.reload();await page.getByRole('button',{name:'Ouvrir Third',exact:true}).click();await dialog.getByRole('button',{name:'Stockage',exact:true}).click();await dialog.getByRole('button',{name:'S3',exact:true}).click();};
 const switched=creations[ids[1]];creations[ids[1]]=[{job_id:'create-3',bucket:'vc-third-2026-10-14-cd34',phase:'uncertain',message:null}];jobs[ids[1]]=[{...jobs[ids[1]][0],id:'create-3',status:'running',message:'Vérification de la création en cours.'}];
 keys='invalid';await reopen();
 await expect(connection.getByRole('alert')).toHaveText('OVH refuse ces clés. Corrigez-les avec « Modifier ».');
 await expect(connection.locator('p.text-destructive').first()).toContainText('da7b…d8cd');await expect(connection.locator('.text-emerald-700')).toHaveCount(0);
 await expect(bucketStep).toContainText('Corrigez d’abord les clés de la connexion.');await expect(bucketStep).not.toContainText('Vérification de la création en cours.');
 await expect(bucketStep.getByLabel('Prochain bucket',{exact:true})).toBeDisabled();await expect(bucketStep.getByRole('button',{name:'Changer',exact:true})).toBeDisabled();
 const readyStep=dialog.getByRole('region',{name:'Prêt à exporter',exact:true});await expect(readyStep).not.toContainText('Prêt à exporter vers');
 await connection.getByRole('button',{name:'Modifier',exact:true}).click();await expect(connection.getByLabel('Secret key',{exact:true})).toBeVisible();
 // OVH does not answer: an amber sentence; ② keeps its own state.
 keys='unreachable';await reopen();
 await expect(connection.getByRole('status')).toHaveText('OVH ne répond pas pour le moment. Les clés n’ont pas pu être vérifiées.');await expect(connection.getByRole('alert')).toHaveCount(0);
 await expect(bucketStep).toContainText('Vérification de la création en cours.');await expect(bucketStep).not.toContainText('Corrigez d’abord les clés de la connexion.');
 // Listing forbidden: still accepted, with one sentence.
 creations[ids[1]]=switched;jobs[ids[1]]=[{...jobs[ids[1]][0],id:'create-2',status:'completed',message:'Bucket créé et activé.'}];
 keys='list_denied';await reopen();
 await expect(connection.getByRole('status')).toHaveText('OVH accepte ces clés, mais interdit de lister les buckets.');await expect(connection.locator('.text-emerald-700')).toContainText('da7b…d8cd');
 await expect(readyStep).toContainText('Prêt à exporter vers vc-third-2026-10-13');
 keys='ok';
 // A typed name survives new suggestions and job updates, stays during the creation, and is replaced only after it succeeded.
 {const field=bucketStep.getByLabel('Prochain bucket',{exact:true});await expect(field).toHaveValue('third-2026-10-13');
  await field.fill('third-perso');drift=1;await page.waitForTimeout(12000);
  assert.ok(drift>2,'discover ran again while the name was typed');await expect(field).toHaveValue('third-perso');
  await bucketStep.getByRole('button',{name:'Changer',exact:true}).click();
  await expect(bucketStep).toContainText('Création de vc-third-perso en cours.');
  await page.waitForTimeout(6000);await expect(field).toHaveValue('third-perso');
  creations[ids[1]]=[{job_id:'create-1',bucket:'vc-third-perso',phase:'switched',message:null}];jobs[ids[1]]=[{...jobs[ids[1]][0],status:'completed'}];libraries[1].storage={...libraries[1].storage,bucket:'vc-third-perso'};
  await expect(bucketStep.getByRole('status').filter({hasText:'Le bucket vc-third-perso a été créé et activé.'})).toBeVisible({timeout:15000});
  copies['vc-third-perso']=1;jobs[ids[1]]=[...jobs[ids[1]],{id:'export-2',kind:'export',subject:'TAPE01',status:'completed',progress:100,message:null,created_at:'2026-10-13',updated_at:'2026-10-13'}];
  await expect(field).toHaveValue(/^third-2026-10-13-\d+$/,{timeout:15000});drift=0;}
 // A non-OVH connection cannot rotate or archive (R18).
 connections[ids[1]]={...connections[ids[1]],provider:'s3_compatible'};await page.reload();await page.getByRole('button',{name:'Ouvrir Third',exact:true}).click();await dialog.getByRole('button',{name:'Stockage',exact:true}).click();await dialog.getByRole('button',{name:'S3',exact:true}).click();
 const plain=dialog.getByRole('region',{name:'Bucket des exports',exact:true});
 await expect(plain.getByText('Le changement planifié nécessite une connexion OVH Cold Archive.')).toBeVisible();await expect(plain.getByText('L’archivage automatique nécessite une connexion OVH Cold Archive.')).toBeVisible();
 for(const name of ['Chaque semaine, le','Chaque mois, le','Archiver automatiquement'])await expect(plain.getByLabel(name)).toBeDisabled();
 assert.ok(writes.every(write=>write.id===ids[1]));assert.deepEqual(errors,[]);
 console.log('PASS VTL S3 tab: three steps, provider defaults, credential refusal, rejected or unchecked saved keys, inline create, automatic creation check, occupied bucket, schedule with OVH terms, advanced options on the saved connection, mobile layout.');
}finally{await browser.close();}
