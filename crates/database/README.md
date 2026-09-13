# docparse-database

SeaORM 2.0 PostgreSQL connections, generated entities, and durable parse-job queries.
`query::parse_job::ParseJobQuery` owns idempotent insertion, transactional claims,
lease fencing, progress updates, and completion. `JobStatus` is a string-backed
SeaORM active enum; the existing varchar schema and JSON spellings are unchanged.
Completion accepts `Result<&str, &str>` (result path or attempt error), so invalid
combinations cannot be passed to the query. `thiserror` preserves database
failures without importing server-only Snafu or HTTP/APICODE types.

The schema is owned by `docparse-migration`. `connection::connect` applies pending
migrations on the configured database before returning its pool. A PostgreSQL
transaction lock serializes automatic migrations across application instances;
failure rolls back and returns an error instead of exposing a partially migrated
connection. The PostgreSQL database itself must already exist.

Generate entities from the migrated schema using the installed CLI:

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

The automatic-migration regression uses
`DOCPARSE_MIGRATION_TEST_DATABASE_URL`, which must point to an empty disposable
PostgreSQL database. It tests concurrent startup, existing-data upgrades and
failure recovery, and removes only the migration tables it creates.
