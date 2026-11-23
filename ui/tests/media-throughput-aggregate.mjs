import {selectOption} from './helpers/select.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from 'node:assert/strict';
import {chromium,expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId,demoMedia,telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {createDemoHistory} from '../dev-history.mjs';
// Selector value of the summed curve, as exported by media-throughput.tsx.
const ALL_TAPES='{"aggregate":true}';

// Demo history: each series averaged per bucket, then summed; an unmeasured bucket stays null.
{
 const clock=3_000_000_000,start=clock-120000;
 const store=createDemoHistory({now:()=>clock,retentionDays:30});
 const sample=(drive_id,barcode,at,writeRate,localGrowth=null)=>({vtl_id:demoId,kind:'pbs',drive_id,barcode,transfer_id:null,at,writeRate,localGrowth});
 store.record([sample('11','TAPEA',start+1000,50,10),sample('11','TAPEA',start+2000,150,10),sample('12','TAPEB',start+1000,50,5),
  sample('11','TAPEA',start+10500,40),{...sample('11','TAPEA',start+1000,1000),vtl_id:'00000000-0000-4000-8000-0000000000ff'}]);
 const ask=extra=>store.query(demoId,new URLSearchParams({kind:'pbs',window_seconds:'120',end_ms:String(clock),...extra}));
 const all=ask({aggregate:'true'});
 assert.equal(all.status,'available');assert.equal(all.points.length,24);
 assert.deepEqual([all.points[0].writeRate,all.points[0].localGrowth],[150,15]);
 assert.equal(all.points[1].writeRate,null,'an unmeasured bucket is a gap, not zero');
 assert.deepEqual([all.points[2].writeRate,all.points[2].localGrowth],[40,null]);
 assert.equal(ask({aggregate:'true',drive_id:'12'}).points[0].writeRate,50);
 assert.equal(all.series.length,2,'the inventory stays the usual one');
 assert.throws(()=>ask({aggregate:'true',barcode:'TAPEA'}));
 assert.throws(()=>ask({aggregate:'false'}));
}

const ui=await staticUi();let browser;
try{
 browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM,args:['--no-sandbox']});
 const now=Date.now();
 async function open(locale,scenario){
  const page=await browser.newPage({locale,viewport:{width:1440,height:1100}});const errors=[],requests=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/auth/**',r=>r.fulfill({json:{local:true,oidc:false,user:{id:'test',username:'test',role:'viewer',issuer:null,disabled:false},csrf_token:'test'}}));
  await page.route('**/v1/**',r=>{
   const url=new URL(r.request().url()),path=url.pathname,query=url.searchParams;
   if(path==='/v1/vtls')return r.fulfill({json:[{id:demoId,name:'Test',lifecycle:'ready',provisioning_revision:1}]});
   if(path.endsWith('/telemetry/history')){
    requests.push(query);
    assert.ok(!(query.has('aggregate')&&(query.has('barcode')||query.has('transfer_id'))),'aggregate never goes with a barcode or a transfer');
    const result=emptyThroughputHistory(demoId,r.request().url());result.status='available';
    result.series=query.get('kind')==='s3'?[{drive_id:'s3',barcode:'CVT002L8',transfer_id:'t1',last_at:now}]:[{drive_id:'11',barcode:'CVT001L8',last_at:now},{drive_id:'11',barcode:'CVT002L8',last_at:now-1000}];
    result.points=query.has('aggregate')?[{at:now-10000,writeRate:(query.get('kind')==='s3'?63:30)*1024**2,localGrowth:null}]:query.has('barcode')?[{at:now-10000,writeRate:12*1024**2,localGrowth:null}]:[];
    return r.fulfill({json:result});
   }
   if(path.endsWith('/telemetry'))return r.fulfill({json:telemetryFixture(demoId,scenario)});
   return r.fulfill({json:path.endsWith('/cartridges')?demoMedia():path.endsWith('/jobs')?[]:{status:'ok',cartridges:9,jobs:0,media_root:'/demo'}});
  });
  await page.goto(ui.url+'/#media');
  return {page,errors,requests,selector:()=>page.getByRole('combobox',{name:locale==='en-US'?'Chart cartridge':'Cartouche du graphique'})};
 }

 // No tape in the drive: the panel opens on the sum of every tape.
 const empty=await open('fr-FR','empty');
 const all=empty.page.getByRole('figure',{name:'Débit de copie de toutes les cartouches',exact:true});
 await expect(all).toBeVisible();await expect(all).toContainText('30 Mio/s');
 await expect(empty.selector()).toHaveText('Toutes les cartouches');
 assert.ok(empty.requests.some(q=>q.get('aggregate')==='true'&&q.get('kind')==='pbs'));
 assert.ok(empty.requests.every(q=>!q.has('barcode')),'nothing asks for one tape while none is loaded or picked');
 await empty.selector().click();
 await expect(empty.page.getByRole('listbox').getByRole('option')).toHaveText(['Toutes les cartouches','CVT001L8','CVT002L8']);
 await empty.page.keyboard.press('Escape');
 // One tape can still be picked, and changing the transfer kind applies the default again.
 await selectOption(empty.selector(),JSON.stringify({barcode:'CVT001L8'}));
 await expect(empty.page.getByRole('figure',{name:'Débit de copie de CVT001L8',exact:true})).toContainText('12 Mio/s');
 await selectOption(empty.page.getByRole('combobox',{name:'Type de transfert du graphique'}),'s3');
 await expect(empty.page.getByRole('figure',{name:'Débit vers S3 de toutes les cartouches',exact:true})).toContainText('63 Mio/s');
 assert.ok(empty.requests.some(q=>q.get('aggregate')==='true'&&q.get('kind')==='s3'&&!q.has('drive_id')));
 assert.deepEqual(empty.errors,[]);

 // A tape is loaded: its own curve by default, the sum on demand.
 const writing=await open('fr-FR','writing');
 await expect(writing.page.getByRole('figure',{name:'Débit de copie de CVT001L8',exact:true})).toBeVisible();
 await expect(writing.selector()).toHaveText('CVT001L8');
 // Until the first telemetry answer the drive looks empty, so earlier requests may have asked for the sum.
 const beforeChoice=writing.requests.length;
 await selectOption(writing.selector(),ALL_TAPES);
 const summed=writing.page.getByRole('figure',{name:'Débit de copie de toutes les cartouches',exact:true});
 await expect(summed).toContainText('30 Mio/s');
 await expect(summed.locator('.lto-throughput-value')).toHaveText('30 Mio/s');
 assert.ok(writing.requests.slice(beforeChoice).some(q=>q.get('aggregate')==='true'&&q.get('drive_id')==='11'));
 assert.deepEqual(writing.errors,[]);

 // English label.
 const english=await open('en-US','empty');
 await expect(english.selector()).toHaveText('All cartridges');
 assert.deepEqual(english.errors,[]);
 console.log('PASS aggregated throughput: demo sums per bucket with gaps, default when no tape is loaded, option first, one tape still selectable, kind change, loaded tape, English');
} finally {await browser?.close();await ui.close();}
