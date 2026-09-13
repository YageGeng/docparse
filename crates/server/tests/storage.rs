use docparse_server::storage::SharedStorage;
use std::io::Write;

/// A duplicate object must be identical; corrupted or conflicting existing bytes cannot be acknowledged as durable input.
#[tokio::test]
async fn immutable_publication_rejects_different_existing_bytes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    for bytes in [b"original".as_slice(), b"original".as_slice()] {
        let mut file = storage.temporary().await.expect("temporary");
        file.write_all(bytes).expect("write");
        storage
            .publish(file, "object.pdf")
            .await
            .expect("idempotent publication");
    }
    let mut file = storage.temporary().await.expect("temporary");
    file.write_all(b"conflict")
        .expect("write equal-length different data");
    let rejected = storage.publish(file, "object.pdf").await;
    assert_eq!(
        std::fs::read(directory.path().join("object.pdf"))
            .expect("existing object"),
        b"original"
    );
    rejected.expect_err("conflicting content must not be silently accepted");
}
