-- Enrollment intents and pending wake work cannot be reconstructed after rollback.
CREATE OR REPLACE FUNCTION peppy_reject_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' AND current_setting('peppy.compaction', true) = 'on' THEN
    RETURN OLD;
  END IF;
  RAISE EXCEPTION '% rows are immutable', TG_TABLE_NAME
    USING ERRCODE = 'integrity_constraint_violation';
END
$$;
CREATE OR REPLACE FUNCTION peppy_key_profile_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'vault key profiles are immutable'
      USING ERRCODE = 'integrity_constraint_violation';
  END IF;
  IF NEW.vault_id IS DISTINCT FROM OLD.vault_id
     OR NEW.key_epoch IS DISTINCT FROM OLD.key_epoch
     OR NEW.public_key_profile IS DISTINCT FROM OLD.public_key_profile
     OR NEW.encrypted_vault_check_header IS DISTINCT FROM OLD.encrypted_vault_check_header
     OR NEW.profile_fingerprint IS DISTINCT FROM OLD.profile_fingerprint
     OR NEW.created_by_device_id IS DISTINCT FROM OLD.created_by_device_id
     OR NEW.created_at IS DISTINCT FROM OLD.created_at
     OR (OLD.activated_at IS NOT NULL AND NEW.activated_at IS DISTINCT FROM OLD.activated_at)
  THEN
    RAISE EXCEPTION 'vault key profiles are immutable'
      USING ERRCODE = 'integrity_constraint_violation';
  END IF;
  RETURN NEW;
END
$$;

ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
-- Vault-deletion intents have no predecessor enum value. They are retained in
-- the migration-repair backup but must be removed to restore the old check.
DELETE FROM storage_deletions WHERE reason = 'vault_deleted';
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check
  CHECK (reason IN ('upload_attempt', 'superseded_upload', 'expired_reservation', 'public_copy', 'finalized_attachment'));
DROP INDEX device_wake_jobs_due;
DROP TABLE device_wake_jobs;
DROP TABLE device_wake_routes;
DROP INDEX pairing_intents_expiry;
DROP TABLE pairing_intents;
