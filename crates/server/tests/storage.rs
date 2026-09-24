use docparse_server::storage::SharedStorage;
use std::io::Write;

/// Only files inside the selected result's figure directory may become browser assets.
#[tokio::test]
async fn figure_paths_stay_inside_their_result() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let owned = directory.path().join("result.json.figures-owned");
    let other = directory.path().join("other.json.figures-owned");
    std::fs::create_dir(&owned).expect("owned directory");
    std::fs::create_dir(&other).expect("other directory");
    let image = owned.join("image.png");
    std::fs::write(&image, b"image").expect("image");
    let other_image = other.join("image.png");
    std::fs::write(&other_image, b"other image").expect("other image");
    let outside = directory.path().join("outside.png");
    std::fs::write(&outside, b"outside").expect("outside image");
    assert_eq!(
        storage
            .figure_path("result.json", image.to_str().expect("path"))
            .await
            .expect("owned figure"),
        image.canonicalize().expect("canonical image")
    );
    for path in [&other_image, &outside] {
        storage
            .figure_path("result.json", path.to_str().expect("path"))
            .await
            .expect_err("unowned figure must be rejected");
    }
}

/// A changed Markdown projection must rebuild old output with portable, inlined file figures.
#[tokio::test]
async fn markdown_cache_rebuilds_and_inlines_file_figures() {
    use docparse_core::{
        DocumentContext, DocumentResult, FigureDelivery, FigureImage,
        FigureMediaType, FigureSource, PageImageAsset, PageResult,
        SchemaVersion,
    };
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let figures = storage.root().join("result.json.figures-owned");
    std::fs::create_dir(&figures).expect("figures");
    let image = figures.join("image.png");
    std::fs::write(&image, [137, 80, 78, 71]).expect("image bytes");
    let document = DocumentResult::builder()
        .schema_version(SchemaVersion::V2_0)
        .context(DocumentContext::builder().page_count(1).build())
        .pages(vec![
            PageResult::builder()
                .page_number(1)
                .width(100.0)
                .height(100.0)
                .rotation(0)
                .blocks(Vec::new())
                .images(vec![PageImageAsset {
                    id: "image-1".into(),
                    bbox: docparse_layout::Bbox::try_from([
                        10.0, 10.0, 20.0, 20.0,
                    ])
                    .expect("bbox"),
                    image: FigureImage::builder()
                        .source(FigureSource::Embedded)
                        .media_type(FigureMediaType::Png)
                        .width(1)
                        .height(1)
                        .delivery(FigureDelivery::File {
                            path: image.to_string_lossy().into_owned(),
                        })
                        .build(),
                }])
                .build(),
        ])
        .build();
    let mut source = storage.temporary().await.expect("temporary");
    serde_json::to_writer(
        source.as_file_mut(),
        &serde_json::json!({ "data": document }),
    )
    .expect("source JSON");
    storage
        .publish(source, "result.json")
        .await
        .expect("publish");
    let placeholder = "[formula]";
    let hash: String = blake3::hash(placeholder.as_bytes())
        .to_hex()
        .chars()
        .take(32)
        .collect();
    let old_cache = directory.path().join(format!("result.json.v6.{hash}.md"));
    std::fs::write(&old_cache, "stale line breaks").expect("old Markdown");

    let cache = storage
        .result_artifact("result.json", Some(placeholder))
        .await
        .expect("current Markdown");
    assert_ne!(cache, old_cache);
    assert!(cache.to_string_lossy().contains(".v7."));
    assert_eq!(
        std::fs::read_to_string(cache).expect("Markdown"),
        "![image](data:image/png;base64,iVBORw==)"
    );
}

/// Deleting a result also removes its attempt-owned image directories without touching another result.
#[tokio::test]
async fn deletion_removes_only_owned_figure_directories() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    for name in ["result.json.figures-one", "other.json.figures-two"] {
        let path = directory.path().join(name);
        std::fs::create_dir(&path).expect("directory");
        std::fs::write(path.join("image.png"), b"image").expect("image");
    }
    storage.remove("result.json").await.expect("delete");
    assert!(!directory.path().join("result.json.figures-one").exists());
    assert!(
        directory
            .path()
            .join("other.json.figures-two/image.png")
            .exists()
    );
}

/// Real database fences distinguish active writers, failed attempts, ambiguous commits, and deleted results.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn pending_figure_recovery_follows_durable_attempt_ownership() {
    use docparse_database::{
        connection, query::parse_job::ParseJobQuery as Jobs,
    };
    use docparse_server::{
        cleanup::DeletedResults,
        state::{AppState, HttpOptions},
    };
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;
    let db = connection::connect(
        &docparse_config::DatabaseConfig::builder()
            .url(
                std::env::var("DOCPARSE_TEST_DATABASE_URL")
                    .expect("test database"),
            )
            .build(),
    )
    .await
    .expect("database");
    let directory = tempfile::tempdir().expect("storage");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let state = AppState::new(
        db.clone(),
        storage,
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    let cleanup = DeletedResults::from(&state);
    let id = Uuid::new_v4();
    Jobs::submit(&db, id, &"a".repeat(64), None, None)
        .await
        .expect("submit");
    let lease = Jobs::claim(&db, 60, 3).await.expect("claim").expect("job");
    assert_eq!(lease.job.id, id);
    let active = directory
        .path()
        .join(format!("{id}-{}.json.figures-active", lease.token));
    let stale = directory
        .path()
        .join(format!("{id}-{}.json.figures-stale", Uuid::new_v4()));
    for path in [&active, &stale] {
        std::fs::create_dir(path).expect("directory");
        std::fs::write(path.join(".pending"), []).expect("marker");
        std::fs::write(path.join("image.png"), b"image").expect("image");
    }
    cleanup.sweep().await.expect("recover stale");
    assert!(active.join("image.png").exists());
    assert!(!stale.exists());
    assert!(
        Jobs::finish(
            &db,
            &lease,
            Err("retry"),
            None,
            std::time::Duration::ZERO
        )
        .await
        .expect("fail")
    );
    cleanup.sweep().await.expect("recover failed");
    assert!(!active.exists());
    let lease = Jobs::claim(&db, 60, 3)
        .await
        .expect("claim")
        .expect("retry");
    let name = format!("{id}-{}.json", lease.token);
    let committed = directory.path().join(format!("{name}.figures-committed"));
    std::fs::create_dir(&committed).expect("directory");
    std::fs::write(committed.join(".pending"), []).expect("marker");
    std::fs::write(committed.join("image.png"), b"image").expect("image");
    assert!(
        Jobs::finish(&db, &lease, Ok(&name), None, std::time::Duration::ZERO)
            .await
            .expect("commit")
    );
    cleanup.sweep().await.expect("recover ambiguous success");
    assert!(committed.join("image.png").exists());
    assert!(!committed.join(".pending").exists());
    Jobs::mark_deleted(&db, id)
        .await
        .expect("delete")
        .expect("job");
    cleanup.sweep().await.expect("cleanup deleted job");
    assert!(!committed.exists());
}

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
