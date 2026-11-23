import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

// The S3 shelf keeps its room for what moves or needs a decision: a running send keeps
// the wide row, waiting sends and failures a shorter one (failures with a resume action),
// and verified sends a compact paginated list. A long queue is bounded until asked for.
const ui=await staticUi();let browser;
const base=Date.now();
const at=minutes=>new Date(base-minutes*60000).toISOString();
const code=n=>`CVT0${String(n).padStart(2,'0')}L8`;
const job=(n,status,minutes,message=null)=>({id:`export-${n}-${status}`,kind:'export',subject:code(n),destination_id:'original',configuration_revision:1,generation:1,status,progress:status==='running'?10:100,created_at:at(minutes+5),updated_at:at(minutes),message});
const receipt=(n,bytes)=>JSON.stringify({status:'verified',receipt:{scope:{job_id:`export-${n}-completed`,vtl_id:demoId,destination_id:'original',configuration_revision:1},generation:{barcode:code(n),generation:1,logical_size_bytes:bytes,segment_count:1},integrity:{status:'verified',method:'s3_sha256',verified_at:Math.floor(base/1000),verified_segments:1,verified_bytes:bytes}}});
const MiB=1024**2;
const jobs=[
 job(1,'running',1,JSON.stringify({export_progress:{phase:'uploading',completed_bytes:100*MiB,total_bytes:1000*MiB}})),
 // Seven tapes wait; the oldest request goes first.
 ...[2,3,4,5,6,7,8].map(n=>job(n,'queued',-n)),
 job(9,'failed',30,'S3 request failed'),
 job(10,'failed',40,'S3 request failed'),
 // Seven verified sends, the most recent first, and one without checksum evidence.
 ...[11,12,13,14,15,16,17].map(n=>job(n,'completed',n*10,receipt(n,n*100*MiB))),
 job(18,'completed',500,JSON.stringify({status:'verified'})),
 // An older failure of a tape sent since: its retry is the only result shown.
 {...job(11,'failed',900,'S3 request failed'),id:'export-11-old'},
];
const media=Array.from({length:18},(_,i)=>({barcode:code(i+1),capacity_bytes:2*1024**3,bytes_written:100*MiB,generation:1,state:i+1===9||i+1===10?'failed':'writable',export_resumable:i+1===9}));
async function open(role){
 const page=await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1400}});
 await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role},csrf_token:'test'}}));
 await page.route('**/v1/**',r=>{
  const path=new URL(r.request().url()).pathname;
  if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'VEEAM',lifecycle:'ready',provisioning_revision:1}]});
  if(path==='/v1/activity')return r.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
  if(path.endsWith('/storage/buckets'))return r.fulfill({json:[{id:'original',bucket:'vc-veeam-2026-10-13-0ab0',active:true,created_at:at(0),configuration_revision:1,copy_count:8,observed_status:null,rotation_phase:null}]});
  if(path.endsWith('/telemetry/history'))return r.fulfill({json:emptyThroughputHistory(demoId,r.request().url())});
  if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,'empty')});
  return r.fulfill({json:path.endsWith('/cartridges')?media:path.endsWith('/jobs')?jobs:path.endsWith('/maintenance')?{configured:false}:{status:'ok',cartridges:18,jobs:jobs.length,media_root:'/demo'}});
 });
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 await page.goto(`${ui.url}/#media?vtl=${demoId}`);
 return {page,errors,shelf:page.getByRole('region',{name:'Stockage S3 — externalisation'})};
}
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM||undefined,args:['--no-sandbox']});
 const {page,errors,shelf}=await open('admin');
 // The heading counts what is running, waiting and failed.
 await expect(shelf.locator('.lto-s3-heading')).toContainText('1 en cours · 7 en attente · 2 échecs');
 // The LCD keeps its room for the journal: what runs, then the queue as a single line.
 const lcd=page.getByRole('region',{name:'Journal d’activité de la bibliothèque'});
 await expect(lcd.locator('.lto-lcd-operation')).toHaveCount(2);
 await expect(lcd.locator('.lto-lcd-operation').nth(0)).toContainText('CVT001L8');
 const waiting=lcd.getByRole('link',{name:'+ 7 en attente'});
 await expect(waiting).toHaveAttribute('href',/^#jobs\?vtl=/);
 await expect(lcd.locator('.lto-lcd-operations')).not.toContainText('CVT002L8');
 await lcd.screenshot({path:'test-results/media-s3-zones-lcd.png'});
 // Running first, then the queue in order, bounded to five rows.
 const flows=shelf.getByRole('article');
 await expect(flows).toHaveCount(8);
 await expect(flows.nth(0)).toHaveAccessibleName('Externalisation de CVT001L8');
 await expect(flows.nth(1)).toHaveAccessibleName('Externalisation de CVT002L8');
 await expect(flows.nth(5)).toHaveAccessibleName('Externalisation de CVT006L8');
 // Only a running send draws the progress track.
 await expect(flows.nth(0).locator('.lto-s3-track')).toHaveCount(1);
 await expect(flows.nth(1)).toContainText('En attente');
 await expect(flows.nth(1).locator('.lto-s3-track')).toHaveCount(0);
 await expect(shelf.getByRole('article',{name:'Externalisation de CVT007L8'})).toHaveCount(0);
 const more=shelf.getByRole('button',{name:'+ 2 en attente'});
 await expect(more).toHaveAttribute('aria-expanded','false');
 await more.click();
 await expect(flows).toHaveCount(10);
 await expect(shelf.getByRole('article',{name:'Externalisation de CVT008L8'})).toBeVisible();
 await shelf.getByRole('button',{name:'Réduire la file d’attente'}).click();
 await expect(flows).toHaveCount(8);
 // A failure has no progress track; it offers to resume only a resumable tape.
 const failed=shelf.getByRole('article',{name:'Externalisation de CVT009L8'});
 await expect(failed).toContainText('Échec');
 await expect(failed.locator('.lto-s3-track')).toHaveCount(0);
 await expect(failed.getByRole('button',{name:'Reprendre l’envoi de CVT009L8'})).toBeVisible();
 await expect(shelf.getByRole('article',{name:'Externalisation de CVT010L8'}).getByRole('button')).toHaveCount(0);
 // Verified sends: one line each, five per page, the latest result of each tape only.
 const done=shelf.getByRole('list',{name:'Terminés récemment'});
 const lines=done.getByRole('listitem');
 await expect(lines).toHaveCount(5);
 await expect(done.locator('.lto-s3-track')).toHaveCount(0);
 await expect(lines.nth(0)).toContainText('CVT011L8');
 await expect(lines.nth(0)).toContainText('Empreinte validée');
 await expect(lines.nth(0)).toContainText(/1,07 Gio/);
 await expect(shelf).toContainText('Page 1 sur 2');
 const previous=shelf.getByRole('button',{name:'Page précédente des envois terminés'}),next=shelf.getByRole('button',{name:'Page suivante des envois terminés'});
 await expect(previous).toBeDisabled();
 const row=await lines.nth(0).boundingBox(),wide=await flows.nth(0).boundingBox();
 assert.ok(row.height<=wide.height/2,`a finished line (${row.height}px) must be far shorter than a running row (${wide.height}px)`);
 await shelf.evaluate(el=>el.scrollIntoView({block:'start'}));
 await page.screenshot({path:'test-results/media-s3-zones.png',fullPage:true});
 await next.click();
 await expect(shelf).toContainText('Page 2 sur 2');
 await expect(lines).toHaveCount(3);
 await expect(next).toBeDisabled();
 await expect(lines.nth(2)).toContainText('CVT018L8');
 await expect(lines.nth(2)).toContainText('Empreinte non confirmée');
 await expect(lines.nth(2)).not.toContainText('Empreinte validée');
 // Resuming goes through the usual confirmation of a send.
 await failed.getByRole('button',{name:'Reprendre l’envoi de CVT009L8'}).click();
 const dialog=page.getByRole('dialog');
 await expect(dialog).toBeVisible();
 await expect(dialog).toContainText('CVT009L8');
 await page.keyboard.press('Escape');
 // Folded, the heading still reports the failures.
 await shelf.getByRole('button',{name:'Stockage S3',exact:true}).click();
 await expect(shelf.getByRole('article')).toHaveCount(0);
 await expect(shelf.locator('.lto-s3-heading .lto-s3-error')).toHaveText('2 échecs');
 await shelf.getByRole('button',{name:'Stockage S3',exact:true}).click();
 await page.setViewportSize({width:390,height:1400});
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await shelf.evaluate(el=>el.scrollIntoView({block:'start'}));
 await page.screenshot({path:'test-results/media-s3-zones-mobile.png',fullPage:true});
 assert.deepEqual(errors,[]);
 await page.close();
 // A viewer sees the failure but cannot resume it.
 const viewer=await open('viewer');
 await expect(viewer.shelf.getByRole('article',{name:'Externalisation de CVT009L8'})).toContainText('Échec');
 await expect(viewer.shelf.getByRole('button',{name:'Reprendre l’envoi de CVT009L8'})).toHaveCount(0);
 console.log('PASS media S3 zones: counts, bounded queue, failures with resume, compact paginated finished sends, folded failures, mobile, viewer');
} finally {await browser?.close();await ui.close();}
