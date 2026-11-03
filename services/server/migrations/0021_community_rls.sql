-- Community RLS is intentionally additive to hosted RLS.  Policy names and
-- resolver names never overlap the private hosted migration ledger.
ALTER TABLE vaults ENABLE ROW LEVEL SECURITY;
ALTER TABLE devices ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_credentials ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE encrypted_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE commands ENABLE ROW LEVEL SECURITY;
ALTER TABLE command_receipts ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_cursors ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbox_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_capabilities ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_presence ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_sockets ENABLE ROW LEVEL SECURITY;
ALTER TABLE upload_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachments ENABLE ROW LEVEL SECURITY;
ALTER TABLE public_attachment_copies ENABLE ROW LEVEL SECURITY;
ALTER TABLE vault_key_profiles ENABLE ROW LEVEL SECURITY;
ALTER TABLE compacted_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachment_record_references ENABLE ROW LEVEL SECURITY;
ALTER TABLE record_compaction ENABLE ROW LEVEL SECURITY;
ALTER TABLE record_supersessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE storage_deletions ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_wake_routes ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_wake_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE vault_admission_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_admissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE phone_invitations ENABLE ROW LEVEL SECURITY;
ALTER TABLE computer_enrollments ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachment_retention_uses ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachment_retention_devices ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachment_removals ENABLE ROW LEVEL SECURITY;

DO $$
DECLARE table_name text;
DECLARE schema_name text := current_schema();
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'vaults', 'devices', 'device_credentials', 'event_log', 'encrypted_records', 'commands',
    'command_receipts', 'device_cursors', 'outbox_jobs', 'device_capabilities', 'device_presence',
    'device_sockets', 'upload_reservations', 'attachments', 'public_attachment_copies',
    'vault_key_profiles', 'compacted_records', 'attachment_record_references', 'record_compaction',
    'record_supersessions', 'storage_deletions', 'device_wake_routes', 'device_wake_jobs',
    'vault_admission_keys', 'device_admissions', 'phone_invitations', 'computer_enrollments',
    'attachment_retention_uses', 'attachment_retention_devices', 'attachment_removals'
  ] LOOP
    EXECUTE format(
      'CREATE POLICY %I ON %I.%I USING (vault_id::text = nullif(current_setting(''peppy.vault_id'', true), '''')) WITH CHECK (vault_id::text = nullif(current_setting(''peppy.vault_id'', true), ''''))',
      'community_' || table_name || '_scope', schema_name, table_name
    );
  END LOOP;
END $$;

DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_device_credential(bytea)
    RETURNS TABLE(vault_id uuid, device_id uuid, role text, token_digest bytea)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT c.vault_id, c.device_id, d.role, c.token_digest
      FROM %I.device_credentials AS c
      JOIN %I.devices AS d ON d.vault_id = c.vault_id AND d.device_id = c.device_id
      WHERE octet_length($1) = 32 AND c.token_digest = $1
        AND c.revoked_at IS NULL AND d.revoked_at IS NULL
    $body$ $function$, schema_name, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_public_copy_vault(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT p.vault_id FROM %I.public_attachment_copies AS p
      WHERE octet_length($1) = 32 AND p.token_digest = $1
        AND p.ready_at IS NOT NULL AND p.revoked_at IS NULL AND p.retired_at IS NULL
        AND p.expires_at > now()
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_phone_invitation(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT p.vault_id FROM %I.phone_invitations AS p
      WHERE octet_length($1) = 32 AND p.token_digest = $1
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_computer_enrollment(uuid, bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT c.vault_id FROM %I.computer_enrollments AS c
      WHERE c.id = $1 AND octet_length($2) = 32 AND c.claim_token_digest = $2
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_phone_invitation_claim(uuid, bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT p.vault_id FROM %I.phone_invitations AS p
      WHERE p.id = $1 AND octet_length($2) = 32 AND p.claim_secret_digest = $2
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_device_admission(uuid)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT a.vault_id FROM %I.device_admissions AS a WHERE a.id = $1
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.community_admin_vault_summaries()
    RETURNS TABLE(vault_id uuid, username text, is_admin boolean, created_at text, device_count bigint, storage_bytes bigint)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT owners.vault_id, owners.username, owners.is_admin,
        to_char(owners.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        (SELECT count(*) FROM %I.devices WHERE vault_id = owners.vault_id)::bigint,
        (SELECT COALESCE(sum(ciphertext_bytes), 0)::bigint FROM %I.attachments WHERE vault_id = owners.vault_id)
      FROM %I.vault_owners AS owners ORDER BY owners.created_at
    $body$ $function$, schema_name, schema_name, schema_name, schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_device_credential(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_public_copy_vault(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_phone_invitation(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_computer_enrollment(uuid, bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_phone_invitation_claim(uuid, bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_device_admission(uuid) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_admin_vault_summaries() FROM PUBLIC', schema_name);
END $$;
REVOKE ALL ON TABLE vaults, devices, device_credentials, event_log, encrypted_records, commands,
  command_receipts, device_cursors, outbox_jobs, device_capabilities, device_presence, device_sockets,
  upload_reservations, attachments, public_attachment_copies, vault_key_profiles, compacted_records,
  attachment_record_references, record_compaction, record_supersessions, storage_deletions,
  device_wake_routes, device_wake_jobs, vault_admission_keys, device_admissions, phone_invitations,
  computer_enrollments, attachment_retention_uses, attachment_retention_devices, attachment_removals,
  vault_owners, instance_sessions, vault_invites, vault_requests FROM PUBLIC;
