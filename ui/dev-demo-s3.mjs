// In-memory simulation of the S3 side of the local demonstration: export jobs, the
// copies they leave, OVH archive and recall requests per bucket, and local writes.
// Nothing here contacts PBS, mhvtl or S3. Every transition is derived from the clock,
// so tests advance time instead of waiting.
import {demoId,demoMedia} from './tests/helpers/telemetry-fixtures.mjs';
import {demoBuckets} from './tests/helpers/storage-fixtures.mjs';
import {createHash} from 'node:crypto';

const MiB=1024**2;
export const UPLOAD_BPS=12*MiB;        // constant for completion times; the shown rate wobbles around it
export const WRITE_BPS=MiB;            // local growth while the drive scenario writes
const ARCHIVE_VERIFY_MS=4000,ARCHIVE_OVH_MS=20000,RECALL_SUBMIT_MS=3000,RECALL_OVH_MS=20000;
const RECALL_READABLE_MS=30*86400000;
const FAIL_AT=0.4;                     // a failing upload stops at 40 %
const minutes=n=>n*60000;

// Initial library: one tape per sticker state, plus tapes left to send.
// [barcode, state, local bytes, copy destination or null, copy bytes, extra local writes after the copy]
const SEED=[
 ['CVT001L8','writable',850*MiB,null],                                  // in the drive, written by PBS
 ['CVT002L8','verified',600*MiB,'demo-bucket-previous',600*MiB,0],       // copy in the cold-archived bucket
 ['CVT003L8','verified',30*MiB,'demo-bucket-current',30*MiB,10*MiB],     // written again after its copy
 ['CVT004L8','verified',300*MiB,'demo-bucket-current',300*MiB,0],        // copy available on S3
 ['CVT005L8','writable',520*MiB,null],                                  // ready to send
 ['CVT006L8','verified',1200*MiB,'demo-bucket-previous',1200*MiB,0],
 ['CVT007L8','failed',700*MiB,null],                                    // last send failed, resumable
 ['CVT008L8','uploading',900*MiB,null],                                 // sending now
 ['CVT009L8','uploading',650*MiB,null],                                 // waiting its turn
];

// A busier library for the dev server: one more verified tape and five more waiting.
const CROWD=[
 ['CVT010L8','verified',450*MiB,'demo-bucket-current',450*MiB,0],
 ...[11,12,13,14,15].map(n=>[`CVT0${n}L8`,'uploading',(n-9)*120*MiB,null]),
];

