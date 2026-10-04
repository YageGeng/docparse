//! Real PostgreSQL claims verify metric classification at the committed lease boundary.
#![allow(clippy::float_cmp)] // Counters represent these small integer event totals exactly.
use docparse_database::{
    connection::connect,
    entities::parse_jobs,
    query::parse_job::ParseJobQuery as Jobs,
    seaorm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set},
};
use metrics_exporter_prometheus::PrometheusBuilder;
use std::time::Duration;
use uuid::Uuid;

/// A retry and an expired-lease recovery must not be counted as new work or stale completions.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn committed_claims_distinguish_initial_retry_and_recovery() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).expect("recorder");
    let db = connect(
        &docparse_config::DatabaseConfig::builder()
            .url(
                std::env::var("DOCPARSE_TEST_DATABASE_URL")
                    .expect("test database"),
            )
            .build(),
    )
    .await
    .expect("database");
    let id = Uuid::new_v4();
    Jobs::submit(&db, id, &"a".repeat(64), None, None)
        .await
        .expect("submit");
    let first = Jobs::claim(&db, 60, 3)
        .await
        .expect("claim")
        .expect("first attempt");
    assert_eq!(first.job.id, id);
    assert!(
        Jobs::finish(
            &db,
            &first,
            Err("retryable"),
            None,
            Duration::from_secs(1)
        )
        .await
        .expect("requeue")
    );
    let retry = Jobs::claim(&db, 60, 3)
        .await
        .expect("claim")
        .expect("retry");
    // Expire only this owned row through SeaORM; no wall-clock sleeps or raw SQL are required.
    let mut expired = retry.job.clone().into_active_model();
    expired.lease_until = Set(Some(
        (chrono::Utc::now() - chrono::Duration::seconds(1)).fixed_offset(),
    ));
    expired.update(&db).await.expect("expire lease");
    let recovery = Jobs::claim(&db, 60, 3)
        .await
        .expect("claim")
        .expect("recovery");
    assert_eq!(recovery.job.id, id);
    assert!(
        !Jobs::finish(
            &db,
            &retry,
            Ok("stale.json"),
            None,
            Duration::from_secs(1)
        )
        .await
        .expect("stale completion")
    );
    assert!(
        Jobs::finish(
            &db,
            &recovery,
            Ok("result.json"),
            None,
            Duration::from_secs(1)
        )
        .await
        .expect("finish")
    );
    assert!(
        !Jobs::finish(
            &db,
            &recovery,
            Ok("result.json"),
            None,
            Duration::from_secs(1)
        )
        .await
        .expect("duplicate finish")
    );
    parse_jobs::Entity::delete_by_id(id)
        .exec(&db)
        .await
        .expect("cleanup owned row");
    let scrape = prometheus_parse::Scrape::parse(
        handle.render().lines().map(|line| Ok(line.to_owned())),
    )
    .expect("scrape");
    for (name, label, expected) in [
        (
            "docparse_job_attempts_started_total",
            ("kind", "initial"),
            1.0,
        ),
        (
            "docparse_job_attempts_started_total",
            ("kind", "retry"),
            1.0,
        ),
        (
            "docparse_job_attempts_started_total",
            ("kind", "recovery"),
            1.0,
        ),
        (
            "docparse_job_attempts_finished_total",
            ("outcome", "success"),
            1.0,
        ),
        (
            "docparse_job_attempts_finished_total",
            ("outcome", "error"),
            1.0,
        ),
        (
            "docparse_jobs_completed_total",
            ("outcome", "succeeded"),
            1.0,
        ),
    ] {
        let sample = scrape.samples.iter().find(|sample| {
            sample.metric == name && sample.labels.get(label.0) == Some(label.1)
        });
        assert!(
            matches!(sample.map(|sample| &sample.value), Some(prometheus_parse::Value::Counter(value)) if *value == expected),
            "{name} {label:?}"
        );
    }
}
