import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

// Clicking a tape shows what the appliance knows about it: state, position,
// media type, occupancy, version and its latest S3 copy, without a backend change.
const ui=await staticUi(),browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM||undefined,args:['--no-sandbox']});
const MiB=1024**2,checked=new Date(Date.now()-2*86400000).toISOString();
const media=demoMedia().map(m=>m.barcode==='CVT004L8'
 ?{...m,media_type:'LTO-8',generation:2,last_copy:{bucket:'vc-pbs-2026-11',generation:2,checked_at:checked,bucket_state:'available',bytes:512*MiB}}
 :m);
function telemetry(){
 const value=telemetryFixture(demoId,'writing');
 // PBS wrote 700 MiB to CVT004L8 since its 512 MiB copy.
 value.media=value.media.map(m=>m.barcode==='CVT004L8'?{...m,local_size_bytes:700*MiB}:m);
 return value;
}
async function open(locale='fr-FR',viewport={width:1440,height:1100}){
 const page=await browser.newPage({locale,viewport}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer'},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VTL test',lifecycle:'ready',provisioning_revision:1}]});
  if(path==='/v1/activity')return r.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetry()});
  if(path.endsWith('/storage/buckets'))return r.fulfill({json:[]});
  if(path.endsWith('/jobs'))return r.fulfill({json:[]});
  return r.fulfill({json:path.endsWith('/cartridges')?media:path.endsWith('/maintenance')?{configured:false}:{status:'ok'}});
 });
 await page.goto(ui.url+'/#media');
 await page.getByRole('button',{name:'Tableau',exact:true}).or(page.getByRole('button',{name:'Table',exact:true})).click();
 return {page,errors};
}
const row=(details,term)=>details.locator('div').filter({has:details.page().locator('dt',{hasText:new RegExp(`^${term}$`)})});
try{
 const {page,errors}=await open();
 await page.getByRole('button',{name:'Détails de CVT004L8',exact:true}).click();
 const dialog=page.getByRole('dialog');
 const details=dialog.getByRole('definition').first().locator('xpath=ancestor::dl');
 await expect(dialog.locator('dl[aria-label="Détails de la cartouche"]')).toBeVisible();
 // The heading counts the same bytes as the details, not the catalogue's last reading of 0.
 await expect(dialog.locator('[data-slot="dialog-description"]')).toContainText('700 Mio');
 await expect(row(details,'État')).toContainText('Inscriptible');
 await expect(row(details,'Position')).toContainText('Emplacement 4');
 await expect(row(details,'Type de média')).toContainText('LTO-8');
 // A writable tape is measured from its local volume, not the catalogue's last reading.
 await expect(row(details,'Occupation')).toContainText('700 Mio sur 2 Gio · 34,2 %');
 await expect(row(details,'Occupation')).toContainText('Occupation locale estimée');
 await expect(row(details,'Occupation').getByRole('progressbar',{name:'Occupation de la cartouche'})).toBeVisible();
 await expect(row(details,'Version')).toContainText('2');
 const copy=row(details,'Copie S3');
 await expect(copy).toContainText('vc-pbs-2026-11 · version 2 · 512 Mio');
 await expect(copy).toContainText('Vérifiée le');
 await expect(copy).toContainText('Des écritures ont eu lieu après cette copie.');
 await dialog.screenshot({path:'test-results/cartridge-details.png'});
 await page.keyboard.press('Escape');
 // A tape in a drive names the drive and what it is doing; no type row without a type.
 await page.getByRole('button',{name:'Détails de CVT001L8',exact:true}).click();
 await expect(row(details,'Position')).toContainText('Lecteur LTO · 01 · Écriture en cours');
 await expect(row(details,'Type de média')).toHaveCount(0);
 await expect(row(details,'Copie S3')).toContainText('Aucune copie S3 pour l’instant.');
 await page.keyboard.press('Escape');
 // On a phone the terms go above their values and nothing scrolls sideways.
 await page.setViewportSize({width:390,height:900});
 await page.getByRole('button',{name:'Détails de CVT004L8',exact:true}).click();
 await expect(row(details,'Copie S3')).toBeVisible();
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await dialog.screenshot({path:'test-results/cartridge-details-mobile.png'});
 assert.deepEqual(errors,[]);
 await page.close();
 // English.
 const en=await open('en-US');
 await en.page.getByRole('button',{name:'Details of CVT004L8',exact:true}).or(en.page.getByRole('button',{name:'Détails de CVT004L8',exact:true})).click();
 const enDetails=en.page.getByRole('dialog').locator('dl');
 await expect(enDetails).toContainText('Media type');
 await expect(enDetails).toContainText('700 MiB of 2 GiB · 34.2%');
 await expect(enDetails).toContainText('The tape was written to after this copy.');
 assert.deepEqual(en.errors,[]);
 console.log('PASS cartridge details: state, position, media type, occupancy, version, S3 copy and later writes, drive, mobile, English');
}finally{await browser.close();await ui.close();}
