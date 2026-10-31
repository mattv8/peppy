-- Retention metadata is deliberately separate from compaction references.  Old
-- attachments remain protected because the default is never reclassified.
ALTER TABLE upload_reservations
  ADD COLUMN retention_class TEXT NOT NULL DEFAULT 'protected'
  CHECK (retention_class IN ('protected', 'message_media'));
ALTER TABLE attachments
  ADD COLUMN retention_class TEXT NOT NULL DEFAULT 'protected'
  CHECK (retention_class IN ('protected', 'message_media')),
  ADD COLUMN finalized_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE attachment_retention_uses (
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  use_id UUID NOT NULL,
  attachment_id UUID NOT NULL,
  device_id UUID NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('message_history', 'pending_send', 'contact_photo', 'restore_critical')),
  released_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (vault_id, use_id),
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);
CREATE INDEX attachment_retention_uses_attachment ON attachment_retention_uses(vault_id, attachment_id);
CREATE INDEX attachment_retention_uses_device ON attachment_retention_uses(vault_id, device_id);

CREATE TABLE attachment_retention_devices (
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  device_id UUID NOT NULL,
  cleanup_ready BOOLEAN NOT NULL DEFAULT FALSE,
  reconciliation_needed BOOLEAN NOT NULL DEFAULT TRUE,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (vault_id, device_id),
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);

CREATE TABLE attachment_removals (
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE RESTRICT,
  attachment_id UUID NOT NULL,
  object_key TEXT NOT NULL,
  ciphertext_bytes BIGINT NOT NULL CHECK (ciphertext_bytes > 0),
  policy_version TEXT,
  removed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  purged_at TIMESTAMPTZ,
  PRIMARY KEY (vault_id, attachment_id)
);
CREATE INDEX attachment_removals_pending ON attachment_removals(vault_id) WHERE purged_at IS NULL;

ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check
  CHECK (reason IN ('upload_attempt', 'superseded_upload', 'expired_reservation', 'public_copy', 'finalized_attachment', 'vault_deleted', 'retention_cleanup'));

CREATE FUNCTION initialize_attachment_retention_device() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO attachment_retention_devices(vault_id, device_id)
  VALUES (NEW.vault_id, NEW.device_id)
  ON CONFLICT (vault_id, device_id) DO NOTHING;
  RETURN NEW;
END;
$$;

CREATE TRIGGER devices_initialize_attachment_retention
AFTER INSERT ON devices
FOR EACH ROW EXECUTE FUNCTION initialize_attachment_retention_device();

INSERT INTO attachment_retention_devices(vault_id, device_id)
SELECT vault_id, device_id FROM devices
ON CONFLICT (vault_id, device_id) DO NOTHING;
