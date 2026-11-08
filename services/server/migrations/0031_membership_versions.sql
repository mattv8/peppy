CREATE TABLE membership_versions (
  vault_id UUID PRIMARY KEY REFERENCES vaults(vault_id) ON DELETE CASCADE,
  version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0)
);

ALTER TABLE membership_versions ENABLE ROW LEVEL SECURITY;
CREATE POLICY community_membership_versions_scope ON membership_versions
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

DO $functions$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($sql$
    CREATE FUNCTION %1$I.membership_version_initialize() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp AS $body$
    BEGIN
      INSERT INTO %1$I.membership_versions(vault_id) VALUES (NEW.vault_id);
      RETURN NEW;
    END;
    $body$
  $sql$, schema_name);
  EXECUTE format($sql$
    CREATE FUNCTION %1$I.membership_version_invalidate() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp AS $body$
    DECLARE changed_vault_id uuid;
    DECLARE updated integer;
    BEGIN
      FOR changed_vault_id IN
        SELECT vault_id FROM (
          SELECT CASE WHEN TG_OP = 'DELETE' THEN OLD.vault_id ELSE NEW.vault_id END AS vault_id
          UNION
          SELECT CASE WHEN TG_OP = 'UPDATE' THEN OLD.vault_id END
        ) changed_vaults
        WHERE vault_id IS NOT NULL
      LOOP
        UPDATE %1$I.membership_versions
        SET version = version + 1
        WHERE vault_id = changed_vault_id AND version < 9223372036854775807;
        GET DIAGNOSTICS updated = ROW_COUNT;
        IF updated <> 1 AND EXISTS (SELECT 1 FROM %1$I.vaults WHERE vault_id = changed_vault_id) THEN
          RAISE EXCEPTION 'membership version missing or exhausted for vault %%', changed_vault_id;
        END IF;
      END LOOP;
      IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
      RETURN NEW;
    END;
    $body$
  $sql$, schema_name);
END $functions$;

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
AFTER INSERT OR DELETE OR UPDATE OF vault_id, device_id, role, public_key, profile_fingerprint, key_epoch, revoked_at ON devices
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER device_credentials_invalidate_membership_version
AFTER INSERT OR DELETE OR UPDATE OF token_digest, vault_id, device_id, revoked_at ON device_credentials
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER vault_key_profiles_invalidate_membership_version
AFTER INSERT OR DELETE ON vault_key_profiles
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();
CREATE TRIGGER vault_admission_keys_invalidate_membership_version
AFTER INSERT OR DELETE OR UPDATE OF vault_id, key_epoch, profile_fingerprint, admission_public_key ON vault_admission_keys
FOR EACH ROW EXECUTE FUNCTION membership_version_invalidate();

INSERT INTO membership_versions(vault_id, version)
SELECT vault_id, 1 FROM vaults
ON CONFLICT (vault_id) DO NOTHING;

REVOKE ALL ON TABLE membership_versions FROM PUBLIC;
REVOKE ALL ON FUNCTION membership_version_initialize() FROM PUBLIC;
REVOKE ALL ON FUNCTION membership_version_invalidate() FROM PUBLIC;
