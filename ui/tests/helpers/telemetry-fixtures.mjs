export const demoId = '00000000-0000-4000-8000-000000000001';
export const demoMedia = () => Array.from({length:9},(_,i)=>({barcode:`CVT00${i+1}L8`,capacity_bytes:2*1024**3,bytes_written:[850*1024**2,600*1024**2,40*1024**2,0,0,0,0,0,0][i],generation:1,state:i<2?'verified':'writable'}));
export function telemetryFixture(id=demoId, scenario='writing', now=Date.now()) {
  const stale=scenario==='stale'; const missing=scenario==='missing'; const known=scenario!=='unknown';
  const time=now-(stale?30000:0);
  const state={writing:'writing',reading:'reading',idle:'idle',loaded:'loaded',empty:'empty',error:'error',unknown:'writing',stale:'writing',missing:'unknown'}[scenario]??'writing';
  const loaded=!['empty','error','unknown'].includes(state) && known;
  return {version:1,vtl_id:id,generated_at:time,observed_at:time,source:'demo',status:missing?'unavailable':'available',reason:missing?'source_missing':null,
    drives:missing?[]:[{id:'11',label:'Lecteur LTO · 01',barcode:loaded?'CVT001L8':null,origin_slot:loaded?1:null,state,observed_at:time,bytes_written:null,write_rate_bps:null}],
    slots:known&&!missing?demoMedia().map((m,i)=>({number:i+1,kind:'storage',barcode:i===0&&loaded?null:m.barcode,position_known:true,observed_at:time})):[],
    media:missing?[]:demoMedia().map((m,i)=>({barcode:m.barcode,observed_at:time,local_size_bytes:m.bytes_written+(i===0?Math.floor((now%60000)/1000)*1024**2:0),local_growth_bps:i===0&&state==='writing'?1024**2:0,logical_used_bytes:null,logical_capacity_bytes:null}))};
}
