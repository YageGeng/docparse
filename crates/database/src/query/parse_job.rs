use crate::{
    JobStatus,
    entities::parse_jobs::{self, Column, Entity, Model},
    error::DatabaseError,
};
use sea_orm::sea_query::{Expr, ExprTrait, Func, LockBehavior, LockType};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait,
    DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait, TransactionTrait,
};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

/// The immutable token fences one attempt from workers that resume after their lease expires.
#[derive(Debug)]
pub struct Lease {
    pub job: Model,
    pub token: Uuid,
}

impl Lease {
    /// Uses PostgreSQL's clock for ownership checks so different worker clocks cannot extend stale leases.
    fn condition(&self) -> Condition {
        Condition::all()
            .add(Column::Id.eq(self.job.id))
            .add(Column::Status.eq(JobStatus::Running))
            .add(Column::LeaseToken.eq(self.token))
            .add(
                Expr::col(Column::LeaseUntil).gt(Func::cust("clock_timestamp")),
            )
    }

    /// Builds a typed PostgreSQL interval expression without interpolating raw SQL.
    fn deadline(seconds: i32) -> Expr {
        Func::cust("clock_timestamp").add(Func::cust("make_interval").args([
            Expr::val(0_i32),
            Expr::val(0_i32),
            Expr::val(0_i32),
            Expr::val(0_i32),
            Expr::val(0_i32),
            Expr::val(0_i32),
            Expr::val(f64::from(seconds)),
        ]))
    }
}

/// Task queries use generated entities and SeaQuery expressions for every database operation.
pub struct ParseJobQuery;

