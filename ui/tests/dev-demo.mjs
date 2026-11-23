import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdtempSync,readFileSync,rmSync,statSync,writeFileSync,appendFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createDemoHistory} from '../dev-history.mjs';
import {createDemoHandler,assertLoopback} from '../dev-demo.mjs';
assert.throws(()=>assertLoopback('0.0.0.0'));
assertLoopback('127.0.0.1');
let now=Date.now();
// Without the in-flight uploads, so power-off and inventories stay deterministic.
const handler=createDemoHandler({seedExports:false,now:()=>now});let upstream=0;
const server=createServer((req,res)=>{if(!handler(req,res)){upstream++;res.end('upstream');}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const url=`http://127.0.0.1:${server.address().port}`;
try {
 for(const [path,method] of [['/auth/login','POST'],['/auth/me','GET'],['/v1/vtls','GET'],['/v1/vtls/00000000-0000-4000-8000-000000000001/telemetry','GET'],['/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges/CVT003L8/seal','POST'],['/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges/CVT003L8/export','POST'],['/auth/oidc/login','GET'],['/v1/unsupported','POST']]) {
  const response=await fetch(url+path,{method,redirect:'manual',headers:{'Content-Type':'application/json'},body:method==='POST'?'{}':undefined});
  assert.notEqual(response.status,302);assert.equal(response.headers.get('x-vaultcenter-demo'),'1');await response.json();
 }
 const telemetry=()=>fetch(url+'/v1/vtls/00000000-0000-4000-8000-000000000001/telemetry').then(r=>r.json());
 const start=()=>fetch(url+'/__demo/scenario?state=s3-export',{method:'POST'});
 assert.equal((await start()).status,200);
 const initial=(await telemetry()).demo_transfer;
 assert.equal(initial.transferred_bytes,0);
 now+=10000;
 const moving=(await telemetry()).demo_transfer;
 assert.ok(moving.transferred_bytes>0&&moving.transferred_bytes<moving.total_bytes);
 assert.ok(moving.rate_bps>0);
 now+=120000;
 const complete=(await telemetry()).demo_transfer;
 assert.equal(complete.status,'completed');assert.equal(complete.rate_bps,0);
 assert.equal(complete.transferred_bytes,complete.total_bytes);
 await start();const restarted=(await telemetry()).demo_transfer;
 assert.notEqual(restarted.id,initial.id);assert.equal(restarted.transferred_bytes,0);
 await fetch(url+'/__demo/scenario?state=idle',{method:'POST'});
 assert.equal((await telemetry()).demo_transfer,undefined);
 const scope='/v1/vtls/00000000-0000-4000-8000-000000000001';
 const power=action=>fetch(url+scope+'/maintenance/'+action,{method:'POST',headers:{'X-CSRF-Token':'demo-only'}});
 assert.equal((await fetch(url+scope+'/maintenance/enter',{method:'POST'})).status,403);
 for(const state of ['writing','reading','loaded','idle','stale','missing','s3-export']){
  await fetch(url+'/__demo/scenario?state='+state,{method:'POST'});
  assert.equal((await power('enter')).status,409,'Busy or unconfirmed demo state must refuse shutdown: '+state);
 }
 await fetch(url+'/__demo/scenario?state=empty',{method:'POST'});
 const stopped=await power('enter');assert.equal(stopped.status,200);
 assert.ok(Object.values((await stopped.json()).services).every(value=>value==='inactive'));
 assert.equal((await telemetry()).status,'unavailable');
 assert.equal((await fetch(url+'/__demo/scenario?state=writing',{method:'POST'})).status,409);
 const resumed=await power('leave');assert.equal(resumed.status,200);
 assert.ok(Object.values((await resumed.json()).services).every(value=>value==='active'));
 await fetch(url+'/__demo/scenario?state=idle',{method:'POST'});
 const buckets=await fetch(url+scope+'/storage/buckets').then(r=>r.json());
 assert.equal(buckets.length,2);
 const current=await fetch(url+scope+'/storage?destination_id='+buckets[0].id).then(r=>r.json());
 const previous=await fetch(url+scope+'/storage?destination_id='+buckets[1].id).then(r=>r.json());
 assert.equal(current.demo,true);assert.deepEqual(current.inventory.cartridges.map(c=>c.barcode),['CVT003L8','CVT004L8']);
 assert.deepEqual(previous.inventory.cartridges.map(c=>c.barcode),['CVT002L8','CVT006L8']);
 assert.equal((await fetch(url+scope+'/storage?destination_id=unknown')).status,404);
 const options=await fetch(url+scope+'/cartridges/options').then(r=>r.json());
 assert.equal(options.ready,true);assert.deepEqual(options.media_types,['LTO7','LTO8']);
 const payload={barcode:'NEW001L8',media_type:'LTO8',capacity_bytes:4*1024**3,segment_size_mib:1048576,multipart_part_size_mib:256};
 const create=body=>fetch(url+scope+'/cartridges',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
 assert.equal((await create({...payload,segment_size_mib:0})).status,400);
 assert.equal((await create({...payload,multipart_part_size_mib:16})).status,400);
 const created=await create(payload);assert.equal(created.status,201);
 const createdBody=await created.json();assert.equal(createdBody.segment_size_mib,1048576);assert.equal(createdBody.multipart_part_size_mib,256);
 assert.equal((await create(payload)).status,409);
 assert.equal((await telemetry()).slots.find(s=>s.barcode===payload.barcode).number,10);
 assert.equal(upstream,0);
 assert.equal((await fetch(url+'/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges').then(r=>r.json()))[2].state,'verified');
 const batchRequest={request_id:'a'.repeat(32),count:2,media_type:'LTO8',capacity_bytes:1024**3,segment_size_mib:256,multipart_part_size_mib:256};
 const batchStep=()=>fetch(url+scope+'/cartridges/batch',{method:'POST',headers:{'Content-Type':'application/json','x-csrf-token':'demo-only'},body:JSON.stringify(batchRequest)});
 const first=await batchStep();assert.equal(first.status,200);assert.equal((await first.json()).completed,1);
 const completed=await batchStep();assert.equal((await completed.json()).phase,'completed');
 assert.equal((await (await batchStep()).json()).completed,2);
 console.log('PASS demo: local auth, telemetry, in-memory mutations, no OIDC redirect or upstream fallback, loopback only');
}finally{handler.close();await new Promise(r=>server.close(r));}

function invoke(handler,path,method='GET') {
 let result;
 const request={url:path,method,headers:{},resume(){}};
 const response={writeHead(status){this.status=status;},end(body){result={status:this.status,body:JSON.parse(body)};}};
 assert.equal(handler(request,response),true);return result;
}
const demoVtl='00000000-0000-4000-8000-000000000001';
const historyPath=`/v1/vtls/${demoVtl}/telemetry/history`;
const selected='?barcode=CVT001L8&drive_id=11';
const directory=mkdtempSync(join(tmpdir(),'vaultcenter-demo-history-'));
try {
 let clock=1800000000000;
 const file=join(directory,'history.json');
 const empty=createDemoHandler({seedExports:false,now:()=>clock,historyFile:file,autoCollect:false});
 const before=invoke(empty,historyPath+selected).body;
 assert.equal(before.status,'unavailable');assert.equal(before.retention_days,30);
 assert.equal(before.points.length,24);assert.ok(before.points.every(point=>point.writeRate===null&&point.localGrowth===null));
 assert.deepEqual(before.series,[]);empty.close();
 let tick=null;let cancelled=0;let unref=0;
 // Without the seeded S3 uploads, so the history holds only the drive series.
 const collected=createDemoHandler({seedExports:false,now:()=>clock,historyFile:file,
  schedule(callback,interval){assert.equal(interval,5000);tick=callback;return {unref(){unref++;}};},cancel(){cancelled++;}});
 const initialSnapshot=readFileSync(file,'utf8');
 // No HTTP request takes place during these background ticks.
 clock+=5000;tick();clock+=10000;tick();clock+=5000;
 assert.equal(unref,1);assert.equal(readFileSync(file,'utf8'),initialSnapshot,'5-second sampling must append, not rewrite month-long snapshots');
 assert.ok(statSync(file+'.journal').size>0);assert.equal(statSync(file+'.journal').mode&0o777,0o600);
 const first=invoke(collected,historyPath+selected).body;
 assert.equal(first.status,'available');assert.equal(first.source,'demo');
 assert.equal(first.series[0].drive_id,'11');assert.equal(first.series[0].barcode,'CVT001L8');
 const observed=first.points.filter(point=>point.writeRate!==null);assert.equal(observed.length,3);
 assert.ok(first.points.slice(-3,-1).some(point=>point.writeRate===null));
 assert.equal(statSync(file).mode&0o777,0o600);
 const compact=JSON.parse(readFileSync(file,'utf8'));
 assert.equal(compact.version,2);assert.equal(compact.series.length,1);assert.ok(compact.samples.every(row=>Array.isArray(row)&&row.length===4));
 collected.close();collected.close();assert.equal(cancelled,1);
 // Model a crash after snapshot replacement but before log truncation, followed
 // by an interrupted final append. Replay deduplicates and discards only suffix.
 const firstRow=compact.samples[0];
 appendFileSync(file+'.journal',JSON.stringify({version:1,source:'demo',samples:[{...compact.series[firstRow[0]],at:firstRow[1],writeRate:firstRow[2],localGrowth:firstRow[3]}]})+'\n'+ '{"version":');
 const restarted=createDemoHandler({seedExports:false,now:()=>clock,historyFile:file,autoCollect:false});
 assert.deepEqual(invoke(restarted,historyPath+selected).body,first);
 assert.equal(invoke(restarted,historyPath).body.points.length,0);
 assert.equal(invoke(restarted,historyPath+'?window_seconds=123').status,400);
 assert.equal(invoke(restarted,historyPath+`?end_ms=${clock+1}`).status,400);
 assert.equal(invoke(restarted,historyPath+'?kind=foreign').status,400);
 for(const query of ['extra=1','barcode=..%2Fsecret','drive_id=%0A','transfer_id=oops','barcode=','kind=pbs&kind=s3'])assert.equal(invoke(restarted,historyPath+'?'+query).status,400);
 assert.equal(invoke(restarted,'/v1/vtls/foreign/telemetry/history'+selected).status,404);
 for(const seconds of [120,3600,86400,604800,2592000]) {
  const response=invoke(restarted,historyPath+selected+`&window_seconds=${seconds}`).body;
  assert.ok(response.points.length<=600);assert.equal(response.end_ms-response.start_ms,seconds*1000);
 }
 invoke(restarted,'/__demo/scenario?state=stale','POST');clock+=5000;restarted.collectHistory();
 assert.equal(invoke(restarted,historyPath+selected).body.series[0].last_at,first.series[0].last_at);
 invoke(restarted,'/__demo/scenario?state=s3-export','POST');clock+=5000;restarted.collectHistory();
 const transfer=invoke(restarted,`/v1/vtls/${demoVtl}/telemetry`).body.demo_transfer;
 const s3=invoke(restarted,historyPath+'?kind=s3&barcode=CVT001L8').body;
 assert.equal(s3.series[0].transfer_id,transfer.id);assert.ok(s3.points.some(point=>point.writeRate>0));
 assert.ok(s3.points.every(point=>point.localGrowth===null));restarted.close();
 const third=createDemoHandler({seedExports:false,now:()=>clock,historyFile:file,autoCollect:false});
 invoke(third,'/__demo/scenario?state=s3-export','POST');
 const nextTransfer=invoke(third,`/v1/vtls/${demoVtl}/telemetry`).body.demo_transfer;
 assert.notEqual(nextTransfer.id,transfer.id);clock+=5000;third.collectHistory();
 assert.equal(invoke(third,historyPath+'?kind=s3').body.series.length,2);
 const previousTransfer=invoke(third,historyPath+`?kind=s3&barcode=CVT001L8&transfer_id=${transfer.id}`).body;
 assert.equal(previousTransfer.points.filter(point=>point.writeRate!==null).length,1);
 clock+=3600000;
 const old=invoke(third,historyPath+selected).body;
 assert.equal(old.status,'unavailable');assert.ok(old.series.some(series=>series.drive_id==='11'));
 assert.ok(old.points.every(point=>point.writeRate===null));third.collectHistory();
 assert.equal(JSON.parse(readFileSync(file,'utf8')).compacted_at,clock);assert.equal(statSync(file+'.journal').size,0);third.close();
 clock+=31*86400000;
 const purged=createDemoHandler({seedExports:false,now:()=>clock,historyFile:file,autoCollect:false});
 assert.deepEqual(invoke(purged,historyPath).body.series,[]);purged.close();
 assert.deepEqual(JSON.parse(readFileSync(file,'utf8')).samples,[]);

 // Average metrics separately and never mix drives, cartridges, or VTLs.
 const store=createDemoHistory({now:()=>clock,retentionDays:30});
 const base={vtl_id:demoVtl,kind:'pbs',drive_id:'11',barcode:'CVT001L8',transfer_id:null};
 store.record([{...base,at:clock-2000,writeRate:10,localGrowth:null},{...base,at:clock-1000,writeRate:30,localGrowth:8},
  {...base,drive_id:'12',at:clock,writeRate:99,localGrowth:null},
  {...base,barcode:'CVT002L8',at:clock,writeRate:1000,localGrowth:1000},
  {...base,vtl_id:'other',at:clock,writeRate:9999,localGrowth:9999}]);
 const average=store.query(demoVtl,new URLSearchParams('barcode=CVT001L8&drive_id=11'));
 assert.equal(average.points.at(-1).writeRate,20);assert.equal(average.points.at(-1).localGrowth,8);
 const latest=store.query(demoVtl,new URLSearchParams('barcode=CVT001L8'));
 assert.equal(latest.points.at(-1).writeRate,99);assert.equal(latest.series.length,3);
 assert.ok(latest.series.some(series=>series.barcode==='CVT002L8'));
 const legacyFile=join(directory,'legacy.json');
 const legacy={version:1,source:'demo',samples:[{...base,at:clock,writeRate:25,localGrowth:3}]};
 writeFileSync(legacyFile,JSON.stringify(legacy));
 const legacyStore=createDemoHistory({file:legacyFile,now:()=>clock});
 assert.equal(legacyStore.query(demoVtl,new URLSearchParams('barcode=CVT001L8')).points.at(-1).writeRate,25);
 legacyStore.record([{...base,at:clock+1,writeRate:26,localGrowth:4}]);
 assert.equal(JSON.parse(readFileSync(legacyFile,'utf8')).version,2);
 const foreignFile=join(directory,'physical.json');
 writeFileSync(foreignFile,JSON.stringify({...legacy,source:'physical'}));
 assert.throws(()=>createDemoHistory({file:foreignFile,now:()=>clock}),/Invalid demo history/);
 assert.equal(JSON.parse(readFileSync(foreignFile,'utf8')).source,'physical');
 console.log('PASS demo history: background collection, restart persistence, private file, retention, null gaps, aggregation, drive/VTL/transfer isolation');
}finally{rmSync(directory,{recursive:true,force:true});}
