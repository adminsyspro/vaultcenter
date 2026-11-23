use super::*;

#[test]
fn checkpoint_hash_continues_without_replaying_prefix() {
    let mut hash = Sha256::new();
    hash.update(b"a");
    let mut resumed = restore_hash(&save_hash(&hash)).unwrap();
    resumed.update(b"bc");
    assert_eq!(
        hex::encode(resumed.finalize()),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(restore_hash("00").is_err());
}

#[test]
fn composite_uses_binary_part_hashes_in_order() {
    let parts = vec![
        MultipartPart {
            part_number: 1,
            etag: "one".into(),
            size: 1,
            checksum_sha256: Some("ypeBEsobvcr6wjGzmiPcTaeG7/gUfE5yuYB3ha/uSLs=".into()),
        },
        MultipartPart {
            part_number: 2,
            etag: "two".into(),
            size: 1,
            checksum_sha256: Some("PiPoFgA5WUoziU9lZOGxNIu9egCI1CxKy3PurtWcAJ0=".into()),
        },
    ];
    let result = composite(&parts).unwrap();
    // SHA256(SHA256("a") || SHA256("b")), encoded using the S3 composite convention.
    assert_eq!(
        result.value,
        "5aAf7hTg7VxIcU8iGA8lrYNltT+XefedxKPX6Tlj+Uo=-2"
    );
}

#[test]
fn journal_directory_inside_source_is_rejected_without_creating_it() {
    let source = std::env::temp_dir().join(format!("stream-journal-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&source).unwrap();
    let source = source.canonicalize().unwrap();
    let directory = source.join("private");
    assert!(Disk::open(directory.join("journal.json"), &source).is_err());
    assert!(!directory.exists());
    std::fs::remove_dir_all(source).unwrap();
}
