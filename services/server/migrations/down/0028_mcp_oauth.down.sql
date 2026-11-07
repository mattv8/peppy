DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format('DROP FUNCTION %I.mcp_sweep_oauth()', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_resolve_token(bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_resolve_refresh_token(bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_resolve_code(bytea)', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_resolve_access_token(bytea)', schema_name);
END $$;

DROP POLICY community_mcp_operations_scope ON mcp_operations;
DROP POLICY community_mcp_grants_scope ON mcp_grants;
DROP POLICY community_mcp_devices_scope ON mcp_devices;
CREATE POLICY mcp_devices_scope ON mcp_devices
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY mcp_grants_scope ON mcp_grants
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY mcp_operations_scope ON mcp_operations
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

DROP TABLE mcp_oauth_tokens;
DROP TABLE mcp_oauth_codes;
DROP TABLE mcp_oauth_authorizations;
DROP TABLE mcp_oauth_clients;
