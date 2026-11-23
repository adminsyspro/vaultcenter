import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
const ui=await staticUi();let browser;
try {
 browser=await chromium.launch();const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
 let role='admin',scenario='writing',phase='online',fail=false,jobRace=false,release,client='pbs';
 const changes=[],bodies=[];const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const serviceMap=value=>Object.fromEntries(['mhvtl.target','vtllibrary@10.service','vtltape@11.service','tgt.service'].map(k=>[k,value]));
 const status=()=>({configured:true,client,phase,ready:phase==='maintenance',blocked:phase==='maintenance',services:serviceMap(phase==='online'?'active':'inactive'),original:phase==='maintenance'?serviceMap('active'):{}});
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role,issuer:null,disabled:false},csrf_token:'csrf-power'}}));
 await page.route('**/v1/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[{id:demoId,name:'Power test',lifecycle:'ready',provisioning_revision:1}]});
  // The header bell lists every VTL, so its route is the one global read.
  if(path==='/v1/activity')return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  assert.ok(path.startsWith(`/v1/vtls/${demoId}/`));
  const action=path.slice(`/v1/vtls/${demoId}/`.length);
  if(route.request().method()==='POST'){
   assert.equal(route.request().headers()['x-csrf-token'],'csrf-power');changes.push(action);bodies.push({body:route.request().postData(),type:route.request().headers()['content-type']??null});
   if(fail)return route.fulfill({status:409,json:{error:'Le lecteur PBS est utilisé ; terminer la tâche avant la maintenance.'}});
   await new Promise(resolve=>{release=resolve;});phase=action==='maintenance/enter'?'maintenance':'online';
   return route.fulfill({json:status()});
  }
  const json=action==='maintenance'?status():action==='telemetry'?telemetryFixture(demoId,scenario):action==='telemetry/history'?emptyThroughputHistory(demoId,route.request().url()):action==='cartridges'?demoMedia():action==='jobs'?(jobRace?[{id:'race',status:'running',kind:'export',subject:'CVT001L8'}]:[]):{status:'ok',cartridges:9,jobs:0};
  return route.fulfill({json});
 });
 const url=ui.url+'/#media';
 const power=()=>page.getByRole('button',{name:'Alimentation de la bibliothèque',exact:true});
 const stop=()=>page.getByRole('button',{name:'Arrêter',exact:true});
 const dialog=()=>page.getByRole('dialog');
 await page.goto(url);await power().click();await expect(dialog()).toContainText('Une copie ou un mouvement');await expect(stop()).toBeDisabled();assert.equal(changes.length,0);
 await page.getByRole('button',{name:'Annuler',exact:true}).click();scenario='idle';await page.reload();await power().click();await expect(dialog()).toContainText('Déchargez la cartouche');await expect(stop()).toBeDisabled();
 await page.getByRole('button',{name:'Annuler',exact:true}).click();scenario='empty';await page.reload();await power().click();await expect(stop()).toBeEnabled();
 jobRace=true;await stop().click();await expect(dialog()).toContainText('Une opération VaultCenter est en cours');assert.equal(changes.length,0,'Fresh job check must catch a job started after confirmation opened');jobRace=false;
 await page.getByRole('button',{name:'Annuler',exact:true}).click();await page.reload();await power().click();await expect(stop()).toBeEnabled();fail=true;await stop().click();await expect(dialog()).toContainText('Le lecteur PBS est utilisé');await expect(page.locator('.lto-power')).toHaveAttribute('data-power','on');assert.deepEqual(changes,['maintenance/enter']);fail=false;
 await page.getByRole('button',{name:'Annuler',exact:true}).click();await power().click();await expect(stop()).toBeEnabled();await stop().dblclick();await expect(page.getByRole('button',{name:'Transition en cours…',exact:true})).toBeDisabled();await expect.poll(()=>changes.length).toBe(2);assert.equal(changes.length,2,'Double click cannot send a second stop');release();await expect(dialog()).toHaveCount(0);await expect(power()).toHaveAttribute('data-power','off');
 const lcd=page.getByRole('log',{name:'Dernières observations du lecteur'});await expect(lcd).toContainText('Maintenance active · services arrêtés, cartouches modifiables');
 await power().click();const start=page.getByRole('button',{name:'Remettre en service',exact:true});await expect(start).toBeEnabled();await start.click();await expect(page.getByRole('button',{name:'Transition en cours…',exact:true})).toBeDisabled();await expect.poll(()=>changes.length).toBe(3);release();await expect(dialog()).toHaveCount(0);await expect(power()).toHaveAttribute('data-power','on');assert.deepEqual(changes,['maintenance/enter','maintenance/enter','maintenance/leave']);await expect(lcd).toContainText('Bibliothèque en service');
 assert.deepEqual(bodies.map(b=>b.body),[null,null,null],'PBS stop/restart send no body');
 // Veeam: stopping requires confirming that no Veeam job uses the library.
 client='veeam';await page.reload();await power().click();
 const idle=()=>dialog().getByRole('checkbox',{name:'Aucun job Veeam n’utilise cette bibliothèque',exact:true});
 await expect(dialog()).toContainText('La console reste accessible.');await expect(dialog()).not.toContainText('PBS');
 await expect(stop()).toBeDisabled();await idle().check();await expect(stop()).toBeEnabled();
 await page.getByRole('button',{name:'Annuler',exact:true}).click();await power().click();await expect(idle()).not.toBeChecked();await expect(stop()).toBeDisabled();
 await idle().check();await stop().click();await expect.poll(()=>changes.length).toBe(4);
 assert.equal(changes[3],'maintenance/enter');assert.deepEqual(JSON.parse(bodies[3].body),{confirm_idle:true});assert.match(bodies[3].type,/^application\/json/);
 release();await expect(dialog()).toHaveCount(0);await expect(power()).toHaveAttribute('data-power','off');
 await power().click();await expect(dialog()).toContainText('seront rétablis.');await expect(idle()).toHaveCount(0);
 await page.getByRole('button',{name:'Annuler',exact:true}).click();
 role='viewer';await page.reload();await expect(power()).toBeDisabled();
 assert.deepEqual(errors,[]); // The LCD journal scrolls instead of growing the row that holds the cartridge rack.
 await page.setViewportSize({width:1600,height:1000});await page.waitForTimeout(200);
 const lower=page.locator('.lto-library-lower');const before=await lower.evaluate(e=>e.getBoundingClientRect().height);
 await page.locator('.lto-lcd-log').evaluate(log=>{for(let n=0;n<60;n++){const d=document.createElement('div');d.className='lto-lcd-entry';d.textContent='12:00:00 Lecteur 1 · Déchargement';log.appendChild(d);}});
 assert.equal(await lower.evaluate(e=>e.getBoundingClientRect().height),before,'LCD entries must not grow the library row');
 assert.ok(await page.locator('.lto-lcd-log').evaluate(e=>e.scrollHeight>e.clientHeight),'LCD journal must scroll');
 console.log('PASS power: active copy, loaded tape, job race, server refusal, admin-only, confirmation, CSRF, no duplicate stop, verified stop/restart, Veeam idle confirmation');
}finally{await browser?.close();await ui.close();}
