-- Snapshot identities and key-profile history added by this migration cannot be
-- recovered after this schema rollback.
DROP TRIGGER vault_key_profiles_immutable ON vault_key_profiles;
DROP FUNCTION peppy_key_profile_guard();
DROP TABLE vault_key_profiles;

DROP INDEX commands_gateway_pending;
DROP INDEX outbox_jobs_delivered;
DROP INDEX outbox_jobs_due;
-- Version 0007 made payload nullable. Jobs without their legacy payload cannot
-- satisfy the predecessor contract, so remove them during backed-up rollback.
DELETE FROM outbox_jobs WHERE payload IS NULL;
ALTER TABLE outbox_jobs
  DROP COLUMN delivered_at,
  ALTER COLUMN payload SET NOT NULL;

DROP INDEX event_log_retention;
ALTER TABLE vaults
  DROP CONSTRAINT vaults_replay_floor_bounds,
  DROP COLUMN replay_floor_cursor;

DROP TRIGGER encrypted_records_immutable ON encrypted_records;
DROP FUNCTION peppy_reject_mutation();
DROP INDEX encrypted_records_command_unique;
ALTER TABLE encrypted_records
  DROP CONSTRAINT encrypted_records_command_shape,
  DROP CONSTRAINT encrypted_records_purpose_check,
  DROP CONSTRAINT encrypted_records_sequence_positive,
  DROP CONSTRAINT encrypted_records_cursor_positive,
  DROP CONSTRAINT encrypted_records_sequence_unique,
  DROP CONSTRAINT encrypted_records_cursor_unique,
  DROP CONSTRAINT encrypted_records_pkey,
  ADD PRIMARY KEY (vault_id, envelope_id),
  DROP COLUMN created_at,
  DROP COLUMN cipher_digest,
  DROP COLUMN command_id,
  DROP COLUMN purpose,
  DROP COLUMN producer_sequence,
  DROP COLUMN cursor,
  DROP COLUMN producer_device_id;
