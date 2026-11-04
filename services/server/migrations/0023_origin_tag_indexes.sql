-- Built inside the migration transaction: sqlx serializes migrators with an advisory lock, and
-- CREATE INDEX CONCURRENTLY would wait on a migrator blocked by that lock (deadlock). The build
-- holds a SHARE lock on encrypted_records, blocking writes (not reads) for the duration of the scan.
CREATE UNIQUE INDEX encrypted_records_origin_tag_unique
  ON encrypted_records(vault_id, origin_tag) WHERE origin_tag IS NOT NULL;
