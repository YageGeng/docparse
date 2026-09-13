use docparse_config::DatabaseConfig;
use docparse_database::{
    JobStatus, connection::connect, query::parse_job::ParseJobQuery as Jobs,
};
use std::time::Duration;
use uuid::Uuid;

/// Metadata stays immutable across idempotent submissions and history pages neither repeat nor skip their cursor row.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn history_preserves_metadata_and_cursor_order() {
    use docparse_database::{
        entities::parse_jobs,
        seaorm::{ColumnTrait, EntityTrait, QueryFilter},
    };
    let db = connect(
        &DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("database");
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let hash = "b".repeat(64);
    let name = format!("Review-{first}_%.pdf");
    Jobs::submit(&db, first, &hash, Some(&name), Some(123))
        .await
        .expect("first");
    let replay =
        Jobs::submit(&db, first, &hash, Some("different.pdf"), Some(456))
            .await
            .expect("idempotent replay");
    assert_eq!(replay.filename.as_deref(), Some(name.as_str()));
    assert_eq!(replay.size_bytes, Some(123));
    Jobs::submit(&db, second, &hash, Some(&name), Some(123))
        .await
        .expect("second");
    let page = Jobs::list(
        &db,
        None,
        1,
        Some(JobStatus::Queued),
        Some(&name.to_uppercase()),
    )
    .await
    .expect("first page");
    assert_eq!(
        page.iter().map(|job| job.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    let next =
        Jobs::list(&db, Some(second), 1, Some(JobStatus::Queued), Some(&name))
            .await
            .expect("next page");
    assert_eq!(
        next.iter().map(|job| job.id).collect::<Vec<_>>(),
        vec![first]
    );
    assert!(
        Jobs::list(&db, Some(first), 1, None, Some(&name))
            .await
            .expect("last page")
            .is_empty()
    );
    assert!(matches!(
        Jobs::list(&db, None, 0, None, None).await,
        Err(docparse_database::error::DatabaseError::InvalidInput)
    ));
    assert!(matches!(
        Jobs::list(&db, Some(Uuid::new_v4()), 20, None, None).await,
        Err(docparse_database::error::DatabaseError::InvalidInput)
    ));
    // Leave no claimable rows behind for the existing lease integration tests sharing this disposable database.
    parse_jobs::Entity::delete_many()
        .filter(parse_jobs::Column::Id.is_in([first, second]))
        .exec(&db)
        .await
        .expect("remove history fixtures");
}

/// Invalid limits must fail as configuration errors before the connection driver is invoked.
#[tokio::test]
async fn invalid_pool_settings_fail_before_connecting() {
    let config = DatabaseConfig::builder()
        .url("postgresql://127.0.0.1:1/docparse")
        .max_connections(0)
        .build();
    assert!(matches!(
        connect(&config).await,
        Err(docparse_database::error::DatabaseError::Configuration(_))
    ));
}

/// Configured pool limits and acquisition deadlines must apply to real PostgreSQL connections.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn connection_pool_uses_configured_limits() {
    let config = DatabaseConfig::builder()
        .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
        .max_connections(2)
        .min_connections(0)
        .acquire_timeout_ms(500)
        .idle_timeout_ms(12000)
        .build();
    let db = connect(&config).await.expect("configured pool");
    let pool = db.get_postgres_connection_pool();
    assert_eq!(pool.options().get_max_connections(), 2);
    assert_eq!(pool.options().get_min_connections(), 0);
    assert_eq!(
        pool.options().get_acquire_timeout(),
        Duration::from_millis(500)
    );
    assert_eq!(
        pool.options().get_idle_timeout(),
        Some(Duration::from_millis(12000))
    );
    let first = pool.acquire().await.expect("first slot");
    let second = pool.acquire().await.expect("second slot");
    tokio::time::timeout(Duration::from_secs(5), pool.acquire())
        .await
        .expect("bounded wait")
        .expect_err("pool is full");
    drop((first, second));
    db.close().await.expect("close pool");
}

/// Real PostgreSQL transactions must recover abandoned tasks while rejecting stale ownership tokens.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn leases_recover_and_stale_workers_cannot_publish() {
    let url =
        std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("database URL");
    let db = connect(&DatabaseConfig::builder().url(url.clone()).build())
        .await
        .expect("connection");
    let id = Uuid::new_v4();
    let hash = "a".repeat(64);
    assert_eq!(
        Jobs::submit(&db, id, &hash, None, None)
            .await
            .expect("submit")
            .status,
        JobStatus::Queued
    );
    assert_eq!(
        Jobs::submit(&db, id, &hash, None, None)
            .await
            .expect("idempotent")
            .id,
        id
    );
    let conflict = Jobs::submit(&db, id, &"0".repeat(64), None, None)
        .await
        .expect_err("conflicting content");
    assert!(matches!(
        conflict,
        docparse_database::error::DatabaseError::IdempotencyConflict
    ));
    let (left, right) =
        tokio::join!(Jobs::claim(&db, 1, 3), Jobs::claim(&db, 1, 3));
    let claims: Vec<_> = [left.expect("left"), right.expect("right")]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(claims.len(), 1, "exactly one worker owns the queued job");
    let first = claims.into_iter().next().expect("one claim");
    assert_eq!(first.job.id, id);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = Jobs::claim(&db, 60, 3)
        .await
        .expect("reclaim")
        .expect("expired lease");
    assert_eq!(second.job.attempts, 2);
    assert_ne!(first.token, second.token);
    assert!(
        !Jobs::heartbeat(
            &db,
            &first,
            60,
            Some(serde_json::json!({"stage":"complete"}))
        )
        .await
        .expect("stale heartbeat")
    );
    assert!(
        !Jobs::finish(
            &db,
            &first,
            Ok("obsolete.json"),
            None,
            Duration::from_millis(9999)
        )
        .await
        .expect("stale finish")
    );
    // Failure is one explicit outcome, and requeueing must retain its diagnostic without a result path.
    assert!(
        Jobs::finish(
            &db,
            &second,
            Err("retryable attempt failure"),
            None,
            Duration::from_millis(800)
        )
        .await
        .expect("requeue")
    );
    let queued = Jobs::find_by_id(&db, id)
        .await
        .expect("read requeued job")
        .expect("job");
    assert_eq!(queued.status, JobStatus::Queued);
    assert_eq!(queued.error.as_deref(), Some("retryable attempt failure"));
    assert!(queued.result_path.is_none());
    assert_eq!(queued.duration_ms, Some(800));
    let third = Jobs::claim(&db, 60, 3)
        .await
        .expect("retry")
        .expect("third attempt");
    assert_eq!(third.job.attempts, 3);
    assert!(
        third.job.duration_ms.is_none(),
        "retry clears previous timing"
    );
    assert!(
        Jobs::finish(
            &db,
            &third,
            Ok("result.json"),
            Some(serde_json::json!({"stage":"complete","total":1})),
            Duration::from_millis(1234),
        )
        .await
        .expect("finish")
    );
    let reopened = connect(&DatabaseConfig::builder().url(url.clone()).build())
        .await
        .expect("second API instance");
    let result = Jobs::find_by_id(&reopened, id)
        .await
        .expect("get")
        .expect("job persists");
    assert_eq!(result.status, JobStatus::Succeeded);
    assert_eq!(result.result_path.as_deref(), Some("result.json"));
    assert_eq!(result.duration_ms, Some(1234));
    // A stale worker must not overwrite the published duration, even after a successor has completed.
    assert!(
        !Jobs::finish(
            &db,
            &first,
            Ok("obsolete.json"),
            None,
            Duration::from_millis(9999)
        )
        .await
        .expect("late stale finish")
    );
    assert_eq!(
        Jobs::find_by_id(&db, id)
            .await
            .expect("read")
            .expect("job")
            .duration_ms,
        Some(1234)
    );
    assert!(result.version > second.job.version);
    // Exhausted abandoned jobs become terminal instead of remaining permanently runnable.
    let exhausted = Uuid::new_v4();
    Jobs::submit(&db, exhausted, &hash, None, None)
        .await
        .expect("last-attempt job");
    let lease = Jobs::claim(&db, 1, 1)
        .await
        .expect("last attempt")
        .expect("job");
    assert_eq!(lease.job.id, exhausted);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(Jobs::claim(&db, 60, 1).await.expect("reap").is_none());
    assert_eq!(
        Jobs::find_by_id(&db, exhausted)
            .await
            .expect("read terminal failure")
            .expect("job")
            .status,
        JobStatus::Failed
    );
}