impl ParseJobQuery {
    /// Inserts a task or returns its existing identity; conflicting payloads never mutate the original task.
    /// Requires autocommit ownership because emitted metrics cannot be rolled back with an outer transaction.
    /// ```compile_fail
    /// use docparse_database::{seaorm::DatabaseTransaction, query::parse_job::ParseJobQuery};
    /// async fn submit_inside_transaction(db: &DatabaseTransaction) {
    ///     let _ = ParseJobQuery::submit(db, uuid::Uuid::nil(), &"0".repeat(64), None, None).await;
    /// }
    /// ```
    pub async fn submit(
        db: &DatabaseConnection,
        id: Uuid,
        hash: &str,
        filename: Option<&str>,
        size_bytes: Option<i64>,
    ) -> Result<Model, DatabaseError> {
        if filename
            .is_some_and(|name| name.is_empty() || name.chars().count() > 255)
            || size_bytes.is_some_and(|size| size <= 0)
            || hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DatabaseError::InvalidInput);
        }
        // ActiveModel setters preserve database defaults for timestamps and initial queue state.
        let mut active: parse_jobs::ActiveModel = Default::default();
        active.set(Column::Id, id.into());
        active.set(Column::InputHash, hash.into());
        // Metadata is inserted with the task; idempotent replays never rename or resize the original submission.
        active.set(Column::Filename, filename.map(str::to_owned).into());
        active.set(Column::SizeBytes, size_bytes.into());
        let inserted = Entity::insert(active)
            .on_conflict_do_nothing()
            .exec(db)
            .await?;
        let job = Entity::find_by_id(id).one(db).await?.ok_or_else(|| {
            DbErr::RecordNotFound("submitted task disappeared".into())
        })?;
        // A replay must not resurrect an explicitly deleted task under its previous submission identity.
        if job.input_hash != hash || job.deleted_at.is_some() {
            return Err(DatabaseError::IdempotencyConflict);
        }
        if matches!(inserted, sea_orm::TryInsertResult::Inserted(_)) {
            metrics::counter!("docparse_jobs_submitted_total").increment(1);
        }
        Ok(job)
    }

    /// Fetches a visible durable task snapshot on every API replica, excluding deleted results.
    pub async fn find_by_id<C: ConnectionTrait>(
        db: &C,
        id: Uuid,
    ) -> Result<Option<Model>, DatabaseError> {
        Ok(Entity::find_by_id(id)
            .filter(Column::DeletedAt.is_null())
            .one(db)
            .await?)
    }

    /// Reads one stable history page using creation time and UUID as a deterministic cursor ordering.
    pub async fn list<C: ConnectionTrait>(
        db: &C,
        cursor: Option<Uuid>,
        limit: u64,
        status: Option<JobStatus>,
        search: Option<&str>,
    ) -> Result<Vec<Model>, DatabaseError> {
        if !(1..=100).contains(&limit)
            || search.is_some_and(|text| text.chars().count() > 200)
        {
            return Err(DatabaseError::InvalidInput);
        }
        let mut query = Entity::find().filter(Column::DeletedAt.is_null());
        if let Some(id) = cursor {
            // Tombstones remain valid pagination anchors even after their rows disappear from the list.
            let anchor = Entity::find_by_id(id)
                .one(db)
                .await?
                .ok_or(DatabaseError::InvalidInput)?;
            query = query.filter(
                Condition::any()
                    .add(Column::CreatedAt.lt(anchor.created_at))
                    .add(
                        Condition::all()
                            .add(Column::CreatedAt.eq(anchor.created_at))
                            .add(Column::Id.lt(anchor.id)),
                    ),
            );
        }
        if let Some(status) = status {
            query = query.filter(Column::Status.eq(status));
        }
        if let Some(search) = search.filter(|text| !text.is_empty()) {
            // LIKE wildcard characters supplied by a user are literals, not a request to broaden the search.
            let escaped = search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            query = query.filter(
                Expr::expr(Func::lower(Expr::col(Column::Filename))).like(
                    sea_orm::sea_query::LikeExpr::new(format!(
                        "%{}%",
                        escaped.to_lowercase()
                    ))
                    .escape('\\'),
                ),
            );
        }
        Ok(query
            .order_by_desc(Column::CreatedAt)
            .order_by_desc(Column::Id)
            .limit(limit + 1)
            .all(db)
            .await?)
    }

    /// Commits deletion intent on the pool before cleanup; accepting a pool prevents an uncommitted transaction from escaping.
    pub async fn mark_deleted(
        db: &DatabaseConnection,
        id: Uuid,
    ) -> Result<Option<Model>, DatabaseError> {
        // Preserve the original marker when a client retries an already accepted deletion.
        Ok(Entity::update_many()
            .col_expr(
                Column::DeletedAt,
                Func::coalesce([
                    Expr::col(Column::DeletedAt),
                    Func::cust("clock_timestamp").into(),
                ])
                .into(),
            )
            .filter(Column::Id.eq(id))
            .filter(
                Column::Status.is_in([JobStatus::Succeeded, JobStatus::Failed]),
            )
            .exec_with_returning(db)
            .await?
            .into_iter()
            .next())
    }

    /// Reads bounded cleanup batches by UUID so a failed file cannot starve later pending results.
    pub async fn pending_cleanup<C: ConnectionTrait>(
        db: &C,
        after: Option<Uuid>,
    ) -> Result<Vec<Model>, DatabaseError> {
        let mut query = Entity::find()
            .filter(Column::DeletedAt.is_not_null())
            .filter(Column::ResultPath.is_not_null());
        if let Some(after) = after {
            query = query.filter(Column::Id.gt(after));
        }
        Ok(query.order_by_asc(Column::Id).limit(64).all(db).await?)
    }

    /// Acknowledges a durably removed result without altering another worker's or another task's file reference.
    pub async fn finish_cleanup<C: ConnectionTrait>(
        db: &C,
        id: Uuid,
        name: &str,
    ) -> Result<(), DatabaseError> {
        Entity::update_many()
            .col_expr(Column::ResultPath, Expr::val(Option::<String>::None))
            .filter(Column::Id.eq(id))
            .filter(Column::DeletedAt.is_not_null())
            .filter(Column::ResultPath.eq(name))
            .exec(db)
            .await?;
        Ok(())
    }

    /// Checks the generated entity's columns without reading task data or reporting a missing migration as ready.
    pub async fn ready<C: ConnectionTrait>(
        db: &C,
    ) -> Result<(), DatabaseError> {
        Entity::find().limit(0).all(db).await?;
        Ok(())
    }

    /// Selects queue entries and expired attempts while excluding work still owned by a live worker.
    fn available() -> Condition {
        Condition::any()
            .add(Column::Status.eq(JobStatus::Queued))
            .add(
                Condition::all()
                    .add(Column::Status.eq(JobStatus::Running))
                    .add(
                        Expr::col(Column::LeaseUntil)
                            .lte(Func::cust("clock_timestamp")),
                    ),
            )
    }

    /// Aggregates durable backlog without loading PDFs or scanning task rows into application memory.
    pub async fn backlog(
        db: &DatabaseConnection,
    ) -> Result<
        Vec<(
            &'static str,
            i64,
            Option<chrono::DateTime<chrono::FixedOffset>>,
        )>,
        DatabaseError,
    > {
        let mut values = Vec::with_capacity(3);
        for (state, filter, origin) in [
            (
                "queued",
                Condition::all().add(Column::Status.eq(JobStatus::Queued)),
                Func::coalesce([
                    Expr::col(Column::QueuedAt),
                    Expr::col(Column::CreatedAt),
                ])
                .into(),
            ),
            (
                "running",
                Condition::all()
                    .add(Column::Status.eq(JobStatus::Running))
                    .add(
                        Expr::col(Column::LeaseUntil)
                            .gt(Func::cust("clock_timestamp")),
                    ),
                Expr::col(Column::AttemptStartedAt),
            ),
            (
                "recovery",
                Condition::all()
                    .add(Column::Status.eq(JobStatus::Running))
                    .add(
                        Expr::col(Column::LeaseUntil)
                            .lte(Func::cust("clock_timestamp")),
                    ),
                Expr::col(Column::LeaseUntil),
            ),
        ] {
            let (count, oldest) = Entity::find().select_only()
                .column_as(Column::Id.count(), "count")
                .expr_as(Func::min::<Expr>(origin), "oldest")
                .filter(Column::DeletedAt.is_null()).filter(filter)
                .into_tuple::<(i64, Option<chrono::DateTime<chrono::FixedOffset>>)>()
                .one(db).await?.unwrap_or((0, None));
            values.push((state, count, oldest));
        }
        Ok(values)
    }

    /// Claims one row under SKIP LOCKED and commits its new fencing token before inference begins.
    pub async fn claim(
        db: &DatabaseConnection,
        seconds: i32,
        max_attempts: i32,
    ) -> Result<Option<Lease>, DatabaseError> {
        if !(1..=86400).contains(&seconds) || !(1..=100).contains(&max_attempts)
        {
            return Err(DatabaseError::InvalidInput);
        }
        let txn = db.begin().await?;
        // Reaping must also skip locked rows; otherwise one expired row stalls every worker before its claim.
        let expired = Entity::find()
            .select_only()
            .column(Column::Id)
            .filter(Self::available())
            .filter(Column::Attempts.gte(max_attempts))
            .order_by_asc(Column::CreatedAt)
            .limit(64)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .into_query();
        let exhausted = Entity::update_many()
            .col_expr(Column::Status, Expr::val(JobStatus::Failed))
            // Terminal timestamps are independent of heartbeat and deletion updates.
            .col_expr(Column::FinishedAt, Func::cust("clock_timestamp").into())
            .col_expr(Column::LeaseToken, Expr::val(Option::<Uuid>::None))
            .col_expr(
                Column::LeaseUntil,
                Expr::val(
                    Option::<chrono::DateTime<chrono::FixedOffset>>::None,
                ),
            )
            .col_expr(
                Column::Error,
                Func::coalesce([
                    Expr::col(Column::Error),
                    Expr::val("worker lease expired; retry limit reached"),
                ])
                .into(),
            )
            .col_expr(Column::Version, Expr::col(Column::Version).add(1))
            .col_expr(Column::UpdatedAt, Func::cust("clock_timestamp").into())
            .filter(Expr::col(Column::Id).in_subquery(expired))
            .exec_with_returning(&txn)
            .await?;
        let job = Entity::find()
            .filter(Self::available())
            .filter(Column::Attempts.lt(max_attempts))
            .order_by_asc(Column::CreatedAt)
            .order_by_asc(Column::Id)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .one(&txn)
            .await?;
        let Some(job) = job else {
            txn.commit().await?;
            for job in &exhausted {
                job.record_terminal("failed");
            }
            return Ok(None);
        };
        let queue_origin = if job.status == JobStatus::Running {
            job.lease_until
        } else {
            job.queued_at.or(Some(job.created_at))
        };
        let token = Uuid::new_v4();
        let job = Entity::update_many()
            .col_expr(Column::Status, Expr::val(JobStatus::Running))
            // Preserve the first start while recording the origin of this fenced attempt.
            .col_expr(
                Column::StartedAt,
                Func::coalesce([
                    Expr::col(Column::StartedAt),
                    Func::cust("clock_timestamp").into(),
                ])
                .into(),
            )
            .col_expr(
                Column::AttemptStartedAt,
                Func::cust("clock_timestamp").into(),
            )
            .col_expr(Column::Attempts, Expr::col(Column::Attempts).add(1))
            .col_expr(Column::Version, Expr::col(Column::Version).add(1))
            .col_expr(Column::LeaseToken, Expr::val(token))
            .col_expr(Column::LeaseUntil, Lease::deadline(seconds))
            .col_expr(Column::Progress, Expr::val(Option::<Value>::None))
            // Each retry measures its own execution; an interrupted attempt must not inherit an earlier duration.
            .col_expr(Column::DurationMs, Expr::val(Option::<i64>::None))
            .col_expr(Column::Error, Expr::val(Option::<String>::None))
            .col_expr(Column::UpdatedAt, Func::cust("clock_timestamp").into())
            .filter(Column::Id.eq(job.id))
            .exec_with_returning(&txn)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                DbErr::RecordNotFound("claimed task disappeared".into())
            })?;
        txn.commit().await?;
        for job in &exhausted {
            job.record_terminal("failed");
        }
        if let (Some(origin), Some(start)) =
            (queue_origin, job.attempt_started_at)
        {
            metrics::histogram!("docparse_job_queue_wait_seconds").record(
                Ord::max((start - origin).num_milliseconds(), 0) as f64
                    / 1000.0,
            );
        }
        metrics::counter!("docparse_job_attempts_started_total").increment(1);
        Ok(Some(Lease { job, token }))
    }

    /// Renews live ownership, optionally recording the latest coalesced progress snapshot.
    pub async fn heartbeat<C: ConnectionTrait>(
        db: &C,
        lease: &Lease,
        seconds: i32,
        progress: Option<Value>,
    ) -> Result<bool, DatabaseError> {
        if !(1..=86400).contains(&seconds) {
            return Err(DatabaseError::InvalidInput);
        }
        let mut update = Entity::update_many()
            .col_expr(Column::LeaseUntil, Lease::deadline(seconds))
            .col_expr(Column::UpdatedAt, Func::cust("clock_timestamp").into())
            .filter(lease.condition());
        if let Some(progress) = progress {
            update = update
                .col_expr(Column::Progress, Expr::val(progress))
                .col_expr(Column::Version, Expr::col(Column::Version).add(1));
        }
        Ok(update.exec(db).await?.rows_affected == 1)
    }

    /// Publishes progress, elapsed time, and attempt status atomically while the worker still owns a live lease.
    /// Requires a pool so completion metrics describe a committed transition, never an outer transaction.
    /// ```compile_fail
    /// use docparse_database::{seaorm::DatabaseTransaction, query::parse_job::{ParseJobQuery, Lease}};
    /// async fn finish_inside_transaction(db: &DatabaseTransaction, lease: &Lease) {
    ///     let _ = ParseJobQuery::finish(db, lease, Ok("result.json"), None, std::time::Duration::ZERO).await;
    /// }
    /// ```
    pub async fn finish(
        db: &DatabaseConnection,
        lease: &Lease,
        outcome: Result<&str, &str>,
        progress: Option<Value>,
        duration: Duration,
    ) -> Result<bool, DatabaseError> {
        let duration_ms = i64::try_from(duration.as_millis())
            .map_err(|_overflow| DatabaseError::InvalidInput)?;
        // The API admits exactly one outcome; nullable columns are only a persistence detail.
        let (status, result, error) = match outcome {
            Ok(path) => (JobStatus::Succeeded, Some(path), None),
            Err(message) => (JobStatus::Queued, None, Some(message)),
        };
        let success = outcome.is_ok();
        let jobs = Entity::update_many()
            // A failed attempt requeues; only a successful terminal transition sets finished_at.
            .col_expr(
                Column::FinishedAt,
                if success {
                    Func::cust("clock_timestamp").into()
                } else {
                    Expr::val(
                        Option::<chrono::DateTime<chrono::FixedOffset>>::None,
                    )
                },
            )
            .col_expr(
                Column::QueuedAt,
                if success {
                    Expr::col(Column::QueuedAt)
                } else {
                    Func::cust("clock_timestamp").into()
                },
            )
            .col_expr(Column::Status, Expr::val(status))
            // The final callback can arrive between periodic flushes; persist it with the terminal transition.
            .col_expr(
                Column::Progress,
                Func::coalesce([
                    Expr::val(progress),
                    Expr::col(Column::Progress),
                ])
                .into(),
            )
            .col_expr(Column::ResultPath, Expr::val(result))
            .col_expr(Column::DurationMs, Expr::val(duration_ms))
            .col_expr(Column::Error, Expr::val(error))
            .col_expr(Column::LeaseToken, Expr::val(Option::<Uuid>::None))
            .col_expr(
                Column::LeaseUntil,
                Expr::val(
                    Option::<chrono::DateTime<chrono::FixedOffset>>::None,
                ),
            )
            .col_expr(Column::Version, Expr::col(Column::Version).add(1))
            .col_expr(Column::UpdatedAt, Func::cust("clock_timestamp").into())
            .filter(lease.condition())
            .exec_with_returning(db)
            .await?;
        if let Some(job) = jobs.first() {
            let outcome = if success { "success" } else { "error" };
            metrics::counter!("docparse_job_attempts_finished_total", "outcome" => outcome).increment(1);
            metrics::histogram!("docparse_job_attempt_seconds", "outcome" => outcome).record(duration.as_secs_f64());
            if success {
                job.record_terminal("succeeded");
            }
        }
        Ok(!jobs.is_empty())
    }
}

impl Model {
    /// Counts only committed terminal transitions, never retries or stale fencing tokens.
    fn record_terminal(&self, outcome: &'static str) {
        metrics::counter!("docparse_jobs_completed_total", "outcome" => outcome).increment(1);
        if let Some(finished) = self.finished_at {
            metrics::histogram!("docparse_job_end_to_end_seconds", "outcome" => outcome)
                .record(Ord::max((finished - self.created_at).num_milliseconds(), 0) as f64 / 1000.0);
        }
    }
}
