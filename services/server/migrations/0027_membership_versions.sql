CREATE TABLE membership_versions (
  vault_id UUID PRIMARY KEY REFERENCES vaults(vault_id) ON DELETE CASCADE,
  version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0)
);

ALTER TABLE membership_versions ENABLE ROW LEVEL SECURITY;
CREATE POLICY community_membership_versions_scope ON membership_versions
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

CREATE FUNCTION membership_version_initialize() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path FROM CURRENT AS $$
BEGIN
  INSERT INTO membership_versions(vault_id) VALUES (NEW.vault_id);
  RETURN NEW;
END;
$$;

CREATE FUNCTION membership_version_invalidate() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path FROM CURRENT AS $$
DECLARE
  changed_vault_id uuid;
  updated integer;
BEGIN
  IF TG_OP = 'DELETE' THEN
    changed_vault_id := OLD.vault_id;
  ELSE
    changed_vault_id := NEW.vault_id;
  END IF;
  UPDATE membership_versions
  SET version = version + 1
  WHERE vault_id = changed_vault_id AND version < 9223372036854775807;
  GET DIAGNOSTICS updated = ROW_COUNT;
  IF updated = 1 THEN
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
  END IF;

  IF NOT EXISTS (SELECT 1 FROM vaults WHERE vault_id = changed_vault_id) THEN
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'membership version missing or exhausted for vault %', changed_vault_id;
END;
$$;

CREATE TRIGGER vaults_initialize_membership_version
AFTER INSERT ON vaults
FOR EACH ROW EXECUTE FUNCTION membership_version_initialize();
CREATE TRIGGER vaults_invalidate_membership_version
AFTER UPDATE OF public_key_profile, encrypted_vault_check_header, key_epoch, profile_fingerprint ON vaults
FOR EACH ROW WHEN (
  OLD.public_key_profile IS DISTINCT FROM NEW.public_key_profile OR
  OLD.encrypted_vault_check_header IS DISTINCT FROM NEW.encrypted_vault_check_header OR
  OLD.key_epoch IS DISTINCT FROM NEW.key_epoch OR
  OLD.profile_fingerprint IS DISTINCT FROM NEW.profile_fingerprint
)
EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER devices_invalidate_membership_version
AFTER INSERT OR DELETE OR UPDATE OF device_id, role, public_key, profile_fingerprint, key_epoch, revoked_at ON devices
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER device_credentials_invalidate_membership_version
AFTER INSERT OR DELETE OR UPDATE OF token_digest, vault_id, device_id, revoked_at ON device_credentials
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER vault_key_profiles_invalidate_membership_version
AFTER INSERT OR DELETE ON vault_key_profiles
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER vault_admission_keys_invalidate_membership_version
AFTER INSERT OR DELETE ON vault_admission_keys
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER device_admissions_invalidate_membership_version
AFTER INSERT OR DELETE ON device_admissions
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER phone_invitations_invalidate_membership_version
AFTER INSERT OR DELETE ON phone_invitations
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER computer_enrollments_invalidate_membership_version
AFTER INSERT OR DELETE ON computer_enrollments
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();

INSERT INTO membership_versions(vault_id)
SELECT vault_id FROM vaults
ON CONFLICT (vault_id) DO NOTHING;

REVOKE ALL ON TABLE membership_versions FROM PUBLIC;
REVOKE ALL ON FUNCTION membership_version_initialize() FROM PUBLIC;
REVOKE ALL ON FUNCTION membership_version_invalidate() FROM PUBLIC;
