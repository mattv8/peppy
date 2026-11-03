DO $$
DECLARE table_name text;
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format('DROP FUNCTION %I.community_admin_vault_summaries()', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_device_admission(uuid)', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_phone_invitation_claim(uuid, bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_computer_enrollment(uuid, bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_phone_invitation(bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_public_copy_vault(bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.community_resolve_device_credential(bytea)', schema_name);
  FOREACH table_name IN ARRAY ARRAY[
    'vaults', 'devices', 'device_credentials', 'event_log', 'encrypted_records', 'commands',
    'command_receipts', 'device_cursors', 'outbox_jobs', 'device_capabilities', 'device_presence',
    'device_sockets', 'upload_reservations', 'attachments', 'public_attachment_copies',
    'vault_key_profiles', 'compacted_records', 'attachment_record_references', 'record_compaction',
    'record_supersessions', 'storage_deletions', 'device_wake_routes', 'device_wake_jobs',
    'vault_admission_keys', 'device_admissions', 'phone_invitations', 'computer_enrollments',
    'attachment_retention_uses', 'attachment_retention_devices', 'attachment_removals'
  ] LOOP
    EXECUTE format('DROP POLICY %I ON %I.%I', 'community_' || table_name || '_scope', schema_name, table_name);
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname = schema_name AND tablename = table_name) THEN
      EXECUTE format('ALTER TABLE %I.%I DISABLE ROW LEVEL SECURITY', schema_name, table_name);
    END IF;
  END LOOP;
END $$;
