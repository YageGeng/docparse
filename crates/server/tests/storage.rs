use docparse_server::storage::SharedStorage;
use std::io::Write;

/// Cache generation and deletion must not acquire locks on immutable payload files.
#[tokio::test]
async fn result_maintenance_does_not_lock_the_payload() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let mut source = storage.temporary().await.expect("temporary");
    source.write_all(br#"{"data":{"context":{"page_count":1},"pages":[{"page_number":1}],"errors":[]}}"#).expect("source");
    storage
        .publish(source, "result.json")
        .await
        .expect("publish");
    let payload = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("result.json"))
        .expect("payload");
    payload.lock().expect("payload lock");
    let generated = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        storage.result_artifact("result.json", None),
    )
    .await;
    // Release the guard even on failure so an old blocking implementation cannot hang runtime shutdown.
    payload.unlock().expect("unlock payload");
    generated
        .expect("cache generation must not wait for a payload lock")
        .expect("index");
    let coordination = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join(".locks/result.json"))
        .expect("coordination file");
    payload.lock().expect("payload lock");
    let deleted = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        storage.remove("result.json"),
    )
    .await;
    payload.unlock().expect("unlock payload");
    deleted
        .expect("deletion must not wait for a payload lock")
        .expect("delete");
    // A waiting replica's old descriptor must still identify the same inode after deletion.
    coordination.lock().expect("retained coordination lock");
    let next = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join(".locks/result.json"))
        .expect("next replica");
    assert!(matches!(
        next.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
}

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

/// A concurrent cache builder must never recreate artifacts after deletion wins the coordination lock.
#[tokio::test]
async fn deleting_a_result_cleans_concurrent_cache_builds() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let mut source = storage.temporary().await.expect("temporary");
    source.write_all(br#"{"data":{"context":{"page_count":1},"pages":[{"page_number":1}],"errors":[]}}"#).expect("source");
    storage
        .publish(source, "result.json")
        .await
        .expect("publish");
    let (first, second, deleted) = tokio::join!(
        storage.result_artifact("result.json", None),
        storage.result_artifact("result.json", None),
        storage.remove("result.json")
    );
    // Builders may finish before deletion or report that the source disappeared; neither may leave a cache behind.
    drop((first, second));
    deleted.expect("delete");
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("storage")
            .filter(|entry| entry
                .as_ref()
                .expect("entry")
                .file_type()
                .expect("file type")
                .is_file())
            .count(),
        0
    );
}
