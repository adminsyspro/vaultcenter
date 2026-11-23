use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, Response, StatusCode},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Fake {
    objects: BTreeMap<String, Vec<u8>>,
    deleted: Vec<String>,
    deny: bool,
    versioned: bool,
}
async fn serve(State(state): State<Arc<Mutex<Fake>>>, request: Request<Body>) -> Response<Body> {
    let mut state = state.lock().unwrap();
    let uri = request.uri();
    let key = uri
        .path()
        .strip_prefix("/test-bucket/")
        .unwrap_or("")
        .to_string();
    let (status, body) = if uri.query().is_some_and(|q| q.contains("versioning")) {
        (
            200,
            format!(
                "<VersioningConfiguration>{}</VersioningConfiguration>",
                if state.versioned {
                    "<Status>Enabled</Status>"
                } else {
                    ""
                }
            )
            .into_bytes(),
        )
    } else if uri.query().is_some_and(|q| q.contains("versions")) {
        (
            200,
            b"<ListVersionsResult><IsTruncated>false</IsTruncated></ListVersionsResult>".to_vec(),
        )
    } else if request.method() == "DELETE" {
        if state.deny && key.ends_with("1.bin") {
            (403, b"<Error><Code>AccessDenied</Code></Error>".to_vec())
        } else {
            state.deleted.push(key.clone());
            state.objects.remove(&key);
            (204, vec![])
        }
    } else if let Some(bytes) = state.objects.get(&key) {
        (
            200,
            if request.method() == "HEAD" {
                vec![]
            } else {
                bytes.clone()
            },
        )
    } else {
        (404, b"<Error><Code>NoSuchKey</Code></Error>".to_vec())
    };
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .body(Body::from(body))
        .unwrap()
}
async fn fixture() -> (
    S3Store,
    Arc<Mutex<Fake>>,
    tokio::task::JoinHandle<()>,
    String,
) {
    let manifest = json!({"format_version":1,"barcode":"TAPE01","generation":3,"logical_size_bytes":2,"files":[],"segments":[
 {"sequence":0,"source_path":"data","source_offset_bytes":0,"size_bytes":1,"checksum_sha256":"a".repeat(64),"object_key":"vtl/cartridges/TAPE01/generations/3/segments/0.bin"},
 {"sequence":1,"source_path":"data","source_offset_bytes":1,"size_bytes":1,"checksum_sha256":"b".repeat(64),"object_key":"vtl/cartridges/TAPE01/generations/3/segments/1.bin"}]});
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let hash = crate::sha256_hex(&bytes);
    let fake = Arc::new(Mutex::new(Fake::default()));
    {
        let mut state = fake.lock().unwrap();
        state
            .objects
            .insert("vtl/cartridges/TAPE01/manifests/3.json".into(), bytes);
        for key in [
            "vtl/cartridges/TAPE01/generations/3/segments/0.bin",
            "vtl/cartridges/TAPE01/generations/3/segments/1.bin",
            "vtl/cartridges/TAPE01/generations/4/segments/0.bin",
            "other/keep",
        ] {
            state.objects.insert(key.into(), vec![1]);
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(serve).with_state(fake.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config=serde_json::from_value(json!({"provider":"s3_compatible","endpoint":endpoint,"region":"local","bucket":"test-bucket","prefix":"vtl","access_key_id":"test","secret_access_key":"test"})).unwrap();
    (S3Store::connect(config).await, fake, server, hash)
}
#[tokio::test]
async fn removes_only_pinned_generation_and_manifest_last() {
    let (store, fake, server, hash) = fixture().await;
    let barcode = Barcode::parse("TAPE01").unwrap();
    let preview = run(&store, &barcode, 3, &hash, false).await.unwrap();
    assert_eq!(preview["objects"], 3);
    assert!(fake.lock().unwrap().deleted.is_empty());
    let result = run(&store, &barcode, 3, &hash, true).await.unwrap();
    assert_eq!(result["status"], "deleted");
    let state = fake.lock().unwrap();
    assert_eq!(state.deleted.len(), 3);
    assert!(state.deleted.last().unwrap().ends_with("manifests/3.json"));
    assert_eq!(state.objects.len(), 2);
    server.abort();
}
#[tokio::test]
async fn interrupted_delete_keeps_manifest_and_can_retry() {
    let (store, fake, server, hash) = fixture().await;
    let barcode = Barcode::parse("TAPE01").unwrap();
    fake.lock().unwrap().deny = true;
    assert!(run(&store, &barcode, 3, &hash, true).await.is_err());
    assert!(
        fake.lock()
            .unwrap()
            .objects
            .contains_key("vtl/cartridges/TAPE01/manifests/3.json")
    );
    fake.lock().unwrap().deny = false;
    run(&store, &barcode, 3, &hash, true).await.unwrap();
    assert_eq!(fake.lock().unwrap().objects.len(), 2);
    server.abort();
}
#[tokio::test]
async fn changed_manifest_foreign_segment_and_versioning_delete_nothing() {
    for case in ["changed", "foreign", "traversal", "versioned"] {
        let (store, fake, server, mut hash) = fixture().await;
        let barcode = Barcode::parse("TAPE01").unwrap();
        {
            let mut state = fake.lock().unwrap();
            if case == "versioned" {
                state.versioned = true;
            } else if case == "changed" {
                hash = "0".repeat(64);
            } else {
                let key = "vtl/cartridges/TAPE01/manifests/3.json";
                let mut m: Value = serde_json::from_slice(&state.objects[key]).unwrap();
                m["segments"][0]["object_key"] = json!(if case == "traversal" {
                    "vtl/cartridges/TAPE01/generations/3/segments/../../4/segments/0.bin"
                } else {
                    "vtl/cartridges/OTHER1/generations/3/segments/0.bin"
                });
                let bytes = serde_json::to_vec(&m).unwrap();
                hash = crate::sha256_hex(&bytes);
                state.objects.insert(key.into(), bytes);
            }
        }
        assert!(
            run(&store, &barcode, 3, &hash, true).await.is_err(),
            "{case}"
        );
        assert!(fake.lock().unwrap().deleted.is_empty());
        server.abort();
    }
}
