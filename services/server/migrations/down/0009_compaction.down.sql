-- Compaction decisions and attachment-reference registrations cannot be restored.
CREATE OR REPLACE FUNCTION peppy_reject_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION '% rows are immutable', TG_TABLE_NAME
    USING ERRCODE = 'integrity_constraint_violation';
END
$$;

ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
-- These rows have no predecessor enum value. The migration-repair backup
-- preserves them before this lossy rollback removes them.
DELETE FROM storage_deletions WHERE reason = 'finalized_attachment';
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check
  CHECK (reason IN ('upload_attempt', 'superseded_upload', 'expired_reservation', 'public_copy'));
-- Compaction could reclaim a private attachment while retaining its public
-- derivatives. Remove those now-orphaned rows before restoring the old FK.
DELETE FROM public_attachment_copies copy
WHERE NOT EXISTS (
  SELECT 1 FROM attachments attachment
  WHERE attachment.vault_id = copy.vault_id
    AND attachment.attachment_id = copy.attachment_id
);
ALTER TABLE public_attachment_copies
  ADD CONSTRAINT public_attachment_copies_vault_id_attachment_id_fkey
  FOREIGN KEY (vault_id, attachment_id) REFERENCES attachments(vault_id, attachment_id);

DROP INDEX record_supersessions_target;
DROP TABLE record_supersessions;
DROP INDEX record_compaction_key;
DROP TABLE record_compaction;
DROP TABLE attachment_record_references;
DROP TABLE compacted_records;
DROP INDEX devices_compaction_fence;
ALTER TABLE devices DROP COLUMN compaction_generation_fence;
ALTER TABLE vaults
  DROP COLUMN last_compacted_at,
  DROP COLUMN compaction_generation;
