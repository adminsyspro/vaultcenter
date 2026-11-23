import {chmodSync,closeSync,existsSync,fsyncSync,ftruncateSync,lstatSync,mkdirSync,openSync,readFileSync,renameSync,unlinkSync,writeFileSync} from 'node:fs';
import {dirname,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';

const WINDOWS=new Set([120,300,900,3600,21600,86400,604800,2592000]);
const DAY=86400000;
const finiteRate=value=>value===null||(Number.isFinite(value)&&value>=0);
const identity=sample=>JSON.stringify([sample.vtl_id,sample.kind,sample.drive_id,sample.barcode,sample.transfer_id??null]);
function validSample(sample) {
 return sample&&typeof sample.vtl_id==='string'&&['pbs','s3'].includes(sample.kind)&&
  typeof sample.barcode==='string'&&typeof sample.drive_id==='string'&&
  (sample.transfer_id===null||typeof sample.transfer_id==='string')&&
  Number.isSafeInteger(sample.at)&&sample.at>=0&&finiteRate(sample.writeRate)&&finiteRate(sample.localGrowth);
}
// Repeated series identity dominates month-long histories. Keep disk rows
// lossless and compact without reducing the five-second sampling resolution.
function encode(samples) {
 const index=new Map();const series=[];const rows=[];
 for(const sample of samples) {
  const key=identity(sample);let number=index.get(key);
  if(number===undefined){number=series.length;index.set(key,number);series.push({vtl_id:sample.vtl_id,kind:sample.kind,drive_id:sample.drive_id,barcode:sample.barcode,transfer_id:sample.transfer_id});}
  rows.push([number,sample.at,sample.writeRate,sample.localGrowth]);
 }
 return {version:2,source:'demo',series,samples:rows};
}
function decode(saved) {
 if(saved.source!=='demo'||!Array.isArray(saved.samples))throw Error('Invalid demo history; existing file was preserved');
 let samples=saved.samples;
 if(saved.version===2&&Array.isArray(saved.series)) {
  samples=saved.samples.map(row=>{
   if(!Array.isArray(row)||row.length!==4||!Number.isInteger(row[0])||row[0]<0||row[0]>=saved.series.length)throw Error('Invalid demo history row');
   return {...saved.series[row[0]],at:row[1],writeRate:row[2],localGrowth:row[3]};
  });
 }else if(saved.version!==1)throw Error('Unsupported demo history version');
 if(!samples.every(validSample))throw Error('Invalid demo history; existing file was preserved');
 return samples;
}
function integer(value,name,fallback) {
 if(value===null)return fallback;
 if(!/^\d+$/.test(value)||!Number.isSafeInteger(Number(value)))throw Error(`${name} invalide`);
 return Number(value);
}

/** Local demonstration samples only. A missing file starts empty; no past data is seeded. */
export function createDemoHistory({file=null,now=Date.now,retentionDays=30}={}) {
 if(!Number.isInteger(retentionDays)||retentionDays<1||retentionDays>90)throw Error('Demo history retention must be 1..90 days');
 let samples=[];
 let storageError=null;
 let lastCompacted=now();
 const path=file===null?null:resolve(file);
 const journal=path?`${path}.journal`:null;
 if(path&&existsSync(path)) {
  if(!lstatSync(path).isFile())throw Error('Demo history must be a regular private file');
  const saved=JSON.parse(readFileSync(path,'utf8'));
  samples=decode(saved);lastCompacted=saved.version===1?now()-3600000:saved.compacted_at??now();
  chmodSync(path,0o600);
 }
 if(journal&&existsSync(journal)) {
  if(!lstatSync(journal).isFile())throw Error('Demo history journal must be a regular private file');
  chmodSync(journal,0o600);
  const bytes=readFileSync(journal);const complete=bytes.lastIndexOf(10)+1;
  const recovered=[];
  for(const line of bytes.subarray(0,complete).toString('utf8').split('\n'))if(line)recovered.push(...decode(JSON.parse(line)));
  // Atomic snapshot replacement can succeed before journal truncation. Replayed
  // identities/timestamps replace those rows, never count them twice.
  const keys=new Set(recovered.map(sample=>`${identity(sample)}:${sample.at}`));
  const times=new Set(recovered.map(sample=>sample.at));
  samples=samples.filter(sample=>!times.has(sample.at)||!keys.has(`${identity(sample)}:${sample.at}`));
  const unique=new Map(recovered.map(sample=>[`${identity(sample)}:${sample.at}`,sample]));
  samples.push(...unique.values());
  // An interrupted append may leave a partial final line. It was not fsynced as
  // a complete batch, so discard only that suffix before any future append.
  if(complete<bytes.length){const handle=openSync(journal,'r+');try{ftruncateSync(handle,complete);fsyncSync(handle);}finally{closeSync(handle);}}
 }
 const prune=()=>{
  const cutoff=now()-retentionDays*DAY;
  const before=samples.length;samples=samples.filter(sample=>sample.at>=cutoff);
  return samples.length!==before;
 };
 const persist=(incoming=[],force=false)=>{
  if(!path)return;
  if(storageError)throw Error(`History storage requires restart after error: ${storageError}`);
  const parent=dirname(path);mkdirSync(parent,{recursive:true,mode:0o700});
  const compact=force||!existsSync(path)||now()-lastCompacted>=3600000;
  const temporary=compact?`${path}.${process.pid}.${randomUUID()}.tmp`:null;
  let handle;
  try {
   if(compact) {
    handle=openSync(temporary,'wx',0o600);
    writeFileSync(handle,JSON.stringify({...encode(samples),compacted_at:now()}));fsyncSync(handle);closeSync(handle);handle=undefined;
    renameSync(temporary,path);
    // Make the replacement durable before deleting the replay log.
    const directory=openSync(parent,'r');try{fsyncSync(directory);}finally{closeSync(directory);}
    handle=openSync(journal,'w',0o600);fsyncSync(handle);closeSync(handle);handle=undefined;
    lastCompacted=now();
   }else if(incoming.length) {
    handle=openSync(journal,'a',0o600);
    writeFileSync(handle,JSON.stringify({version:1,source:'demo',samples:incoming})+'\n');fsyncSync(handle);closeSync(handle);handle=undefined;
   }
   const directory=openSync(parent,'r');try{fsyncSync(directory);}finally{closeSync(directory);}
  }catch(error){storageError=error.message;throw error;}
  finally{if(handle!==undefined)closeSync(handle);if(temporary&&existsSync(temporary))unlinkSync(temporary);}
 };
 // Expired points are hidden immediately; normal operation compacts disk hourly.
 // Restart also purges expired disk rows before opening the history endpoint.
 if(prune())persist([],true);
 return {
  record(incoming) {
   if(!incoming.every(validSample))throw Error('Invalid demo throughput sample');
   // A collector restart at the same timestamp must not overweight a bucket.
   const keys=new Set(incoming.map(sample=>`${identity(sample)}:${sample.at}`));
   const times=new Set(incoming.map(sample=>sample.at));
   if(keys.size)samples=samples.filter(sample=>!times.has(sample.at)||!keys.has(`${identity(sample)}:${sample.at}`));
   samples.push(...incoming);const purged=prune();
   if(incoming.length||purged)persist(incoming);
  },
  query(vtlId,params) {
   const allowed=new Set(['barcode','drive_id','kind','transfer_id','window_seconds','end_ms','aggregate']);
   for(const key of params.keys())if(!allowed.has(key)||params.getAll(key).length!==1)throw Error('Paramètre d’historique invalide');
   const text=value=>value.length>0&&value.length<=120&&!/[\x00-\x1f\x7f-\x9f]/.test(value);
   if(params.has('barcode')&&!/^[A-Za-z0-9_-]{1,32}$/.test(params.get('barcode')))throw Error('barcode invalide');
   for(const key of ['drive_id','transfer_id'])if(params.has(key)&&!text(params.get(key)))throw Error(`${key} invalide`);
   const kind=params.get('kind')??'pbs';if(!['pbs','s3'].includes(kind))throw Error('kind invalide');
   const windowSeconds=integer(params.get('window_seconds'),'window_seconds',120);
   if(!WINDOWS.has(windowSeconds))throw Error('window_seconds invalide');
   const end=integer(params.get('end_ms'),'end_ms',now());if(end>now())throw Error('end_ms ne peut pas être dans le futur');
   const start=end-windowSeconds*1000;
   const resolution=Math.max(5000,Math.ceil(windowSeconds*1000/600));
   const barcode=params.get('barcode');const requestedDrive=params.get('drive_id');const requestedTransfer=params.get('transfer_id');
   if(requestedTransfer&&kind!=='s3')throw Error('transfer_id exige kind=s3');
   if(params.has('aggregate')&&params.get('aggregate')!=='true')throw Error('aggregate invalide');
   const aggregate=params.has('aggregate');
   if(aggregate&&(barcode||requestedTransfer))throw Error('aggregate exclut barcode et transfer_id');
   const retained=samples.filter(sample=>sample.vtl_id===vtlId&&sample.kind===kind&&sample.at>=now()-retentionDays*DAY&&sample.at<=end&&(!requestedDrive||sample.drive_id===requestedDrive));
   const inventory=new Map();
   for(const sample of retained) {
    const key=identity(sample);const previous=inventory.get(key);
    if(!previous||sample.at>previous.last_at)inventory.set(key,{drive_id:sample.drive_id,barcode:sample.barcode,last_at:sample.at,...(kind==='s3'?{transfer_id:sample.transfer_id}:{})});
   }
   const series=[...inventory.values()].sort((a,b)=>b.last_at-a.last_at||a.barcode.localeCompare(b.barcode));
   let matching=barcode?retained.filter(sample=>sample.barcode===barcode&&sample.at<=end&&(!requestedTransfer||sample.transfer_id===requestedTransfer)):[];
   // Never aggregate two physical drives or two independent S3 transfers together.
   const latest=matching.reduce((best,sample)=>!best||sample.at>best.at?sample:best,null);
   if(latest)matching=matching.filter(sample=>sample.drive_id===(requestedDrive??latest.drive_id)&&(kind!=='s3'||sample.transfer_id===(requestedTransfer??latest.transfer_id)));
   matching=matching.filter(sample=>sample.at>=start&&sample.at<=end);
   if(aggregate)matching=retained.filter(sample=>sample.at>=start&&sample.at<=end);
   const buckets=barcode||aggregate?Array.from({length:Math.ceil(windowSeconds*1000/resolution)},(_,index)=>({at:start+index*resolution,write:0,writes:0,growth:0,growths:0})):[];
   for(const sample of matching) {
    const bucket=buckets[Math.min(buckets.length-1,Math.floor((sample.at-start)/resolution))];
    if(sample.writeRate!==null){bucket.write+=sample.writeRate;bucket.writes++;}
    if(sample.localGrowth!==null){bucket.growth+=sample.localGrowth;bucket.growths++;}
   }
   // Aggregate: each series keeps its own average per bucket, then the series add up; an unmeasured bucket stays null.
   let points=buckets.map(bucket=>({at:bucket.at,writeRate:bucket.writes?bucket.write/bucket.writes:null,localGrowth:bucket.growths?bucket.growth/bucket.growths:null}));
   if(aggregate) {
    const perSeries=buckets.map(()=>new Map());
    for(const sample of matching) {
     const own=perSeries[Math.min(buckets.length-1,Math.floor((sample.at-start)/resolution))];const key=identity(sample);
     const cell=own.get(key)??{write:0,writes:0,growth:0,growths:0};own.set(key,cell);
     if(sample.writeRate!==null){cell.write+=sample.writeRate;cell.writes++;}
     if(sample.localGrowth!==null){cell.growth+=sample.localGrowth;cell.growths++;}
    }
    const sum=(cells,total,count)=>{const measured=cells.filter(cell=>cell[count]);return measured.length?measured.reduce((all,cell)=>all+cell[total]/cell[count],0):null;};
    points=buckets.map((bucket,index)=>{const cells=[...perSeries[index].values()];return {at:bucket.at,writeRate:sum(cells,'write','writes'),localGrowth:sum(cells,'growth','growths')};});
   }
   const available=barcode||aggregate?matching.length>0:series.length>0;
   return {vtl_id:vtlId,source:'demo',status:storageError?'unavailable':available?'available':'unavailable',
    ...(!available||storageError?{reason:storageError?'storage_error':'history_missing'}:{}),retention_days:retentionDays,
    start_ms:start,end_ms:end,resolution_ms:resolution,series,
    points};
  },
 };
}
