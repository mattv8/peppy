DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM draft_incarnations)
     OR EXISTS (SELECT 1 FROM draft_branches)
     OR EXISTS (SELECT 1 FROM draft_revisions)
     OR EXISTS (SELECT 1 FROM draft_terminals)
     OR EXISTS (SELECT 1 FROM draft_attachment_holds)
     OR EXISTS (SELECT 1 FROM attachments WHERE draft_id IS NOT NULL)
     OR EXISTS (SELECT 1 FROM upload_reservations WHERE draft_id IS NOT NULL)
     OR EXISTS (SELECT 1 FROM storage_deletions WHERE reason = 'draft_expired') THEN
    RAISE EXCEPTION 'cannot downgrade 0026_draft_retention: draft data is present'
      USING HINT = 'Draft terminal metadata is retained indefinitely; discarding or expiring drafts is not sufficient. Keep a compatible server version, or prepare an explicitly reviewed data-preserving recovery migration.';
  END IF;
END $$;
DROP TABLE draft_attachment_holds;
DROP TABLE draft_terminals;
DROP TABLE draft_revisions;
DROP TABLE draft_branches;
DROP TABLE draft_incarnations;
DROP INDEX attachments_draft_id;
ALTER TABLE attachments DROP COLUMN draft_id;
ALTER TABLE upload_reservations DROP COLUMN draft_id;
ALTER TABLE vaults DROP COLUMN draft_change_seq;
ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check CHECK (reason IN ('upload_attempt','superseded_upload','expired_reservation','public_copy','finalized_attachment','vault_deleted','retention_cleanup'));