/// A locked exhausted job must not block other workers from claiming an unrelated queued document.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn locked_reaper_rows_do_not_block_claims() {
    use docparse_database::{
        entities::parse_jobs,
        seaorm::{EntityTrait, QuerySelect, TransactionTrait},
    };
    let db = connect(
        &DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("db");
    let blocked_id = Uuid::new_v4();
    Jobs::submit(&db, blocked_id, &"a".repeat(64), None, None)
        .await
        .expect("submit");
    let blocked = Jobs::claim(&db, 1, 1).await.expect("claim").expect("job");
    assert_eq!(blocked.job.id, blocked_id);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let transaction = db.begin().await.expect("transaction");
    parse_jobs::Entity::find_by_id(blocked_id)
        .lock_exclusive()
        .one(&transaction)
        .await
        .expect("lock");
    let queued_id = Uuid::new_v4();
    Jobs::submit(&db, queued_id, &"a".repeat(64), None, None)
        .await
        .expect("queued");
    let claimed = tokio::time::timeout(
        Duration::from_millis(500),
        Jobs::claim(&db, 30, 1),
    )
    .await;
    transaction.rollback().await.expect("unlock");
    // Remove only this test's rows before asserting, including when the old reaper stalls.
    parse_jobs::Entity::delete_by_id(blocked_id)
        .exec(&db)
        .await
        .expect("cleanup blocked");
    parse_jobs::Entity::delete_by_id(queued_id)
        .exec(&db)
        .await
        .expect("cleanup queued");
    assert_eq!(
        claimed
            .expect("reaper must skip locked rows")
            .expect("claim")
            .expect("queued job")
            .job
            .id,
        queued_id
    );
}
