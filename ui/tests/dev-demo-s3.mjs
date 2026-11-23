import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {createDemoHandler} from '../dev-demo.mjs';

// The demo simulates the S3 side end to end through the real routes: uploads that finish
// or fail, OVH archive and recall of a bucket, and local writes after a copy. No PBS, mhvtl
// or S3 is contacted; the clock is advanced instead of waited for.
let now=1800000000000;
const handler=createDemoHandler({now:()=>now,autoCollect:false});
const server=createServer((req,res)=>{if(!handler(req,res)){res.writeHead(502);res.end('{}');}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const url=`http://127.0.0.1:${server.address().port}`;
const scope=`${url}/v1/vtls/00000000-0000-4000-8000-000000000001`;
const MiB=1024**2;
const get=path=>fetch(scope+path).then(r=>r.json());
const post=(path,body,headers={})=>fetch(scope+path,{method:'POST',headers:{'Content-Type':'application/json',...headers},body:body===undefined?undefined:JSON.stringify(body)});
const csrf={'X-CSRF-Token':'demo-only'};
const demo=path=>fetch(url+path,{method:'POST'});
const tapes=async()=>Object.fromEntries((await get('/cartridges')).map(t=>[t.barcode,t]));
const observed=async barcode=>(await get('/telemetry')).media.find(m=>m.barcode===barcode);
const exports=async()=>(await get('/jobs')).filter(j=>j.kind==='export');
try {
 // The dev server asks for a busier library: one more verified tape and five more waiting.
 {
  const busy=createDemoHandler({now:()=>now,autoCollect:false,crowded:true});
  const other=createServer((req,res)=>{if(!busy(req,res)){res.writeHead(502);res.end('{}');}});
  await new Promise(r=>other.listen(0,'127.0.0.1',r));
  try {
   const from=path=>fetch(`http://127.0.0.1:${other.address().port}/v1/vtls/00000000-0000-4000-8000-000000000001${path}`).then(r=>r.json());
   const sends=(await from('/jobs')).filter(j=>j.kind==='export');
   assert.equal(sends.filter(j=>j.status==='queued').length,6);assert.equal(sends.filter(j=>j.status==='running').length,1);
   assert.equal(sends.filter(j=>j.status==='completed').length,5);
   assert.equal((await from('/cartridges')).length,15);
  } finally {await new Promise(r=>other.close(r));}
 }
 // Initial library: copies in an available and in a cold-archived bucket, one stale copy,
 // a failed send, one upload running and one waiting.
 let tape=await tapes();
 assert.equal(tape.CVT002L8.last_copy.bucket_state,'archived');
 assert.equal(tape.CVT004L8.last_copy.bucket_state,'available');
 assert.ok((await observed('CVT003L8')).local_size_bytes>tape.CVT003L8.last_copy.bytes,'CVT003L8 was written after its copy');
 assert.equal((await observed('CVT004L8')).local_size_bytes,tape.CVT004L8.last_copy.bytes);
 assert.equal(tape.CVT007L8.state,'failed');assert.equal(tape.CVT007L8.export_resumable,true);
 const running=(await exports()).find(j=>j.subject==='CVT008L8');
 assert.equal(running.status,'running');assert.ok(JSON.parse(running.message).export_progress.completed_bytes>0);
 assert.equal((await exports()).find(j=>j.subject==='CVT009L8').status,'queued');
 // Like the appliance, power-off waits for the uploads.
 await demo('/__demo/scenario?state=empty');
 assert.equal((await post('/maintenance/enter',undefined,csrf)).status,409);
 // Both uploads finish one after the other and leave verified copies in the active bucket.
 now+=130000;tape=await tapes();
 for(const barcode of ['CVT008L8','CVT009L8']){assert.equal(tape[barcode].state,'verified');assert.equal(tape[barcode].last_copy.bucket,'vaultcenter-demo');}
 assert.ok((await exports()).filter(j=>['CVT008L8','CVT009L8'].includes(j.subject)).every(j=>j.status==='completed'&&JSON.parse(j.message).receipt.integrity.status==='verified'));

 // The next send fails at 40 %; the tape becomes resumable and the retry succeeds.
 assert.equal((await demo('/__demo/fail-next?enabled=1')).status,200);
 assert.equal((await get('/telemetry')).demo_fail_next,true);
 assert.equal((await post('/cartridges/CVT005L8/export')).status,202);
 assert.equal((await get('/telemetry')).demo_fail_next,false,'only the next send fails');
 assert.equal((await tapes()).CVT005L8.state,'uploading');
 assert.equal((await post('/cartridges/CVT005L8/export')).status,409,'one job per cartridge');
 now+=60000;tape=await tapes();
 assert.equal(tape.CVT005L8.state,'failed');assert.equal(tape.CVT005L8.export_resumable,true);assert.equal(tape.CVT005L8.last_copy,null);
 assert.match((await exports()).find(j=>j.subject==='CVT005L8').message,/response error/);
 assert.equal((await post('/cartridges/CVT005L8/export')).status,202);
 now+=60000;tape=await tapes();
 assert.equal(tape.CVT005L8.state,'verified');assert.equal(tape.CVT005L8.last_copy.bytes,520*MiB);
 // A verified tape is not sent again, as on the appliance.
 assert.equal((await post('/cartridges/CVT004L8/export')).status,409);

 // "Buckets et copies" lists each copy with what its bucket allows; the send dialog names the destination.
 const listed=await get('/storage/recovery?destination_id=demo-bucket-current');
 assert.deepEqual(listed.access,{readable:true,writable:true,raw_status:'None',supported:true});
 assert.ok(listed.copies.some(c=>c.barcode==='CVT005L8'&&c.logical_size_bytes===520*MiB&&/^[0-9a-f]{64}$/.test(c.manifest_sha256)));
 assert.equal((await get('/storage/recovery?destination_id=demo-bucket-previous')).access.raw_status,'Archived');
 assert.equal((await get('/storage/schedule')).active_bucket,'vaultcenter-demo');
 // Archiving the active bucket: confirmations, rotation to the next bucket, OVH progress.
 const current=await get('/storage?destination_id=demo-bucket-current');
 assert.equal(current.supported,true);assert.equal(current.raw_status,'None');assert.ok(current.next_bucket);
 const archive={confirm_bucket:'vaultcenter-demo',acknowledge_billing:true,exclusive_bucket:true,next_bucket:current.next_bucket};
 assert.equal((await post('/storage/archive?destination_id=demo-bucket-current',archive)).status,403);
 assert.equal((await post('/storage/archive?destination_id=demo-bucket-current',{...archive,acknowledge_billing:false},csrf)).status,400);
 assert.equal((await post('/storage/archive?destination_id=demo-bucket-current',archive,csrf)).status,202);
 const buckets=await get('/storage/buckets');
 assert.equal(buckets.find(b=>b.active).bucket,current.next_bucket);
 assert.equal((await tapes()).CVT004L8.last_copy.bucket_state,'archiving');
 assert.equal((await get('/storage?destination_id=demo-bucket-current')).raw_status,'Archiving');
 now+=30000;
 assert.equal((await tapes()).CVT004L8.last_copy.bucket_state,'archived');
 assert.equal((await get('/storage?destination_id=demo-bucket-current')).raw_status,'Archived');
 const cold=await get('/storage/recovery?destination_id=demo-bucket-current');
 assert.equal(cold.access.readable,false);assert.ok(cold.copies.every(c=>c.raw_status==='Archived'));
 assert.equal((await get('/storage/schedule')).active_bucket,current.next_bucket);
 const jobs=await get('/jobs');
 assert.ok(jobs.some(j=>j.kind==='archive_submit'&&j.status==='completed'));
 assert.ok(jobs.some(j=>j.kind==='archive_refresh'&&j.destination_id==='demo-bucket-current'));
 assert.equal((await get('/storage/archive?destination_id=demo-bucket-current')).operations[0].phase,'archived');
 // New sends go to the next bucket.
 assert.equal((await post('/cartridges/CVT001L8/export')).status,202);
 assert.equal((await exports()).find(j=>j.subject==='CVT001L8').destination_id,buckets.find(b=>b.active).id);
 now+=120000;

 // Recall: in progress, then "Vérifier la demande chez OVH" brings the answer now.
 assert.equal((await post('/storage/recall?destination_id=demo-bucket-current',{confirm_bucket:'vaultcenter-demo',acknowledge_retrieval_cost:true},csrf)).status,202);
 assert.equal((await tapes()).CVT004L8.last_copy.bucket_state,'recalling');
 assert.equal((await post('/storage/archive/refresh?destination_id=demo-bucket-current',{},csrf)).status,200);
 assert.equal((await tapes()).CVT004L8.last_copy.bucket_state,'recalled');
 assert.equal((await get('/storage?destination_id=demo-bucket-current')).raw_status,'Restored');
 assert.ok((await get('/storage/archive?destination_id=demo-bucket-current')).operations[0].restored_until);

 // Writing to a copied tape: load it, write, and its local volume exceeds the copy.
 assert.equal((await demo('/__demo/drive?barcode=CVT004L8')).status,200);
 await demo('/__demo/scenario?state=writing');now+=5000;
 const live=await get('/telemetry');
 assert.equal(live.drives[0].barcode,'CVT004L8');assert.equal(live.demo_drive,'CVT004L8');
 assert.equal(live.slots.find(s=>s.number===4).barcode,null);assert.equal(live.slots.find(s=>s.number===1).barcode,'CVT001L8');
 assert.equal((await observed('CVT004L8')).local_size_bytes,(await tapes()).CVT004L8.last_copy.bytes+5*MiB);
 await demo('/__demo/scenario?state=idle');now+=5000;
 assert.equal((await observed('CVT004L8')).local_size_bytes,(await tapes()).CVT004L8.last_copy.bytes+5*MiB,'writes stop with the scenario and are kept');
 assert.equal((await demo('/__demo/drive?barcode=NOPE01L8')).status,404);
 console.log('PASS demo S3 simulation: uploads, failure and retry, verified tapes not resent, archive with rotation, recall, writes after a copy');
} finally { handler.close(); await new Promise(r=>server.close(r)); }
