-- MC-2: reclaim OAuth client registrations that have no authorization, code, or token rows
-- after 1 hour instead of 24 hours. Only the client age threshold changes. The rest of
-- mcp_sweep_oauth() is restored exactly as 0028 defines it.
DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($function$
    CREATE OR REPLACE FUNCTION %I.mcp_sweep_oauth()
    RETURNS void LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
    DECLARE
      sweep_now timestamptz := clock_timestamp();
      code_cutoff timestamptz := sweep_now - interval '1 day';
      token_cutoff timestamptz := sweep_now - interval '7 days';
      candidate record;
    BEGIN
      DELETE FROM %I.mcp_oauth_authorizations WHERE id IN (
        SELECT id FROM %I.mcp_oauth_authorizations WHERE expires_at <= sweep_now ORDER BY expires_at LIMIT 512
      );
      DELETE FROM %I.mcp_oauth_codes WHERE digest IN (
        SELECT digest FROM %I.mcp_oauth_codes
        WHERE expires_at <= code_cutoff
        ORDER BY expires_at LIMIT 512
      );
      DELETE FROM %I.mcp_oauth_tokens WHERE digest IN (
        SELECT digest FROM %I.mcp_oauth_tokens
        WHERE expires_at <= token_cutoff
        ORDER BY expires_at LIMIT 512
      );
      FOR candidate IN
        SELECT c.client_id FROM %I.mcp_oauth_clients c
        WHERE c.created_at < sweep_now - interval '1 hour'
          AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_authorizations a WHERE a.client_id=c.client_id)
          AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_codes code WHERE code.client_id=c.client_id)
          AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_tokens token WHERE token.client_id=c.client_id)
        ORDER BY c.created_at LIMIT 512 FOR UPDATE SKIP LOCKED
      LOOP
        IF NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_authorizations a WHERE a.client_id=candidate.client_id)
          AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_codes code WHERE code.client_id=candidate.client_id)
          AND NOT EXISTS (SELECT 1 FROM %I.mcp_oauth_tokens token WHERE token.client_id=candidate.client_id)
        THEN
          DELETE FROM %I.mcp_oauth_clients WHERE client_id=candidate.client_id;
        END IF;
      END LOOP;
    END $body$ $function$, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name, schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_sweep_oauth() FROM PUBLIC', schema_name);
END $$;
