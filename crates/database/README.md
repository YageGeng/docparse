# docparse-database

SeaORM 2.0 PostgreSQL connections, generated entities, and durable parse-job queries.
`query::parse_job::ParseJobQuery` owns idempotent insertion, transactional claims,
lease fencing, progress updates, and completion. `JobStatus` is a string-backed
SeaORM active enum; the existing varchar schema and JSON spellings are unchanged.
Completion accepts `Result<&str, &str>` (result path or attempt error), so invalid
combinations cannot be passed to the query. `thiserror` preserves database
failures without importing server-only Snafu or HTTP/APICODE types.

The schema is owned by `docparse-migration`. Run migrations before API/worker
startup. Generate entities from the migrated schema using the installed CLI:

```bash
rtk proxy sea-orm-cli generate entity --tables parse_jobs \
  --output-dir crates/database/src/entities --entity-format compact \
  --with-serde both --model-extra-derives typed_builder::TypedBuilder
```

Supply `DATABASE_URL` through the environment. Keep the generated model's
function/type comments and `#[builder(default)]` attributes on optional fields
and the `Model::status: JobStatus` mapping when reviewing regeneration. Query logic belongs in `query`, not generated files.
All runtime queries use entities and SeaQuery expressions; PostgreSQL clock and
interval functions are expressed with `Func::cust`, without raw SQL strings.
