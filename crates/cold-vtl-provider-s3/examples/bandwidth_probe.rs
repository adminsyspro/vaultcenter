//! Opt-in wire benchmark against the dedicated local HTTP fixture. Never real S3.
use cold_vtl_provider_s3::{S3Config, S3Store, bandwidth::VtlBandwidth};
use std::{
    path::Path,
    time::{Duration, Instant},
};
const ID: &str = "00000000-0000-4000-8000-000000000021";
#[tokio::main]
async fn main() {
    let root = std::env::args().nth(1).expect("temporary state root");
    let rate: u64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let folder = Path::new(&root).join("vtls").join(ID);
    std::fs::create_dir_all(&folder).unwrap();
    let config = serde_json::json!({"schema":1,"vtl_id":ID,"revision":rate+1,"tape_write_bps":null,"s3_bps":if rate==0 {None} else {Some(rate)}});
    std::fs::write(folder.join("bandwidth.json"), config.to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let start = Instant::now();
    let mut tasks = vec![];
    for n in 0..2 {
        let limiter = VtlBandwidth::open(Path::new(&root), ID).unwrap();
        tasks.push(tokio::spawn(async move {
            let store=S3Store::connect(serde_json::from_value::<S3Config>(serde_json::json!({"endpoint":"http://127.0.0.1:19001","region":"us-east-1","bucket":"bandwidth-fixture","prefix":"","access_key_id":"fixture","secret_access_key":"fixture"})).unwrap()).await.with_bandwidth(limiter);
            let key=format!("worker-{n}");let payload=vec![b'A'+n;262_144];let mut bytes=0u64;
            while Instant::now()<deadline {
                store.put_bytes(&key,payload.clone(),"application/octet-stream","fixture").await.unwrap();
                let received=store.get_stream(&key).await.unwrap().collect().await.unwrap().into_bytes();
                assert!(received.as_ref()==payload.as_slice(),"payload mismatch");bytes+=2*payload.len() as u64;
            }
            bytes
        }));
    }
    let mut bytes = 0;
    for task in tasks {
        bytes += task.await.unwrap();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let bps = bytes as f64 / elapsed;
    if rate > 0 {
        assert!(bps <= rate as f64 * 1.05, "aggregate {bps} exceeds {rate}");
    }
    println!(
        "{}",
        serde_json::json!({"limit_bps":rate,"wire_payload_bps":bps,"bytes":bytes,"seconds":elapsed,"workers":2,"roundtrip_equal":true})
    );
}
