CREATE TABLE vault_identity_establishments (
  vault_id UUID PRIMARY KEY REFERENCES vaults(vault_id) ON DELETE CASCADE,
  operation_id UUID NOT NULL,
  key_epoch INTEGER NOT NULL CHECK (key_epoch > 0),
  profile_fingerprint CHAR(64) NOT NULL,
  recovery_record BYTEA NOT NULL CHECK (octet_length(recovery_record) = 266),
  established_by_device_id UUID NOT NULL,
  established_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE vault_identity_establishments ENABLE ROW LEVEL SECURITY;
CREATE POLICY community_vault_identity_establishments_scope ON vault_identity_establishments
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

CREATE FUNCTION peppy_vault_identity_establishment_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' AND NOT EXISTS (SELECT 1 FROM vaults WHERE vault_id = OLD.vault_id) THEN
    RETURN OLD;
  END IF;
  RAISE EXCEPTION 'vault identity establishments are immutable'
    USING ERRCODE = 'integrity_constraint_violation';
END
$$;
CREATE TRIGGER vault_identity_establishments_immutable
  BEFORE UPDATE OR DELETE ON vault_identity_establishments
  FOR EACH ROW EXECUTE FUNCTION peppy_vault_identity_establishment_guard();