export function createS3Simulation({now,seed=true,crowded=false}) {
 const start=now();
 const buckets=demoBuckets.map(b=>({...b}));
 let bucketSerial=buckets.length;
 const tapes=demoMedia().map(base=>{
  const row=SEED.find(r=>r[0]===base.barcode);
  const inFlight=row&&['uploading','failed'].includes(row[1]);
  return {...base,state:row?(inFlight&&!seed?'writable':row[1]):base.state,bytes_written:row?.[2]??base.bytes_written,export_resumable:seed&&row?.[1]==='failed'};
 });
 if(seed&&crowded)for(const row of CROWD)tapes.push({barcode:row[0],capacity_bytes:2*1024**3,bytes_written:row[2],generation:1,state:row[1],export_resumable:false});
 const seeded=seed&&crowded?[...SEED,...CROWD]:SEED;
 const written=new Map(seeded.filter(r=>r[5]).map(r=>[r[0],r[5]]));
 const copies=seeded.filter(r=>r[3]).map(r=>({barcode:r[0],destination:r[3],generation:1,bytes:r[4],checked_at:new Date(start-(r[3]==='demo-bucket-previous'?minutes(4320):minutes(r[0]==='CVT003L8'?360:50))).toISOString()}));
 const jobs=[];   // newest last; internal fields start with an underscore
 const operations=[];
 let failNext=false,serial=0;
 const id=prefix=>`${prefix}-${(++serial).toString().padStart(4,'0')}`;

 function receipt(job) {
  const segments=Math.max(1,Math.ceil(job._total/(256*MiB)));
  return JSON.stringify({status:'verified',receipt:{scope:{job_id:job.id,vtl_id:demoId,destination_id:job.destination_id,configuration_revision:1},generation:{barcode:job.subject,generation:job.generation,logical_size_bytes:job._total,segment_count:segments},integrity:{status:'verified',method:'s3_sha256',verified_at:Math.floor(Date.parse(job.updated_at)/1000),verified_segments:segments,verified_bytes:job._total}}});
 }
 function exportJob({barcode,destination,total,created,status='queued',failAt=null}) {
  const at=new Date(created).toISOString();
  const job={id:id('demo-export'),kind:'export',subject:barcode,destination_id:destination,configuration_revision:1,generation:1,status,progress:0,created_at:at,updated_at:at,message:null,_total:total,_started:null,_failAt:failAt};
  jobs.push(job);return job;
 }
 function finishedJob(kind,destination,subject,at,message) {
  const time=new Date(at).toISOString();
  jobs.push({id:id(`demo-${kind}`),kind,subject,destination_id:destination,configuration_revision:1,generation:null,status:'completed',progress:100,created_at:time,updated_at:time,message});
 }
 if(seed) {
  for(const copy of copies){const job=exportJob({barcode:copy.barcode,destination:copy.destination,total:copy.bytes,created:Date.parse(copy.checked_at)-minutes(20),status:'completed'});job.progress=100;job.updated_at=copy.checked_at;job.message=receipt(job);}
  const failed=exportJob({barcode:'CVT007L8',destination:'demo-bucket-current',total:700*MiB,created:start-minutes(130),status:'failed'});
  failed.progress=100;failed.message='Error: Request("response error")';
  const running=exportJob({barcode:'CVT008L8',destination:'demo-bucket-current',total:900*MiB,created:start-minutes(2),status:'running'});
  running._started=start-30000;
  exportJob({barcode:'CVT009L8',destination:'demo-bucket-current',total:650*MiB,created:start-minutes(1)});
  if(crowded)CROWD.filter(r=>r[1]==='uploading').forEach((r,i)=>exportJob({barcode:r[0],destination:'demo-bucket-current',total:r[2],created:start-50000+i*1000}));
  const archived=start-minutes(2880);
  operations.push({job_id:id('demo-archive_submit'),prepare_job_id:id('demo-prepare'),phase:'archived',operation:'archive',bucket:'vaultcenter-demo-precedent',destination:'demo-bucket-previous',submitted_at:archived,done_at:archived+ARCHIVE_OVH_MS,restored_until:null});
 }
 jobs.sort((a,b)=>a.created_at.localeCompare(b.created_at));

 // Apply every transition due by `time`, in order: an upload ends, the next one starts.
 function advance(time) {
  for(;;) {
   const running=jobs.find(j=>j.kind==='export'&&j.status==='running');
   if(running) {
    const stopBytes=running._failAt??running._total;
    const end=running._started+stopBytes/UPLOAD_BPS*1000;
    if(end>time)break;
    const tape=tapes.find(t=>t.barcode===running.subject);
    running.updated_at=new Date(end).toISOString();running.progress=100;
    if(running._failAt!==null) {
     running.status='failed';running.message='Error: Request("response error")';
     if(tape){tape.state='failed';tape.export_resumable=true;}
    } else {
     running.status='completed';running.message=receipt(running);
     const copy={barcode:running.subject,destination:running.destination_id,generation:running.generation,bytes:running._total,checked_at:running.updated_at};
     copies.splice(0,copies.length,...copies.filter(c=>c.barcode!==copy.barcode),copy);
     if(tape){tape.state='verified';tape.export_resumable=false;}
    }
    const next=jobs.find(j=>j.kind==='export'&&j.status==='queued');
    if(next){next.status='running';next._started=end;}
    continue;
   }
   const queued=jobs.find(j=>j.kind==='export'&&j.status==='queued');
   if(queued&&Date.parse(queued.created_at)<=time){queued.status='running';queued._started=Math.max(Date.parse(queued.created_at),start);continue;}
   break;
  }
  for(const op of operations) {
   if(op.phase==='queued'&&op.accepted_at<=time)op.phase='accepted';
   if(op.phase==='accepted'&&op.done_at<=time) {
    op.phase=op.operation==='archive'?'archived':'restored';
    if(op.operation==='recall')op.restored_until=new Date(op.done_at+RECALL_READABLE_MS).toISOString();
    finishedJob('archive_refresh',op.destination,op.bucket,op.done_at,JSON.stringify({status:op.phase,bucket:op.bucket}));
   }
  }
  for(const job of jobs)if(/^(archive|recall)_submit$/.test(job.kind)&&job.status==='running'&&job._done<=time){job.status='completed';job.progress=100;job.updated_at=new Date(job._done).toISOString();}
 }
 const tape=barcode=>tapes.find(t=>t.barcode===barcode);
 const localSize=barcode=>(tape(barcode)?.bytes_written??0)+(written.get(barcode)??0);
 const activeJobs=()=>jobs.filter(j=>['queued','running'].includes(j.status));
 const latestOperation=destination=>operations.filter(o=>o.destination===destination).at(-1);

 function bucketState(destination,time) {
  const op=latestOperation(destination);
  if(!op)return 'available';
  if(op.operation==='archive')return op.phase==='archived'?'archived':'archiving';
  if(op.phase==='restored')return Date.parse(op.restored_until)>time?'recalled':'archived';
  return 'recalling';
 }
 const rawStatus={available:'None',archiving:'Archiving',archived:'Archived',recalling:'Restoring',recalled:'Restored'};

 function view(job,time) {
  const visible=Object.fromEntries(Object.entries(job).filter(([key])=>!key.startsWith('_')));
  if(job.kind==='export'&&job.status==='running') {
   const sent=Math.min(job._failAt??job._total,Math.floor(Math.max(0,time-job._started)/1000*UPLOAD_BPS));
   return {...visible,updated_at:new Date(time).toISOString(),message:JSON.stringify({export_progress:{phase:'uploading',completed_bytes:sent,total_bytes:job._total}})};
  }
  if(job.kind==='export'&&job.status==='queued')return {...visible,message:JSON.stringify({export_progress:{phase:'preparing',completed_bytes:0,total_bytes:job._total}})};
  return visible;
 }
 const nextBucketName=()=>`vaultcenter-demo-${String(bucketSerial+1).padStart(2,'0')}`;
 function bucket(destination){return buckets.find(b=>b.id===destination);}

 return {
  advance,
  /** Cartridges as the catalogue lists them, each with its latest verified copy. */
  cartridges(time) {
   advance(time);
   return tapes.map(t=>{
    const copy=copies.find(c=>c.barcode===t.barcode);
    return {...t,last_copy:copy?{bucket:bucket(copy.destination)?.bucket??null,generation:copy.generation,checked_at:copy.checked_at,bytes:copy.bytes,bucket_state:bucketState(copy.destination,time)}:null};
   });
  },
  addTape(item){tapes.push({...item,export_resumable:false});},
  hasTape:barcode=>!!tape(barcode),
  tapeCount:()=>tapes.length,
  jobs(time){advance(time);return jobs.map(j=>view(j,time)).reverse();},
  busy(time){advance(time);return activeJobs().length>0;},
  /** Observed local size: catalogue bytes plus what was written since. */
  localSize,
  write(barcode,bytes){if(tape(barcode)&&bytes>0)written.set(barcode,(written.get(barcode)??0)+bytes);},
  failNext:()=>failNext,
  setFailNext(value){failNext=value;},
  /** Upload throughput samples for the S3 shelf history. */
  samples(time) {
   advance(time);
   return jobs.filter(j=>j.kind==='export'&&j.status==='running').map(j=>({vtl_id:demoId,kind:'s3',drive_id:'s3',barcode:j.subject,transfer_id:j.id,at:time,writeRate:Math.round(UPLOAD_BPS*(1+0.2*Math.sin((time-j._started)/7000))),localGrowth:null}));
  },
  seal(barcode,time) {
   advance(time);const item=tape(barcode);
   if(!item)return {status:404,body:{error:'Cartouche absente'}};
   if(item.state!=='writable')return {status:409,body:{error:'Seule une cartouche inscriptible peut être scellée.'}};
   item.state='sealed';item.bytes_written=localSize(barcode);written.delete(barcode);
   return {status:200,body:item};
  },
  exportTape(barcode,time) {
   advance(time);const item=tape(barcode);
   if(!item)return {status:404,body:{error:'Cartouche absente'}};
   if(activeJobs().some(j=>j.subject===barcode))return {status:409,body:{error:'another job is active for this cartridge'}};
   if(!(['writable','sealed'].includes(item.state)||(item.state==='failed'&&item.export_resumable)))return {status:409,body:{error:'Cette cartouche n’est pas disponible pour un envoi. Consultez son état dans la bibliothèque.'}};
   const destination=buckets.find(b=>b.active)?.id;
   if(!destination)return {status:409,body:{error:'Aucune destination S3 active'}};
   // Sealing captures the measured local size; later writes are new local data.
   const total=Math.max(MiB,localSize(barcode));
   item.bytes_written=total;written.delete(barcode);item.state='uploading';item.export_resumable=false;
   const job=exportJob({barcode,destination,total,created:time,failAt:failNext?Math.floor(total*FAIL_AT):null});
   failNext=false;
   advance(time);
   return {status:202,body:view(job,time)};
  },
  buckets(time) {
   advance(time);
   return buckets.map(b=>({...b,copy_count:copies.filter(c=>c.destination===b.id).length,observed_status:rawStatus[bucketState(b.id,time)]}));
  },
  hasBucket:destination=>!!bucket(destination),
  /** The bucket report of the Stockage S3 page, presented as an OVH-managed bucket. */
  report(destination,time) {
   advance(time);const b=bucket(destination);if(!b)return null;
   const state=bucketState(destination,time),held=copies.filter(c=>c.destination===destination);
   const cartridges=held.map(c=>({barcode:c.barcode,objects:Math.max(1,Math.ceil(c.bytes/(256*MiB)))+2,bytes:c.bytes}));
   return {demo:true,destination_id:destination,active:b.active,managed:true,next_bucket:b.active?nextBucketName():null,bucket:b.bucket,prefix:'vaultcenter',region:'rbx-archive',provider:'ovh',provider_label:'OVH Object Storage · simulation',supported:true,raw_status:rawStatus[state],
    state_label:{available:'Disponible',archiving:'Archivage en cours',archived:'Archivé',recalling:'Rappel en cours',recalled:'Rappelé'}[state],
    object_access:['available','recalled'].includes(state)?'Lecture et écriture simulées':'Liste des objets uniquement',archive_lock_until:null,multipart_uploads:0,checked_at:Math.floor(time/1000),checks:[],
    catalogue_generations:held.map(c=>({barcode:c.barcode,generation:c.generation,key:`vaultcenter/cartridges/${c.barcode}/manifests/${c.generation}.json`,checked_at:c.checked_at})),
    inventory:{complete:true,objects:cartridges.reduce((s,c)=>s+c.objects,0),bytes:cartridges.reduce((s,c)=>s+c.bytes,0),outside_prefix:0,unrecognized_objects:0,cartridges,manifests:held.map(c=>({barcode:c.barcode,generation:c.generation,key:`vaultcenter/cartridges/${c.barcode}/manifests/${c.generation}.json`}))}};
  },
  /** "Buckets et copies": every verified copy of a bucket, with what its OVH state allows. */
  recovery(destination,time) {
   advance(time);const b=bucket(destination);if(!b)return null;
   const state=bucketState(destination,time),raw=rawStatus[state];
   const readable=['available','recalled'].includes(state),writable=state==='available';
   return {destination_id:destination,bucket:b.bucket,configuration_revision:1,errors:[],complete:true,access:{readable,writable,raw_status:raw,supported:true},
    copies:copies.filter(c=>c.destination===destination).map(c=>({barcode:c.barcode,generation:c.generation,manifest_sha256:createHash('sha256').update(`${destination}/${c.barcode}/${c.generation}`).digest('hex'),logical_size_bytes:c.bytes,manifest_key:`vaultcenter/cartridges/${c.barcode}/manifests/${c.generation}.json`,file_count:5,segment_count:Math.max(1,Math.ceil(c.bytes/(256*MiB))),format_version:3,raw_status:raw,writable,supported:true,configuration_revision:1,deletion_status:null}))};
  },
  activeBucket(time){advance(time);return buckets.find(b=>b.active)?.bucket??null;},
  operations(destination,time) {
   advance(time);
   return {configured:true,operations:operations.filter(o=>o.destination===destination).map(o=>({job_id:o.job_id,prepare_job_id:o.prepare_job_id,phase:o.phase,operation:o.operation,bucket:o.bucket,observed_status:o.phase==='archived'?'Archived':o.phase==='restored'?'Restored':o.phase==='accepted'?(o.operation==='archive'?'Archiving':'Restoring'):null,observed_at:o.phase==='queued'?null:new Date(Math.min(time,o.done_at)).toISOString(),restored_until:o.restored_until})).reverse()};
  },
  archive(destination,body,time) {
   advance(time);const b=bucket(destination);
   if(!b)return {status:404,body:{error:'Bucket absent'}};
   if(body?.confirm_bucket!==b.bucket||body?.acknowledge_billing!==true||body?.exclusive_bucket!==true)return {status:400,body:{error:'Confirmer le bucket et les conditions OVH.'}};
   if(activeJobs().length)return {status:409,body:{error:'Attendez la fin des opérations en cours pour archiver ce bucket.'}};
   if(bucketState(destination,time)!=='available')return {status:409,body:{error:'Ce bucket n’est pas disponible pour un archivage.'}};
   // Archiving the active bucket first moves new exports to the next one.
   if(b.active) {
    if(body.next_bucket!==nextBucketName())return {status:409,body:{error:'Le nom du prochain bucket a changé. Rouvrez la confirmation.'}};
    b.active=false;bucketSerial++;
    buckets.unshift({id:`demo-bucket-${bucketSerial}`,bucket:body.next_bucket,active:true,created_at:new Date(time).toISOString(),configuration_revision:1,copy_count:0,observed_status:null,rotation_phase:null});
   }
   const job={id:id('demo-archive_submit'),kind:'archive_submit',subject:b.bucket,destination_id:destination,configuration_revision:1,generation:null,status:'running',progress:10,created_at:new Date(time).toISOString(),updated_at:new Date(time).toISOString(),message:null,_done:time+ARCHIVE_VERIFY_MS};
   jobs.push(job);
   operations.push({job_id:job.id,prepare_job_id:id('demo-prepare'),phase:'queued',operation:'archive',bucket:b.bucket,destination,submitted_at:time,accepted_at:time+ARCHIVE_VERIFY_MS,done_at:time+ARCHIVE_VERIFY_MS+ARCHIVE_OVH_MS,restored_until:null});
   return {status:202,body:view(job,time)};
  },
  recall(destination,body,time) {
   advance(time);const b=bucket(destination);
   if(!b)return {status:404,body:{error:'Bucket absent'}};
   if(body?.confirm_bucket!==b.bucket||body?.acknowledge_retrieval_cost!==true)return {status:400,body:{error:'Confirmer le bucket et les frais de rappel.'}};
   if(bucketState(destination,time)!=='archived')return {status:409,body:{error:'Seul un bucket archivé peut être rappelé.'}};
   const job={id:id('demo-recall_submit'),kind:'recall_submit',subject:b.bucket,destination_id:destination,configuration_revision:1,generation:null,status:'running',progress:10,created_at:new Date(time).toISOString(),updated_at:new Date(time).toISOString(),message:null,_done:time+RECALL_SUBMIT_MS};
   jobs.push(job);
   operations.push({job_id:job.id,prepare_job_id:id('demo-prepare'),phase:'queued',operation:'recall',bucket:b.bucket,destination,submitted_at:time,accepted_at:time+RECALL_SUBMIT_MS,done_at:time+RECALL_SUBMIT_MS+RECALL_OVH_MS,restored_until:null});
   return {status:202,body:view(job,time)};
  },
  /** "Vérifier la demande chez OVH": OVH answers that the pending request is done. */
  refresh(destination,time) {
   advance(time);const op=latestOperation(destination);
   if(!op||!['queued','accepted'].includes(op.phase))return {status:409,body:{error:'Aucune demande en cours chez OVH.'}};
   op.accepted_at=Math.min(op.accepted_at,time);op.done_at=time;
   for(const job of jobs)if(job.id===op.job_id&&job.status==='running')job._done=time;
   advance(time);
   return {status:200,body:{phase:op.phase}};
  },
 };
}
