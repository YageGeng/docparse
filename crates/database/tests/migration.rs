use docparse_config::DatabaseConfig;
use docparse_database::{
    connection::connect, error::DatabaseError,
    query::parse_job::ParseJobQuery as Jobs, seaorm::Database,
};
use docparse_migration::{
    Alias, ColumnDef, Migrator, MigratorTrait, SchemaManager, Table,
};
use uuid::Uuid;

/// Concurrent connections must initialize an empty database, preserve old rows on upgrade, and reject failed migrations.
#[tokio::test]
#[ignore = "requires DOCPARSE_MIGRATION_TEST_DATABASE_URL pointing at an empty disposable PostgreSQL database"]
async fn connection_applies_migrations_before_returning() {
    let url = std::env::var("DOCPARSE_MIGRATION_TEST_DATABASE_URL")
        .expect("isolated migration database URL");
    let admin = Database::connect(url.clone())
        .await
        .expect("admin connection");
    let schema = SchemaManager::new(&admin);
    // Never reset an existing task database: this test may only own tables it creates itself.
    assert!(
        !schema
            .has_table("parse_jobs")
            .await
            .expect("empty database")
    );
    let config = DatabaseConfig::builder()
        .url(url)
        .max_connections(1)
        .min_connections(0)
        .build();
    let (first, second) = tokio::join!(connect(&config), connect(&config));
    let db = first.expect("first instance initializes the database");
    let peer =
        second.expect("concurrent instance waits for migration completion");
    Jobs::ready(&db)
        .await
        .expect("connect must apply every migration before returning");
    Jobs::ready(&peer)
        .await
        .expect("both returned connections have the full schema");
    assert!(
        Migrator::get_pending_migrations(&db)
            .await
            .expect("migration status")
            .is_empty()
    );

    let id = Uuid::new_v4();
    let hash = "c".repeat(64);
    Jobs::submit(&db, id, &hash, None, None)
        .await
        .expect("existing job");
    peer.close().await.expect("close peer");
    // Return to the initial schema while retaining a real task row, including when later migrations are added.
    let rollback_steps =
        u32::try_from(Migrator::migrations().len().saturating_sub(1))
            .expect("migration count");
    Migrator::down(&db, Some(rollback_steps))
        .await
        .expect("old schema");
    db.close().await.expect("close initial pool");
    let upgraded = connect(&config).await.expect("upgrade on connect");
    let saved = Jobs::find_by_id(&upgraded, id)
        .await
        .expect("query upgraded schema")
        .expect("job preserved");
    assert_eq!(saved.input_hash, hash);
    assert!(saved.filename.is_none());
    assert!(saved.size_bytes.is_none());
    assert!(saved.duration_ms.is_none());
    assert!(saved.deleted_at.is_none());
    upgraded.close().await.expect("close upgraded pool");

    // A conflicting column makes the pending migration fail; no half-migrated pool may escape connect().
    Migrator::down(&admin, Some(rollback_steps))
        .await
        .expect("prepare migration failure");
    schema
        .alter_table(
            Table::alter()
                .table(Alias::new("parse_jobs"))
                .add_column(
                    ColumnDef::new(Alias::new("filename")).string().null(),
                )
                .to_owned(),
        )
        .await
        .expect("conflicting column");
    assert!(matches!(
        connect(&config).await,
        Err(DatabaseError::SeaOrm(_))
    ));
    assert!(
        !schema
            .has_column("parse_jobs", "size_bytes")
            .await
            .expect("failed migration rolled back")
    );
    schema
        .alter_table(
            Table::alter()
                .table(Alias::new("parse_jobs"))
                .drop_column(Alias::new("filename"))
                .to_owned(),
        )
        .await
        .expect("remove conflict");
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        connect(&config),
    )
    .await
    .expect("failed migration released its lock")
    .expect("retry succeeds");
    Jobs::ready(&recovered)
        .await
        .expect("complete schema after recovery");
    recovered.close().await.expect("close recovered pool");
    // Only the migration-owned tables created above are removed from this dedicated test database.
    Migrator::reset(&admin)
        .await
        .expect("remove migration test tables");
    admin.close().await.expect("close admin pool");
}
