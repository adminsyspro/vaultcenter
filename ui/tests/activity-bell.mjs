import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId} from './helpers/telemetry-fixtures.mjs';
const pbs='00000000-0000-4000-8000-0000000000aa';
const ui=await staticUi();let browser;
const unreadFeed={
 active:[
  {id:'job:1',source:'job',kind:'export',vtl_id:demoId,vtl_name:'Veeam',subject:'CVT002L8',destination_id:'b',started_at:'2026-10-23 08:00:00',progress:41},
  {id:'provider:j2:accepted',source:'provider',kind:'archive',vtl_id:demoId,vtl_name:'Veeam',subject:'vc-veeam-a',destination_id:'b',started_at:'2026-10-23 07:00:00',progress:null},
 ],
 recent:[
  {id:'provider:j3:archived',source:'provider',kind:'archive',vtl_id:pbs,vtl_name:'PBS',subject:'vc-pbs-a',destination_id:'p',outcome:'success',finished_at:'2026-10-23 06:00:00',unread:true},
  {id:'job:4',source:'job',kind:'restore',vtl_id:demoId,vtl_name:'Veeam',subject:'CVT001L8',destination_id:'b',outcome:'failure',finished_at:'2026-10-23 05:00:00',unread:true},
  {id:'job:5',source:'job',kind:'export',vtl_id:demoId,vtl_name:'Veeam',subject:'CVT003L8',destination_id:'b',outcome:'success',finished_at:'2026-10-22 05:00:00',unread:false},
 ],
 seen_at:'2026-10-22 06:00:00',unread:2,
};
const readFeed={...unreadFeed,recent:unreadFeed.recent.map(item=>({...item,unread:false})),seen_at:'2026-10-23 09:00:00',unread:0};
let feed=unreadFeed,failing=false,activityCalls=0,seenCalls=0,afterSeen=0;
async function routes(page){
 await page.route('**/auth/**',route=>route.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'admin',issuer:null,disabled:false},csrf_token:'test'}}));
 await page.route('**/v1/**',route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/v1/vtls')return route.fulfill({json:[
   {id:demoId,name:'Veeam',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-b',prefix:'vtl'}},
   {id:pbs,name:'PBS',lifecycle:'ready',provisioning_revision:1,storage:{provider:'ovh_cold_archive_v1',bucket:'vc-p',prefix:'vtl'}},
  ]});
  if(path==='/v1/activity/seen'){seenCalls++;afterSeen=0;assert.equal(route.request().method(),'POST');if(Array.isArray(feed.recent))feed={...feed,recent:feed.recent.map(item=>({...item,unread:false})),unread:0};return route.fulfill({json:{seen_at:'2026-10-23 09:00:00'}});}
  if(path==='/v1/activity'){activityCalls++;afterSeen++;return failing?route.fulfill({status:500,json:{error:'Catalogue indisponible'}}):route.fulfill({json:feed});}
  if(path.endsWith('/cartridges')||path.endsWith('/jobs'))return route.fulfill({json:[]});
  if(path.endsWith('/storage/archive'))return route.fulfill({json:{configured:true,operations:[]}});
  return route.fulfill({json:{status:'ok',cartridges:0,jobs:0,media_root:'/fixture'}});
 });
}
try {
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const page=await browser.newPage({locale:'fr-FR',timezoneId:'Europe/Paris',viewport:{width:1280,height:900}});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 await routes(page);
 await page.clock.install();
 await page.goto(`${ui.url}/#media?vtl=${demoId}`);
 const bell=page.getByRole('button',{name:/^Notifications :/});
 await expect(page.getByRole('button',{name:'Notifications : 2 en cours, 2 non lues',exact:true})).toBeVisible();
 await expect(bell.locator('.lto-activity-badge')).toHaveText('2');
 await expect(bell.locator('.lto-activity-ring')).toHaveCount(1);

 // Opening marks everything as seen; the dots stay while the panel is open.
 await bell.click();
 const panel=page.getByRole('dialog',{name:'Notifications',exact:true});
 await expect(panel).toBeVisible();
 const running=panel.getByRole('region',{name:'En cours',exact:true});
 const recent=panel.getByRole('region',{name:'Récent',exact:true});
 await expect(running.getByRole('link')).toHaveCount(2);
 await expect(recent.getByRole('link')).toHaveCount(3);
 await expect(running.getByRole('progressbar')).toHaveAttribute('aria-valuenow','41');
 await expect(running).toContainText('Envoi vers S3');
 await expect(running).toContainText('Archivage chez OVH');
 await expect(running).toContainText('vc-veeam-a');
 // 08:00 UTC is 10:00 in Paris.
 await expect(running).toContainText(/Veeam · depuis .*10:00/);
 await expect(recent).toContainText('Archivé chez OVH');
 await expect(recent).toContainText('Téléchargement depuis S3');
 await expect(recent).toContainText(/PBS · .*08:00/);
 await expect.poll(()=>seenCalls).toBe(1);
 await expect(bell.locator('.lto-activity-badge')).toHaveCount(0);
 await expect(page.getByRole('button',{name:'Notifications : 2 en cours, 0 non lues',exact:true})).toBeVisible();
 await expect(panel.locator('.lto-activity-dot')).toHaveCount(2);
 feed=readFeed;const before=activityCalls;
 await page.clock.runFor(31000);
 await expect.poll(()=>activityCalls).toBeGreaterThan(before);
 await expect(panel.locator('.lto-activity-dot')).toHaveCount(2);
 await page.screenshot({path:'test-results/activity-bell-desktop.png'});
 await page.keyboard.press('Escape');
 await expect(panel).toHaveCount(0);
 await expect(bell).toBeFocused();
 await bell.click();
 await expect(panel.locator('.lto-activity-dot')).toHaveCount(0);

 // Each line opens its VTL on the right section.
 await expect(panel.getByRole('link',{name:'Tout voir dans Activité',exact:true})).toHaveAttribute('href',`#jobs?vtl=${demoId}`);
 await panel.getByRole('link',{name:/Archivé chez OVH/}).click();
 await expect(panel).toHaveCount(0);
 await expect.poll(()=>page.evaluate(()=>location.hash)).toBe(`#storage?vtl=${pbs}`);
 await expect(page.getByLabel('Bibliothèque VTL',{exact:true})).toContainText('PBS');
 await bell.click();
 await expect(panel.getByRole('link',{name:'Tout voir dans Activité',exact:true})).toHaveAttribute('href',`#jobs?vtl=${pbs}`);
 await panel.getByRole('link',{name:/Téléchargement depuis S3/}).click();
 await expect.poll(()=>page.evaluate(()=>location.hash)).toBe(`#jobs?vtl=${demoId}`);

 // Keyboard: Enter opens, the first line takes the focus, Enter follows it.
 await page.evaluate(()=>{location.hash='#media';});
 await bell.focus();await page.keyboard.press('Enter');
 await expect(panel).toBeVisible();
 const first=running.getByRole('link').first();
 if(!await first.evaluate(element=>element===document.activeElement))await page.keyboard.press('Tab');
 await expect(first).toBeFocused();
 await page.keyboard.press('Enter');
 await expect.poll(()=>page.evaluate(()=>location.hash)).toBe(`#jobs?vtl=${demoId}`);
 await expect(panel).toHaveCount(0);

 // Empty, many unread, error and retry, unexpected answer.
 feed={active:[],recent:[],seen_at:null,unread:0};await page.reload();
 await expect(page.getByRole('button',{name:'Notifications : 0 en cours, 0 non lues',exact:true})).toBeVisible();
 await expect(bell.locator('.lto-activity-badge')).toHaveCount(0);
 await expect(bell.locator('.lto-activity-ring')).toHaveCount(0);
 await bell.click();await expect(panel).toContainText('Aucune opération en cours ni récente.');
 await page.keyboard.press('Escape');
 feed={...unreadFeed,recent:Array.from({length:12},(_,n)=>({...unreadFeed.recent[1],id:`job:x${n}`})),unread:12};await page.reload();
 await expect(bell.locator('.lto-activity-badge')).toHaveText('9+');
 failing=true;const seen=seenCalls;await page.reload();
 await bell.click();
 // The reload that follows « seen » has failed too.
 await expect.poll(()=>seenCalls>seen&&afterSeen>0).toBe(true);
 await expect(panel.getByRole('alert')).toContainText('Activité indisponible.');
 failing=false;feed=unreadFeed;const retry=activityCalls;
 await panel.getByRole('button',{name:'Réessayer',exact:true}).click();
 await expect(running.getByRole('link')).toHaveCount(2);assert.ok(activityCalls>retry);
 await page.keyboard.press('Escape');
 feed={status:'ok'};await page.reload();
 await bell.click();
 await expect(panel.getByRole('alert')).toContainText('Activité indisponible.');
 await page.keyboard.press('Escape');

 // Phone width: the panel fits the screen.
 feed=unreadFeed;await page.setViewportSize({width:390,height:844});await page.reload();
 await bell.click();await expect(running.getByRole('link')).toHaveCount(2);
 const box=await panel.boundingBox();
 assert.ok(box.x>=0&&box.x+box.width<=390,`panel ${box.x}+${box.width} must fit 390px`);
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'the page must not scroll horizontally');
 await page.screenshot({path:'test-results/activity-bell-mobile.png'});
 assert.deepEqual(errors,[]);

 // English labels.
 const english=await browser.newPage({locale:'en-US',timezoneId:'Europe/Paris',viewport:{width:1280,height:900}});
 english.on('pageerror',error=>errors.push(error.message));
 await routes(english);feed=unreadFeed;
 await english.goto(`${ui.url}/#media?vtl=${demoId}`);
 await english.getByRole('button',{name:'Notifications: 2 in progress, 2 unread',exact:true}).click();
 const englishPanel=english.getByRole('dialog',{name:'Notifications',exact:true});
 await expect(englishPanel).toContainText('Archived at OVH');
 await expect(englishPanel).toContainText('Archiving at OVH');
 await expect(englishPanel.getByRole('link',{name:'See all in Activity',exact:true})).toBeVisible();
 assert.deepEqual(errors,[]);
 console.log('PASS activity bell: badge, ring and label, seen on open with dots kept until close, sections, navigation to each VTL, keyboard, empty, 9+, error and retry, unexpected answer, mobile, English');
} finally {await browser?.close();await ui.close();}
