use super::*;
use tokio::net::UnixListener;

#[tokio::test]
async fn transport_rejects_missing_truncated_and_oversized_responses() {
    let path = std::env::temp_dir().join(format!("fst-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    assert!(invoke(&path, json!({"action":"status"})).await.is_err());
    for response in [
        b"broken\n".to_vec(),
        b"{\"ok\":true}\n".to_vec(),
        vec![b'x'; 65537],
        b"{\"ok\":true,\"result\":{}}".to_vec(),
    ] {
        let listener = UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(&response).await;
        });
        assert!(invoke(&path, json!({"action":"status"})).await.is_err());
        task.await.unwrap();
        std::fs::remove_file(&path).unwrap();
    }
}
#[tokio::test]
async fn bridge_preserves_operation_identity_and_safe_refusal() {
    let path = std::env::temp_dir().join(format!("fst-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    for (response, success) in [
        (
            json!({"ok":true,"result":{"phase":"applying","operation":{"id":"durable"}}}),
            true,
        ),
        (
            json!({"ok":false,"error":{"message":"Chemin non vide"}}),
            false,
        ),
    ] {
        let listener = UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut line = String::new();
            BufReader::new(reader).read_line(&mut line).await.unwrap();
            let received: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(received["request_id"], "durable");
            writer
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        let result = invoke(&path, json!({"action":"apply","request_id":"durable"})).await;
        if success {
            assert_eq!(result.unwrap()["operation"]["id"], "durable");
        } else {
            assert_eq!(result.unwrap_err().message, "Chemin non vide");
        }
        task.await.unwrap();
        std::fs::remove_file(&path).unwrap();
    }
}

#[tokio::test]
async fn scoped_bridge_rejects_another_vtls_response() {
    let path = std::env::temp_dir().join(format!("fst-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let id = cold_vtl_core::VtlId::new();
    let listener = UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["vtl_id"], id.to_string());
        writer
            .write_all(b"{\"ok\":true,\"result\":{\"vtl_id\":\"foreign\",\"phase\":\"ready\"}}\n")
            .await
            .unwrap();
    });
    assert!(
        invoke_scoped(&path, id, json!({"action":"status"}))
            .await
            .is_err()
    );
    task.await.unwrap();
    std::fs::remove_file(path).unwrap();
}
