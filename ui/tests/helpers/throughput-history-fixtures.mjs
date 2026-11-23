export function emptyThroughputHistory(vtlId,url='http://localhost/?window_seconds=120') {
 const query=new URL(url).searchParams;
 const end=Date.now(),seconds=Number(query.get('window_seconds')??120);
 return {vtl_id:vtlId,source:'mhvtl',status:'unavailable',retention_days:30,start_ms:end-seconds*1000,end_ms:end,resolution_ms:Math.max(5000,Math.ceil(seconds*1000/600)),series:[],points:[]};
}
