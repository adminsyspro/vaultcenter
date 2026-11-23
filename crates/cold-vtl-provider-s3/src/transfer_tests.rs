use super::*;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn new(responses: Vec<Vec<u8>>) -> Self {
        Self::paced(responses, Duration::ZERO)
    }

    /// Sends each response's headers at once and its body after `silence`.
    fn paced(responses: Vec<Vec<u8>>, silence: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = requests.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut index = 0;
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                captured.lock().unwrap().push(request);
                // Repeating the last response exposes unintended SDK retries.
                let response = &responses[index.min(responses.len() - 1)];
                let head = response
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map_or(response.len(), |end| end + 4);
                let _ = stream.write_all(&response[..head]);
                std::thread::sleep(silence);
                let _ = stream.write_all(&response[head..]);
                index += 1;
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    async fn store(&self) -> S3Store {
        S3Store::connect(S3Config {
            provider: None,
            endpoint: self.endpoint.clone(),
            region: "us-east-1".into(),
            bucket: "test-bucket".into(),
            prefix: "vtl".into(),
            access_key_id: "test-key".into(),
            secret_access_key: "test-secret".into(),
        })
        .await
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn response(status: u16, headers: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).into_bytes()
}

struct TempFile(PathBuf);
impl TempFile {
    fn new(bytes: &[u8]) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cold-vtl-provider-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn file_ranges_are_exact_bounded_and_support_one_tib_offsets() {
    let file = TempFile::new(b"prefixPAYLOADsuffix");
    let mut stream = file_range(&file.0, 6, 7).await.unwrap();
    assert_eq!(
        stream.try_next().await.unwrap().unwrap().as_ref(),
        b"PAYLOAD"
    );
    assert!(stream.try_next().await.unwrap().is_none());
    assert!(file_range(&file.0, 6, 99).await.is_err());
    assert!(file_range(&file.0, u64::MAX, 1).await.is_err());
    let one_tib = 1024_u64.pow(4);
    let mut sparse = std::fs::OpenOptions::new()
        .write(true)
        .open(&file.0)
        .unwrap();
    sparse.set_len(one_tib + 7).unwrap();
    sparse.seek(SeekFrom::Start(one_tib)).unwrap();
    sparse.write_all(b"PAYLOAD").unwrap();
    let mut stream = file_range(&file.0, one_tib, 7).await.unwrap();
    assert_eq!(
        stream.try_next().await.unwrap().unwrap().as_ref(),
        b"PAYLOAD"
    );
    let mut stream = file_range(&file.0, 0, one_tib).await.unwrap();
    for _ in 0..4 {
        assert!(stream.try_next().await.unwrap().unwrap().len() <= FILE_BUFFER_BYTES);
    }
}

#[tokio::test]
async fn multipart_protocol_streams_ranges_and_paginates_parts() {
    let server = Server::new(vec![
        response(
            200,
            "",
            "<InitiateMultipartUploadResult><UploadId>upload-1</UploadId></InitiateMultipartUploadResult>",
        ),
        response(200, "ETag: \"part-one\"\r\n", ""),
        response(
            200,
            "",
            "<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1</NextPartNumberMarker><Part><PartNumber>1</PartNumber><ETag>\"part-one\"</ETag><Size>7</Size></Part></ListPartsResult>",
        ),
        response(
            200,
            "",
            "<ListPartsResult><IsTruncated>false</IsTruncated><Part><PartNumber>2</PartNumber><ETag>\"part-two\"</ETag><Size>3</Size></Part></ListPartsResult>",
        ),
        response(
            200,
            "",
            "<CompleteMultipartUploadResult><ETag>\"complete\"</ETag></CompleteMultipartUploadResult>",
        ),
        response(204, "", ""),
        response(200, "ETag: \"put\"\r\n", ""),
    ]);
    let store = server.store().await;
    let file = TempFile::new(b"prefixPAYLOADsuffix");
    let id = store
        .create_multipart("vtl/segment", "sha256")
        .await
        .unwrap();
    assert_eq!(id, "upload-1");
    let uploaded = store
        .upload_part_file("vtl/segment", &id, 1, &file.0, 6, 7)
        .await
        .unwrap();
    assert_eq!(uploaded.size, 7);
    assert_eq!(uploaded.etag, "\"part-one\"");
    let parts = store
        .list_multipart_parts("vtl/segment", &id)
        .await
        .unwrap();
    assert_eq!(parts.len(), 2);
    let object = store
        .complete_multipart("vtl/segment", &id, &parts)
        .await
        .unwrap();
    assert_eq!(object.etag.as_deref(), Some("\"complete\""));
    store.abort_multipart("vtl/segment", &id).await.unwrap();
    store
        .put_file_range("vtl/small", &file.0, 6, 7, "sha256")
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 7);
    assert!(requests[0].starts_with("POST /test-bucket/vtl/segment?uploads"));
    assert!(requests[0].contains("x-amz-meta-cold-vtl-sha256: sha256"));
    assert!(requests[1].contains("partNumber=1"));
    assert!(requests[1].contains("uploadId=upload-1"));
    assert!(requests[1].contains("PAYLOAD"));
    assert!(!requests[1].contains("prefixPAYLOAD"));
    assert!(!requests[1].contains("PAYLOADsuffix"));
    assert!(requests[3].contains("part-number-marker=1"));
    assert!(requests[4].contains("<PartNumber>2</PartNumber>"));
    assert!(requests[5].starts_with("DELETE "));
    assert!(requests[6].contains("PAYLOAD"));
}

#[tokio::test]
async fn ambiguous_create_and_complete_are_not_retried() {
    for reply in [
        response(503, "", "<Error><Code>ServiceUnavailable</Code></Error>"),
        Vec::new(),
    ] {
        for create in [true, false] {
            let server = Server::new(vec![reply.clone()]);
            let store = server.store().await;
            if create {
                assert!(store.create_multipart("key", "sha256").await.is_err());
            } else {
                assert!(
                    store
                        .complete_multipart(
                            "key",
                            "id",
                            &[MultipartPart {
                                checksum_sha256: None,
                                part_number: 1,
                                etag: "etag".into(),
                                size: 7
                            }]
                        )
                        .await
                        .is_err()
                );
            }
            assert_eq!(server.requests().len(), 1);
        }
    }
}

#[tokio::test]
async fn upload_part_retry_reopens_the_exact_same_range() {
    let server = Server::new(vec![
        response(503, "", "<Error><Code>ServiceUnavailable</Code></Error>"),
        response(200, "ETag: \"retried\"\r\n", ""),
    ]);
    let file = TempFile::new(b"prefixPAYLOADsuffix");
    let fixture = crate::bandwidth::tests::Fixture::new();
    let limiter =
        crate::bandwidth::VtlBandwidth::open(&fixture.0, crate::bandwidth::tests::A).unwrap();
    let part = server
        .store()
        .await
        .with_bandwidth(limiter)
        .upload_part_file("key", "id", 1, &file.0, 6, 7)
        .await
        .unwrap();
    assert_eq!(part.etag, "\"retried\"");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.contains("uploadId=id"));
        assert!(request.contains("partNumber=1"));
        assert!(request.contains("PAYLOAD"));
        assert!(!request.contains("prefixPAYLOAD"));
        assert!(!request.contains("PAYLOADsuffix"));
    }
}

