import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

const ui=await staticUi(),browser=await chromium.launch();
try {
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1600,height:1200}}),errors=[];
 let scenario='available';
 const exportJob={id:'s3-export',kind:'export',subject:'CVT004L8',status:'running',progress:37,created_at:new Date().toISOString(),updated_at:new Date().toISOString(),message:JSON.stringify({export_progress:{phase:'uploading',completed_bytes:374,total_bytes:1000}})};
 page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  assert.equal(r.request().method(),'GET');
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Bibliothèque test',lifecycle:'ready',provisioning_revision:1}]});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry')){
   const value=telemetryFixture(demoId,scenario==='stale'?'stale':'writing');
   value.slots=Array.from({length:62},(_,i)=>({number:i+1,kind:'storage',barcode:[4,5].includes(i+1)?null:value.slots[i]?.barcode??null,position_known:true,observed_at:value.generated_at}));
   value.slots.push(...[null,'CVT004L8','CVT005L8'].map((barcode,i)=>({number:63+i,kind:'import_export',barcode,position_known:true,observed_at:value.generated_at})));
   if(scenario==='moved'){
    value.slots.find(s=>s.number===64).barcode=null;
    value.slots.find(s=>s.number===5).barcode='CVT004L8';
    value.slots.reverse(); // Telemetry arrival order must not reorder physical slots.
   }
   if(scenario==='unknown')value.slots=[];
   if(scenario==='external')value.slots[ value.slots.length-1 ].barcode='EXTERNAL01';
   return r.fulfill({json:value});
  }
  return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[exportJob]:path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#media');
 const cart=page.getByRole('region',{name:'Chariot d’import / export',exact:true});
 const inventory=page.getByRole('region',{name:'Inventaire des cartouches',exact:true});
 await expect(cart).toBeVisible();
 const sending=page.getByRole('progressbar',{name:'Envoi de CVT004L8 vers S3',exact:true});
 await expect(sending).toHaveAttribute('aria-valuenow','37');
 await expect(sending).toContainText('37 %');
 await expect(sending.locator('svg.lucide-cloud-upload')).toBeVisible();
 await expect(cart.getByRole('article',{name:'Cartouche CVT004L8',exact:true}).locator('.lto-occupancy-donut')).toHaveCount(0);
 await expect(cart.getByRole('article',{name:'Cartouche CVT005L8',exact:true}).locator('.lto-occupancy-donut')).toHaveCount(1);
 await expect(cart.getByText('2 / 3 occupés',{exact:true})).toBeVisible();
 await expect(cart.getByText('63',{exact:true})).toBeVisible();
 for(const barcode of ['CVT004L8','CVT005L8']){
  await expect(cart.getByRole('button',{name:`Détails de ${barcode}`,exact:true})).toHaveCount(1);
  await expect(inventory.getByRole('button',{name:`Détails de ${barcode}`,exact:true})).toHaveCount(0);
 }
 await cart.getByRole('checkbox',{name:'Sélectionner CVT004L8',exact:true}).check();
 await page.getByRole('button',{name:'Tableau',exact:true}).click();
 await expect(page.getByRole('checkbox',{name:'Sélectionner CVT004L8',exact:true})).toBeChecked();
 await page.getByRole('button',{name:'Bibliothèque',exact:true}).click();
 // A full magazine must scroll independently, keeping the cart on screen.
 for(const width of [2048,1600,1000,390]){
  await page.setViewportSize({width,height:1200});
  const main=page.locator('main');
  assert.equal(await main.evaluate(e=>e.scrollWidth<=e.clientWidth),true,`Console overflows at ${width}px`);
  const magazine=inventory.locator('.lto-magazine-spines');
  const before=await cart.boundingBox();
  await magazine.evaluate(e=>{e.scrollLeft=e.scrollWidth;});
  await expect.poll(()=>magazine.evaluate(e=>e.scrollLeft)).toBeGreaterThan(0);
  const after=await cart.boundingBox();
  assert.equal(after.x,before.x,'Scrolling the magazine must not move the cart');
  assert.ok(after.x>=0&&after.x+after.width<=width,'Cart stays within the viewport');
  assert.equal(await main.evaluate(e=>e.scrollWidth<=e.clientWidth&&e.scrollLeft===0),true);
 }
 await page.setViewportSize({width:1600,height:1200});
 const a=await inventory.boundingBox(),b=await cart.boundingBox();
 assert.ok(b.x>=a.x+a.width,'The cart belongs beside the cartridge inventory');
 const capture=async name=>{if(process.env.COLD_VTL_CAPTURE_DIR)await page.screenshot({path:`${process.env.COLD_VTL_CAPTURE_DIR}/${name}.png`,fullPage:true});};
 await capture('desktop-dark');
 await page.getByRole('button',{name:'Changer le thème',exact:true}).click();
 await capture('desktop-light');
 await page.setViewportSize({width:390,height:844});
 await cart.scrollIntoViewIfNeeded();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
 await capture('mobile-light');
 await page.getByRole('button',{name:'Changer le thème',exact:true}).click();
 await cart.scrollIntoViewIfNeeded();await capture('mobile-dark');
 scenario='stale';await page.reload();
 await expect(cart.getByText('Positions non connues',{exact:true})).toBeVisible();
 await expect(cart.getByRole('button',{name:'Détails de CVT004L8',exact:true})).toHaveCount(0);
 await expect(cart.getByText('Vide',{exact:true})).toHaveCount(0);
 scenario='unknown';await page.reload();
 await expect(cart.getByText('Positions non connues',{exact:true})).toBeVisible();
 await expect(cart.getByText('63',{exact:true})).toHaveCount(0);
 scenario='external';await page.reload();
 await expect(cart.getByText('EXTERNAL01',{exact:true})).toBeVisible();
 await expect(cart.getByText('2 / 3 occupés',{exact:true})).toBeVisible();
 scenario='moved';await page.setViewportSize({width:1600,height:1200});await page.reload();
 const slot5=inventory.getByRole('group',{name:'Emplacement 05',exact:true});
 await expect(slot5.getByRole('progressbar',{name:'Envoi de CVT004L8 vers S3',exact:true})).toHaveAttribute('aria-valuenow','37');
 await expect(slot5.getByRole('button',{name:'Détails de CVT004L8',exact:true})).toBeVisible();
 await expect(slot5.getByText('05',{exact:true})).toBeVisible();
 await expect(cart.getByRole('button',{name:'Détails de CVT004L8',exact:true})).toHaveCount(0);
 const slots=inventory.getByRole('group');
 await expect(slots).toHaveCount(62);
 assert.deepEqual(await slots.evaluateAll(nodes=>nodes.map(n=>n.getAttribute('aria-label'))),Array.from({length:62},(_,i)=>`Emplacement ${String(i+1).padStart(2,'0')}`));
 for(const width of [2048,1600,1000,390]){
  await page.setViewportSize({width,height:1200});
  await slot5.scrollIntoViewIfNeeded();
  const tape=await slot5.locator('.lto-cartridge-spine').boundingBox();
  const occupied=await slot5.locator('.lto-bay').boundingBox();
  const empty=await inventory.getByRole('group',{name:'Emplacement 04',exact:true}).locator('.lto-bay').boundingBox();
  const exported=await cart.locator('.lto-bay').first().boundingBox();
  assert.equal(occupied.height,empty.height);
  assert.equal(occupied.height,exported.height);
  assert.equal(occupied.height-tape.height,8,'Cartridge fills its bay with only the 4px inset');
  assert.equal(await page.locator('main').evaluate(e=>e.scrollWidth<=e.clientWidth),true);
 }
 await page.setViewportSize({width:1600,height:1200});
 await inventory.locator('.lto-magazine-spines').evaluate(e=>{e.scrollLeft=0;});
 await capture('slot5-desktop');
 await page.getByRole('button',{name:'Changer le thème',exact:true}).click();await capture('slot5-light');
 await page.setViewportSize({width:390,height:844});await slot5.scrollIntoViewIfNeeded();await capture('slot5-mobile');
 // The cloud tracks the current transfer and disappears when it finishes or fails.
 exportJob.progress=82;exportJob.message=JSON.stringify({export_progress:{phase:"uploading",completed_bytes:820,total_bytes:1000}});
 await expect(sending).toHaveAttribute('aria-valuenow','82',{timeout:12000});
 for(const status of ['completed','failed']) {
  exportJob.status=status;await page.reload();
  await expect(slot5.locator('.lto-occupancy-donut')).toHaveCount(1);
  await expect(sending).toHaveCount(0);
 }
 exportJob.status='queued';exportJob.progress=0;exportJob.message=null;await page.reload();
 await expect(sending).not.toHaveAttribute('aria-valuenow');
 assert.deepEqual(errors,[]);
 console.log('PASS import/export cart: ordered slots after transfer to slot 5, visible numbers, matching bay sizes, 62-slot magazine containment, no duplicates, selection, unknown/stale positions, uncatalogued media, desktop/mobile and themes.');
}finally{await browser.close();await ui.close();}
