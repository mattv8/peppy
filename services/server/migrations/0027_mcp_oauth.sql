-- OAuth registration and pending consent are global: no vault exists until an
-- owner approves a request. Issued credentials are tenant records.
CREATE TABLE mcp_oauth_clients (
  client_id text PRIMARY KEY CHECK (char_length(client_id) BETWEEN 16 AND 128),
  client_name text NOT NULL CHECK (char_length(client_name) BETWEEN 1 AND 128),
  redirect_uris text[] NOT NULL CHECK (cardinality(redirect_uris) BETWEEN 1 AND 8),
  allowed_scopes text[] NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE mcp_oauth_authorizations (
  id text PRIMARY KEY CHECK (char_length(id) BETWEEN 16 AND 128),
  client_id text NOT NULL REFERENCES mcp_oauth_clients(client_id) ON DELETE CASCADE,
  redirect_uri text NOT NULL,
  state text NOT NULL CHECK (char_length(state) <= 2048),
  code_challenge text NOT NULL CHECK (char_length(code_challenge) BETWEEN 43 AND 128),
  requested_scopes text[] NOT NULL CHECK (cardinality(requested_scopes) BETWEEN 1 AND 3),
  resource text NOT NULL,
  expires_at timestamptz NOT NULL,
  decided_at timestamptz,
  denied_at timestamptz,
  vault_id uuid,
  device_id uuid,
  UNIQUE (id, vault_id)
);
CREATE INDEX mcp_oauth_authorizations_pending_idx ON mcp_oauth_authorizations(expires_at) WHERE decided_at IS NULL AND denied_at IS NULL;

CREATE TABLE mcp_oauth_codes (
  digest bytea PRIMARY KEY CHECK (octet_length(digest) = 32),
  vault_id uuid NOT NULL,
  device_id uuid NOT NULL,
  grant_id uuid NOT NULL REFERENCES mcp_grants(id) ON DELETE CASCADE,
  client_id text NOT NULL REFERENCES mcp_oauth_clients(client_id) ON DELETE CASCADE,
  redirect_uri text NOT NULL,
  code_challenge text NOT NULL,
  resource text NOT NULL,
  expires_at timestamptz NOT NULL,
  used_at timestamptz,
  FOREIGN KEY (vault_id, device_id) REFERENCES mcp_devices(vault_id, device_id) ON DELETE CASCADE
);

CREATE TABLE mcp_oauth_tokens (
  digest bytea PRIMARY KEY CHECK (octet_length(digest) = 32),
  token_kind text NOT NULL CHECK (token_kind IN ('access', 'refresh')),
  family_id uuid NOT NULL,
  vault_id uuid NOT NULL,
  device_id uuid NOT NULL,
  grant_id uuid NOT NULL REFERENCES mcp_grants(id) ON DELETE CASCADE,
  client_id text NOT NULL REFERENCES mcp_oauth_clients(client_id) ON DELETE CASCADE,
  resource text NOT NULL,
  expires_at timestamptz NOT NULL,
  used_at timestamptz,
  revoked_at timestamptz,
  FOREIGN KEY (vault_id, device_id) REFERENCES mcp_devices(vault_id, device_id) ON DELETE CASCADE
);
CREATE INDEX mcp_oauth_tokens_live_idx ON mcp_oauth_tokens(digest) WHERE revoked_at IS NULL;

ALTER TABLE mcp_oauth_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE mcp_oauth_tokens ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS mcp_devices_scope ON mcp_devices;
DROP POLICY IF EXISTS mcp_grants_scope ON mcp_grants;
DROP POLICY IF EXISTS mcp_operations_scope ON mcp_operations;
CREATE POLICY community_mcp_devices_scope ON mcp_devices
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY community_mcp_grants_scope ON mcp_grants
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY community_mcp_operations_scope ON mcp_operations
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY community_mcp_oauth_codes_scope ON mcp_oauth_codes
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY community_mcp_oauth_tokens_scope ON mcp_oauth_tokens
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_resolve_access_token(bytea)
    RETURNS TABLE(vault_id uuid, device_id uuid, grant_id uuid, scopes text[], token_digest bytea, family_id uuid, resource text, device_role text)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT t.vault_id, t.device_id, t.grant_id, g.scopes, t.digest, t.family_id, t.resource, d.role
      FROM %I.mcp_oauth_tokens t
      JOIN %I.mcp_grants g ON g.id=t.grant_id AND g.vault_id=t.vault_id AND g.device_id=t.device_id
      JOIN %I.mcp_devices m ON m.vault_id=t.vault_id AND m.device_id=t.device_id
      JOIN %I.devices d ON d.vault_id=t.vault_id AND d.device_id=t.device_id
      WHERE octet_length($1)=32 AND t.digest=$1 AND t.token_kind='access'
        AND t.expires_at > now() AND t.revoked_at IS NULL AND g.revoked_at IS NULL
        AND d.revoked_at IS NULL
    $body$ $function$, schema_name, schema_name, schema_name, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_resolve_code(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT c.vault_id FROM %I.mcp_oauth_codes c
      WHERE octet_length($1)=32 AND c.digest=$1 AND c.expires_at > now() AND c.used_at IS NULL
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_resolve_refresh_token(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT t.vault_id FROM %I.mcp_oauth_tokens t
      WHERE octet_length($1)=32 AND t.digest=$1 AND t.token_kind='refresh'
    $body$ $function$, schema_name, schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_sweep_oauth()
    RETURNS void LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
    BEGIN
      DELETE FROM %I.mcp_oauth_authorizations WHERE id IN (
        SELECT id FROM %I.mcp_oauth_authorizations WHERE expires_at <= now() ORDER BY expires_at LIMIT 256
      );
      DELETE FROM %I.mcp_oauth_clients c
      WHERE c.created_at < now() - interval '24 hours'
        AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_authorizations a WHERE a.client_id=c.client_id AND a.expires_at > now())
        AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_tokens t WHERE t.client_id=c.client_id AND t.expires_at > now() AND t.revoked_at IS NULL);
    END $body$ $function$, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_resolve_access_token(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_resolve_code(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_resolve_refresh_token(bytea) FROM PUBLIC', schema_name);
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_resolve_token(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT t.vault_id FROM %I.mcp_oauth_tokens t
      WHERE octet_length($1)=32 AND t.digest=$1
    $body$ $function$, schema_name, schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_resolve_token(bytea) FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_sweep_oauth() FROM PUBLIC', schema_name);
END $$;

REVOKE ALL ON TABLE mcp_oauth_clients, mcp_oauth_authorizations, mcp_oauth_codes, mcp_oauth_tokens FROM PUBLIC;
