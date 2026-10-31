DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM attachment_removals)
     OR EXISTS (SELECT 1 FROM storage_deletions WHERE reason = 'retention_cleanup') THEN
    RAISE EXCEPTION '0019 downgrade refused: retention tombstones or cleanup authority still exist';
  END IF;
END;
$$;

DROP TRIGGER devices_initialize_attachment_retention ON devices;
DROP FUNCTION initialize_attachment_retention_device();
DROP TABLE attachment_retention_uses;
DROP TABLE attachment_retention_devices;
DROP TABLE attachment_removals;
ALTER TABLE attachments DROP COLUMN finalized_at, DROP COLUMN retention_class;
ALTER TABLE upload_reservations DROP COLUMN retention_class;
ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check
  CHECK (reason IN ('upload_attempt', 'superseded_upload', 'expired_reservation', 'public_copy', 'finalized_attachment', 'vault_deleted'));
