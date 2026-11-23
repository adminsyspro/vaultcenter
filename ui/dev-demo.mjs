import {randomUUID} from 'node:crypto';
import {createDemoHistory} from './dev-history.mjs';
import {demoId,telemetryFixture} from './tests/helpers/telemetry-fixtures.mjs';
import {createS3Simulation,WRITE_BPS} from './dev-demo-s3.mjs';
export function assertLoopback(hostname) {
 if(!['127.0.0.1','localhost','::1'].includes(hostname))throw Error('Demo binds only to loopback');
}
export function createDemoHandler({seedExports=true,crowded=false,now=Date.now,historyFile=null,retentionDays=30,autoCollect=true,schedule=setInterval,cancel=clearInterval}={}) {
 const sim=createS3Simulation({now,seed:seedExports,crowded});let scenario='writing';let transferStart=null;let transferId=null;
 // The tape the drive holds when a scenario loads it, and the clock of its last counted write.
 let driveBarcode='CVT001L8',lastWrite=now();
 const tapes=()=>sim.cartridges(now());
 function tickWrites(time) {
  if(powered&&scenario==='writing')sim.write(driveBarcode,Math.floor(Math.max(0,time-lastWrite)/1000*WRITE_BPS));
  lastWrite=time;
 }
 let bandwidth={vtl_id:demoId,revision:1,tape_write_bps:null,s3_bps:null,tape:{state:"unavailable",observed_revision:null},s3:{state:"configured"},message:"Démonstration : aucun transfert réel n’est limité."};
 let powered=true;const batches=new Map();
 const storage=()=>{const media=tapes();const reserved=media.reduce((sum,m)=>sum+m.capacity_bytes,0)+[...batches.values()].reduce((sum,b)=>sum+(b.request.count-b.completed)*b.request.capacity_bytes,0);return {total_bytes:96*1024**4,free_bytes:64*1024**4,reserved_bytes:reserved,available_bytes:Math.max(0,64*1024**4-reserved),overhead_per_cartridge:0};};
 const maintenanceEvents=[];
 const serviceNames=['mhvtl.target','vtllibrary@10.service','vtltape@11.service','tgt.service'];
 const serviceMap=value=>Object.fromEntries(serviceNames.map(name=>[name,value]));
 const powerStatus=()=>({configured:true,phase:powered?'online':'maintenance',ready:!powered,blocked:!powered,pbs_locked:!powered,services:serviceMap(powered?'active':'inactive'),original:powered?{}:serviceMap('active'),demo:true});
 let signedOut=false;
 const session={user:{id:'local-demo',username:'démo',role:'admin',issuer:null,disabled:false},csrf_token:'demo-only'};
 const history=createDemoHistory({file:historyFile,now,retentionDays});
 function snapshot() {
    const time=now();tickWrites(time);
    const media=tapes();
    const live=telemetryFixture(demoId,scenario==='s3-export'?'empty':scenario,time);
    if(!powered)return {...live,status:'unavailable',reason:'source_missing',drives:[],slots:[],media:[]};
    live.demo_scenario=scenario;live.demo_drive=driveBarcode;live.demo_fail_next=sim.failNext();
    const drive=live.drives[0];
    if(drive?.barcode){drive.barcode=driveBarcode;drive.origin_slot=media.findIndex(m=>m.barcode===driveBarcode)+1;}
    for(const slot of live.slots)slot.barcode=drive?.barcode&&media[slot.number-1]?.barcode===driveBarcode?null:media[slot.number-1]?.barcode??slot.barcode;
    for(const item of live.media){item.local_size_bytes=sim.localSize(item.barcode);item.local_growth_bps=scenario==='writing'&&drive?.barcode===item.barcode?WRITE_BPS:0;}
    if(scenario!=='missing')for(const [index,item] of media.entries()) {
     if(index<9)continue;
     if(scenario!=='unknown')live.slots.push({number:index+1,kind:'storage',barcode:item.barcode,position_known:true,observed_at:time});
     live.media.push({barcode:item.barcode,observed_at:time,local_size_bytes:sim.localSize(item.barcode),local_growth_bps:0,logical_used_bytes:null,logical_capacity_bytes:null});
    }
    if(transferStart!==null) {
     const elapsed=Math.max(0,(time-transferStart)/1000);
     const total=media[0].bytes_written;
     const transferred=Math.min(total,Math.floor((8*elapsed+10*(1-Math.cos(elapsed/5)))*1024**2));
     const complete=transferred===total;
     live.demo_transfer={id:transferId,barcode:media[0].barcode,bucket:'vaultcenter-demo',status:complete?'completed':'running',transferred_bytes:transferred,total_bytes:total,rate_bps:complete?0:Math.round((8+2*Math.sin(elapsed/5))*1024**2),observed_at:time};
    }
    if(scenario==='writing') {
     const seconds=time/1000;
     live.drives[0].write_rate_bps=Math.round((16+5*Math.sin(seconds/5)+2*Math.sin(seconds/1.7))*1024**2);
    }
  return live;
 }
 function collectHistory() {
  const time=now();const live=snapshot();const samples=[];
  if(live.status==='available'&&time-live.observed_at<=15000)for(const drive of live.drives) {
   if(!drive.barcode||time-drive.observed_at>15000)continue;
   const tape=live.media.find(item=>item.barcode===drive.barcode&&time-item.observed_at<=15000);
   const rate=value=>Number.isFinite(value)&&value>=0?value:null;
   const writeRate=rate(drive.write_rate_bps);const localGrowth=rate(tape?.local_growth_bps);
   if(writeRate===null&&localGrowth===null)continue;
   samples.push({vtl_id:demoId,kind:'pbs',drive_id:drive.id,barcode:drive.barcode,transfer_id:null,at:time,writeRate,localGrowth});
  }
  samples.push(...sim.samples(time));
  if(live.demo_transfer) {
   const transfer=live.demo_transfer;
   samples.push({vtl_id:demoId,kind:'s3',drive_id:'s3',barcode:transfer.barcode,transfer_id:transfer.id,at:time,writeRate:transfer.rate_bps,localGrowth:null});
  }
  history.record(samples);
 }
 let timer=null;
 const scheduledCollect=()=>{try{collectHistory();}catch(error){console.error(`Demo throughput history: ${error.message}`);}};
 if(autoCollect){scheduledCollect();timer=schedule(scheduledCollect,5000);timer?.unref?.();}
 function demo(req,res) {

  const url=new URL(req.url,'http://localhost');const path=url.pathname.replace(/\/$/,'');
  if(!/^\/(auth(?:\/|$)|v1(?:\/|$)|health$|__demo(?:\/|$))/.test(path))return false;
  const send=(body,status=200)=>{res.writeHead(status,{'Content-Type':'application/json','Cache-Control':'no-store','X-VaultCenter-Demo':'1'});res.end(JSON.stringify(body));};
  if(req.headers.origin&&req.headers.origin!==`http://${req.headers.host}`){send({error:'Origine refusée'},403);return true;}
  // Only the cartridge form consumes a bounded JSON body; no request leaves this handler.
  if([`/v1/vtls/${demoId}/cartridges`,`/v1/vtls/${demoId}/cartridges/batch`].includes(path)&&req.method==='POST') {
   let body='';let rejected=false;
   req.on('data',chunk=>{body+=chunk;if(body.length>4096&&!rejected){rejected=true;send({error:'Requête trop volumineuse'},413);}});
   req.on('end',()=>{
    if(rejected)return;
    try {
     const input=JSON.parse(body);
     const isBatch=path.endsWith('/batch');let batch;
     if(isBatch){
      if(req.headers['x-csrf-token']!==session.csrf_token){send({error:'Jeton CSRF requis'},403);return;}
      if(!/^[a-f0-9]{32}$/.test(input.request_id)||!Number.isInteger(input.count)||input.count<1||input.count>64){send({error:'Lot invalide'},400);return;}
      batch=batches.get(input.request_id);
      if(batch&&JSON.stringify(batch.request)!==JSON.stringify(input)){send({error:'Paramètres du lot modifiés'},409);return;}
      if(batch?.phase==='completed'){send(batch);return;}
      if(!batch){
       const media=tapes();if(input.count>16-media.length||input.count*input.capacity_bytes>storage().available_bytes){send({error:'Pas assez d’espace ou d’emplacements'},409);return;}
       const maximum=Math.max(0,...media.map(m=>Number(/^CVT(\d+)L[78]$/.exec(m.barcode)?.[1]??0)));
       batch={request:{...input},barcodes:Array.from({length:input.count},(_,i)=>`CVT${String(maximum+i+1).padStart(3,'0')}L${input.media_type?.slice(-1)}`),completed:0,phase:'creating'};
      }
     }
     const barcode=isBatch?batch.barcodes[batch.completed]:input.barcode;
     const {media_type,capacity_bytes,segment_size_mib,multipart_part_size_mib=256}=input;
     if(!/^[A-Z0-9-]{6,16}$/.test(barcode)||!['LTO7','LTO8'].includes(media_type)||!Number.isSafeInteger(capacity_bytes)||capacity_bytes<1024**2||capacity_bytes>16*1024**4||capacity_bytes%1024**2!==0||!Number.isInteger(segment_size_mib)||segment_size_mib<16||segment_size_mib>1048576||!Number.isInteger(multipart_part_size_mib)||multipart_part_size_mib<16||multipart_part_size_mib>1024||Math.ceil(segment_size_mib/multipart_part_size_mib)>10000){send({error:'Paramètres de cartouche invalides'},400);return;}
     if(sim.hasTape(barcode)){send({error:'Ce code-barres existe déjà'},409);return;}
     if(sim.tapeCount()>=16){send({error:'Aucun emplacement libre'},409);return;}
     const item={barcode,media_type,capacity_bytes,segment_size_mib,multipart_part_size_mib,bytes_written:0,generation:1,state:'writable'};
     sim.addTape(item);if(isBatch){batch.completed++;if(batch.completed===batch.request.count)batch.phase='completed';batches.set(input.request_id,batch);send(batch);}else send(item,201);
    }catch{send({error:'Requête JSON invalide'},400);}
   });
   return true;
  }
  // OVH archive actions carry their confirmations; they never leave this process.
  const ovh=/^\/v1\/vtls\/[^/]+\/storage\/(archive|recall|archive\/refresh)$/.exec(path);
  if(ovh&&path.startsWith(`/v1/vtls/${demoId}/`)&&req.method==='POST') {
   if(req.headers['x-csrf-token']!==session.csrf_token){req.resume();send({error:'Jeton CSRF requis'},403);return true;}
   let body='';let rejected=false;
   req.on('data',chunk=>{body+=chunk;if(body.length>4096&&!rejected){rejected=true;send({error:'Requête trop volumineuse'},413);}});
   req.on('end',()=>{
    if(rejected)return;
    let input={};try{input=body?JSON.parse(body):{};}catch{send({error:'Requête JSON invalide'},400);return;}
    const destination=url.searchParams.get('destination_id')??'';const time=now();
    const result=ovh[1]==='archive'?sim.archive(destination,input,time):ovh[1]==='recall'?sim.recall(destination,input,time):sim.refresh(destination,time);
    send(result.body,result.status);
   });
   return true;
  }
  // Consume, but never persist, login bodies. Demonstration requires no credentials.
  req.resume();
  if(path==='/health'){send({status:'ok',demo:true});return true;}
  if(path==='/auth/config'){send({local:true,oidc:false,demo:true});return true;}
  // Signing out shows the sign-in page; any credentials sign back in.
  if(path==='/auth/me'){if(signedOut){send({error:'authentication required'},401);return true;}send(session);return true;}
  if(path==='/auth/login'){signedOut=false;send(session);return true;}
  if(path==='/auth/logout'){signedOut=true;send({ok:true});return true;}
  if(path==='/__demo/scenario'&&req.method==='POST') {
   if(!powered){send({error:'Remettez la bibliothèque en service avant de lancer un scénario.'},409);return true;}
   const next=url.searchParams.get('state');
   if(!['writing','reading','loaded','idle','empty','error','unknown','stale','missing','s3-export'].includes(next)){send({error:'Scénario inconnu'},400);return true;}
   tickWrites(now());scenario=next;transferStart=next==='s3-export'?now():null;transferId=next==='s3-export'?`s3-demo-${randomUUID()}`:null;send({scenario});return true;
  }
  if(path==='/__demo/drive'&&req.method==='POST') {
   const barcode=url.searchParams.get('barcode');
   if(!sim.hasTape(barcode)){send({error:'Cartouche absente'},404);return true;}
   tickWrites(now());driveBarcode=barcode;send({drive:barcode});return true;
  }
  if(path==='/__demo/fail-next'&&req.method==='POST'){sim.setFailNext(url.searchParams.get('enabled')==='1');send({fail_next:sim.failNext()});return true;}
  if(path==='/v1/vtls'&&req.method==='GET'){send([{id:demoId,name:'Bibliothèque de démonstration',lifecycle:'ready',provisioning_revision:1}]);return true;}
  const prefix=`/v1/vtls/${demoId}/`;
  if(path.startsWith(prefix)) {
   const action=path.slice(prefix.length);
   if(action==='bandwidth') {
    if(req.method==='PUT') {
     if(req.headers['x-csrf-token']!==session.csrf_token){send({error:'Jeton CSRF requis'},403);return true;}
     let body='',rejected=false;
     req.on('data',chunk=>{body+=chunk;if(body.length>4096&&!rejected){rejected=true;send({error:'Requête trop volumineuse'},413);}});
     req.on('end',()=>{if(rejected)return;
     let value;try{value=JSON.parse(body);}catch{send({error:'Requête invalide'},400);return;}
     if(value.revision!==bandwidth.revision){send({error:'Révision modifiée'},409);return true;}
     if(!['tape_write_bps','s3_bps'].every(k=>value[k]===null||Number.isSafeInteger(value[k])&&value[k]>=1000000&&value[k]<=100000000000&&value[k]%1000000===0)){send({error:'Débit invalide'},400);return true;}
     bandwidth={...bandwidth,revision:bandwidth.revision+1,tape_write_bps:value.tape_write_bps,s3_bps:value.s3_bps};
     send(bandwidth);});return true;
    }
    send(bandwidth);return true;
   }
   if(req.method==='GET') {
    if(action==='pbs-connection'){send({vtl_id:demoId,revision:0,host:null,port:3260,pbs:null,phase:'missing_prerequisite',steps:[],message:'Aucun PBS associé à la démonstration.'});return true;}
    if(action==='provisioning'){send({vtl_id:demoId,phase:'unavailable',qualified:false,message:'Matériel simulé dans cette démonstration.'});return true;}

    if(action==='telemetry/history') {
     try{send(history.query(demoId,url.searchParams));}catch(error){send({error:error.message},400);}return true;
    }
    if(action==='cartridges/options'){send({configured:true,online_creation:true,pending_batch:[...batches.values()].find(b=>b.phase!=='completed')??null,storage:storage(),ready:sim.tapeCount()<16,reason:sim.tapeCount()<16?null:'Aucun emplacement libre',media_types:['LTO7','LTO8'],default_media_type:'LTO8',default_capacity_mib:2048,default_segment_size_mib:1048576,default_multipart_part_size_mib:256,min_multipart_part_size_mib:16,max_multipart_part_size_mib:1024,min_capacity_mib:1,max_capacity_mib:16777216,min_segment_size_mib:16,max_segment_size_mib:1048576,free_slots:16-sim.tapeCount(),demo:true});return true;}
    if(action==='storage/buckets'){send(sim.buckets(now()));return true;}
    if(action==='storage'){
     const report=sim.report(url.searchParams.get('destination_id')??sim.buckets(now()).find(b=>b.active)?.id,now());
     send(report??{error:'Bucket absent'},report?200:404);return true;
    }
    if(action==='storage/recovery'){
     const inventory=sim.recovery(url.searchParams.get('destination_id')??'',now());
     send(inventory??{error:'Bucket absent'},inventory?200:404);return true;
    }
    if(action==='storage/schedule'){send({rotation_due:false,active_bucket:sim.activeBucket(now())});return true;}
    if(action==='client'){send({client:'veeam'});return true;}
    if(action==='storage/archive'){
     const destination=url.searchParams.get('destination_id')??'';
     if(!sim.hasBucket(destination)){send({error:'Bucket absent'},404);return true;}
     send(sim.operations(destination,now()));return true;
    }
    const live=snapshot();
    const media=tapes(),jobs=sim.jobs(now());
    const responses={status:{status:'ok',cartridges:media.length,jobs:jobs.length,media_root:'/demo'},cartridges:media,jobs,telemetry:live,maintenance:powerStatus(),'maintenance/events':maintenanceEvents,reintegrations:{configured:false,operations:[],eligible:[],pending:null}};
    if(action in responses){send(responses[action]);return true;}
    const details=action.match(/^cartridges\/([^/]+)$/);
    if(details){send(media.find(m=>m.barcode===details[1])??{error:'Cartouche absente'});return true;}
    if(action.endsWith('/generations')){send([]);return true;}
   }
   if(req.method==='POST'&&['maintenance/enter','maintenance/leave'].includes(action)) {
    if(req.headers['x-csrf-token']!==session.csrf_token){send({error:'Jeton CSRF requis'},403);return true;}
    if(sim.busy(now())){send({error:'Une opération VaultCenter est en cours.'},409);return true;}
    if(action==='maintenance/enter'&&powered){
     const live=snapshot();
     if(live.status!=='available'||now()-live.generated_at>10000||!live.drives.length){send({error:'Impossible de confirmer que le lecteur est disponible.'},409);return true;}
     if(live.demo_transfer?.status==='running'||live.drives.some(d=>d.state!=='empty'||d.barcode)){send({error:'Terminez la copie et déchargez le lecteur avant l’arrêt.'},409);return true;}
    }
    powered=action==='maintenance/leave';
    if(!powered){tickWrites(now());scenario='empty';transferStart=null;transferId=null;}
    const result=powerStatus();
    maintenanceEvents.unshift({id:maintenanceEvents.length+1,action:powered?'leave':'enter',success:true,actor:session.user.username,created_at:new Date(now()).toISOString(),message:JSON.stringify(result)});
    send(result);return true;
   }
   const mutation=action.match(/^cartridges\/([^/]+)\/(seal|export)$/);
   if(req.method==='POST'&&mutation) {
    tickWrites(now());
    const result=mutation[2]==='seal'?sim.seal(mutation[1],now()):sim.exportTape(mutation[1],now());
    send(result.body,result.status);return true;
   }
  }
  send({error:'Route non disponible dans la démonstration locale'},404);return true;
 }
 demo.collectHistory=collectHistory;
 demo.close=()=>{if(timer!==null){cancel(timer);timer=null;}};
 return demo;
}