#[tokio::test]
async fn missing_upload_is_recognizable_and_invalid_parts_never_write() {
    let server = Server::new(vec![response(
        404,
        "",
        "<Error><Code>NoSuchUpload</Code></Error>",
    )]);
    let store = server.store().await;
    assert!(
        store
            .list_multipart_parts("key", "id")
            .await
            .unwrap_err()
            .is_no_such_upload()
    );
    assert!(
        store
            .abort_multipart("key", "id")
            .await
            .unwrap_err()
            .is_no_such_upload()
    );
    let file = TempFile::new(b"x");
    for part_number in [0, 10001] {
        assert!(
            store
                .upload_part_file("key", "id", part_number, &file.0, 0, 1)
                .await
                .is_err()
        );
    }
    assert!(store.complete_multipart("key", "id", &[]).await.is_err());
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn malformed_pagination_is_rejected() {
    let server = Server::new(vec![response(
        200,
        "",
        "<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1</NextPartNumberMarker></ListPartsResult>",
    )]);
    assert!(
        server
            .store()
            .await
            .list_multipart_parts("key", "id")
            .await
            .is_err()
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn manifests_are_bounded_even_without_content_length() {
    let mut response_bytes =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    let chunk = vec![b'x'; FILE_BUFFER_BYTES];
    for _ in 0..=MAX_MANIFEST_BYTES / FILE_BUFFER_BYTES {
        response_bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        response_bytes.extend_from_slice(&chunk);
        response_bytes.extend_from_slice(b"\r\n");
    }
    response_bytes.extend_from_slice(b"0\r\n\r\n");
    let server = Server::new(vec![response_bytes]);
    let result = server.store().await.get_bytes("manifest").await;
    assert!(
        matches!(result, Err(S3Error::ManifestTooLarge)),
        "{result:?}"
    );
    let server = Server::new(vec![response(200, "", "manifest")]);
    assert_eq!(
        server.store().await.get_bytes("manifest").await.unwrap(),
        b"manifest"
    );
}

#[tokio::test]
async fn bandwidth_limits_real_uploads_and_verification_downloads() {
    use crate::bandwidth::{
        VtlBandwidth,
        tests::{A, Fixture},
    };
    let fixture = Fixture::new();
    let payload = "x".repeat(524_288);
    let server = Server::new(vec![
        response(200, "ETag: \"part\"\r\n", ""),
        response(200, "", &payload),
        response(200, "ETag: \"part\"\r\n", ""),
    ]);
    let store = server
        .store()
        .await
        .with_bandwidth(VtlBandwidth::open(&fixture.0, A).unwrap());
    let started = std::time::Instant::now();
    store
        .put_bytes(
            "manifest",
            payload.as_bytes().to_vec(),
            "application/octet-stream",
            "hash",
        )
        .await
        .unwrap();
    let downloaded = store
        .get_stream("manifest")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap()
        .into_bytes();
    assert_eq!(downloaded.as_ref(), payload.as_bytes());
    let file = TempFile::new(payload.as_bytes());
    store
        .upload_part_file("key", "upload", 1, &file.0, 0, 524_288)
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(1400),
        "all three object bodies must share the 1 MB/s cap"
    );
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn bucket_presence_reports_absent_only_on_an_explicit_not_found() {
    for (reply, expected) in [
        (response(200, "", ""), "exists"),
        (response(404, "", ""), "absent"),
        (
            response(403, "", "<Error><Code>AccessDenied</Code></Error>"),
            "unknown",
        ),
    ] {
        let server = Server::new(vec![reply]);
        let store = server.store().await;
        assert_eq!(
            store.bucket_presence("vc-probe-2026-10-13-abcd").await,
            expected
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1, "{expected}: {requests:?}");
        assert!(
            requests[0].starts_with("HEAD /vc-probe-2026-10-13-abcd"),
            "{}",
            requests[0]
        );
    }
}

#[tokio::test]
async fn empty_bucket_check_distinguishes_content_from_provider_errors() {
    let empty = vec![
        response(200, "", ""),
        response(
            200,
            "",
            "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
        ),
        response(
            200,
            "",
            "<ListMultipartUploadsResult><IsTruncated>false</IsTruncated></ListMultipartUploadsResult>",
        ),
        response(404, "", "<Error><Code>NoSuchTagSet</Code></Error>"),
        response(
            200,
            "",
            "<ListVersionsResult><IsTruncated>false</IsTruncated></ListVersionsResult>",
        ),
    ];
    let server = Server::new(empty.clone());
    assert!(
        server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .unwrap()
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 5, "{requests:?}");
    assert!(
        requests.iter().all(|r| r.contains(" /vc-next")),
        "{requests:?}"
    );
    drop(server);

    let mut occupied = empty.clone();
    occupied[1] = response(
        200,
        "",
        "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>foreign/file</Key><Size>3</Size></Contents></ListBucketResult>",
    );
    let server = Server::new(occupied);
    assert!(
        !server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .unwrap()
    );
    drop(server);

    let mut uploads = empty.clone();
    uploads[2] = response(
        200,
        "",
        "<ListMultipartUploadsResult><IsTruncated>false</IsTruncated><Upload><Key>k</Key><UploadId>u</UploadId></Upload></ListMultipartUploadsResult>",
    );
    let server = Server::new(uploads);
    assert!(
        !server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .unwrap()
    );
    drop(server);

    let mut archived = empty.clone();
    archived[3] = response(
        200,
        "",
        "<Tagging><TagSet><Tag><Key>ovh:intelligent_tiering_status</Key><Value>Archived</Value></Tag></TagSet></Tagging>",
    );
    let server = Server::new(archived);
    assert!(
        !server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .unwrap()
    );
    drop(server);

    let mut history = empty.clone();
    history[4] = response(
        200,
        "",
        "<ListVersionsResult><IsTruncated>false</IsTruncated><DeleteMarker><Key>old</Key><VersionId>v1</VersionId><IsLatest>true</IsLatest></DeleteMarker></ListVersionsResult>",
    );
    let server = Server::new(history);
    assert!(
        !server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .unwrap()
    );
    drop(server);

    let server = Server::new(vec![response(
        403,
        "",
        "<Error><Code>AccessDenied</Code></Error>",
    )]);
    assert!(
        server
            .store()
            .await
            .managed_bucket_is_empty("vc-next")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_checksum_requires_server_evidence_and_sends_expected_digest() {
    let checksum = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
    let server = Server::new(vec![
        response(200, &format!("x-amz-checksum-sha256: {checksum}\r\n"), ""),
        response(
            200,
            &format!("x-amz-checksum-sha256: {checksum}\r\nx-amz-checksum-type: FULL_OBJECT\r\n"),
            "abc",
        ),
        response(200, "x-amz-meta-cold-vtl-sha256: ignored\r\n", "abc"),
    ]);
    let store = server.store().await;
    store
        .put_checked_bytes("vtl/test", b"abc".to_vec(), checksum)
        .await
        .unwrap();
    let expected = cold_vtl_core::NativeChecksum {
        value: checksum.into(),
        kind: "FULL_OBJECT".into(),
    };
    store
        .verify_native_head("vtl/test", 3, &expected)
        .await
        .unwrap();
    assert!(
        store
            .verify_native_head("vtl/test", 3, &expected)
            .await
            .is_err()
    );
    let requests = server.requests();
    assert!(requests[0].contains(&format!("x-amz-checksum-sha256: {checksum}")));
    assert!(
        requests[1]
            .to_lowercase()
            .contains("x-amz-checksum-mode: enabled")
    );
}

#[tokio::test]
async fn native_checksum_rejects_missing_or_wrong_upload_receipt() {
    for headers in ["", "x-amz-checksum-sha256: wrong\r\n"] {
        let server = Server::new(vec![response(200, headers, "")]);
        assert!(
            server
                .store()
                .await
                .put_checked_bytes(
                    "vtl/test",
                    b"abc".to_vec(),
                    "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn native_head_rejects_wrong_kind_size_and_checksum() {
    let digest = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
    for (value, kind, size) in [
        (digest, "COMPOSITE", 3),
        (digest, "FULL_OBJECT", 4),
        ("wrong", "FULL_OBJECT", 3),
    ] {
        let server = Server::new(vec![response(
            200,
            &format!("x-amz-checksum-sha256: {value}\r\nx-amz-checksum-type: {kind}\r\n"),
            "abc",
        )]);
        let expected = cold_vtl_core::NativeChecksum {
            value: digest.into(),
            kind: "FULL_OBJECT".into(),
        };
        assert!(
            server
                .store()
                .await
                .verify_native_head("vtl/test", size, &expected)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn s3_errors_keep_the_provider_cause() {
    let server = Server::new(vec![response(
        400,
        "",
        "<Error><Code>InvalidPart</Code><Message>part missing</Message></Error>",
    )]);
    let part = MultipartPart {
        part_number: 1,
        etag: "\"one\"".into(),
        size: 3,
        checksum_sha256: Some("ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=".into()),
    };
    let error = server
        .store()
        .await
        .complete_checked_multipart("vtl/test", "upload", &[part], "unused-1")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("InvalidPart"), "{error}");
}

#[tokio::test]
async fn completion_waits_through_the_providers_assembly_silence() {
    // OVH sends the 200 headers of CompleteMultipartUpload at once, then nothing
    // for at least 10 s while it assembles; the SDK used to drop it after 5 s.
    let checksum = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
    let server = Server::paced(
        vec![response(
            200,
            "",
            &format!(
                "<CompleteMultipartUploadResult><ChecksumSHA256>{checksum}</ChecksumSHA256></CompleteMultipartUploadResult>"
            ),
        )],
        Duration::from_secs(12),
    );
    let part = MultipartPart {
        part_number: 1,
        etag: "\"one\"".into(),
        size: 3,
        checksum_sha256: Some(checksum.into()),
    };
    server
        .store()
        .await
        .complete_checked_multipart("vtl/test", "upload", &[part], checksum)
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 1);
}
