export const demoBuckets=[
 {id:'demo-bucket-current',bucket:'vaultcenter-demo',active:true,created_at:'2026-10-01T08:00:00Z',configuration_revision:1,copy_count:3,observed_status:null,rotation_phase:null},
 {id:'demo-bucket-previous',bucket:'vaultcenter-demo-precedent',active:false,created_at:'2026-09-01T08:00:00Z',configuration_revision:1,copy_count:1,observed_status:null,rotation_phase:null},
];
export function storageFixture(id='demo-bucket-current',now=Date.now()) {
 const bucket=demoBuckets.find(b=>b.id===id);
 if(!bucket)return null;
 const cartridges=bucket.active?[{barcode:'CVT001L8',objects:8,bytes:850*1024**2},{barcode:'CVT002L8',objects:12,bytes:600*1024**2},{barcode:'CVT003L8',objects:3,bytes:40*1024**2}]:[{barcode:'CVT009L8',objects:6,bytes:320*1024**2}];
 return {demo:true,destination_id:id,active:bucket.active,bucket:bucket.bucket,prefix:'vaultcenter',region:'local-demo',provider:'s3_compatible',provider_label:'S3 compatible · simulation',supported:false,raw_status:null,state_label:'Simulation locale',object_access:'Objets simulés',archive_lock_until:null,multipart_uploads:0,checked_at:Math.floor(now/1000),checks:[],catalogue_generations:[],inventory:{complete:true,objects:cartridges.reduce((sum,m)=>sum+m.objects,0),bytes:cartridges.reduce((sum,m)=>sum+m.bytes,0),outside_prefix:0,unrecognized_objects:0,cartridges,manifests:cartridges.flatMap((m,i)=>(i===1?[1,2]:[1]).map(generation=>({barcode:m.barcode,generation,key:`vaultcenter/${m.barcode}/gen-${generation}/manifest.json`})))} };
}
