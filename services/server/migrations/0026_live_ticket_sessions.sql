CREATE TABLE live_ticket_sessions (
  vault_id UUID NOT NULL,
  device_id UUID NOT NULL,
  session_id_hash BYTEA NOT NULL CHECK (octet_length(session_id_hash) = 32),
  state TEXT NOT NULL CHECK (state IN ('open', 'closed', 'terminal')),
  lease_until TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  closed_at TIMESTAMPTZ,
  PRIMARY KEY (vault_id, device_id, session_id_hash),
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE,
  CHECK ((state = 'open' AND closed_at IS NULL) OR (state IN ('closed', 'terminal') AND closed_at IS NOT NULL))
);

CREATE TABLE live_tickets (
  ticket_digest BYTEA PRIMARY KEY CHECK (octet_length(ticket_digest) = 32),
  vault_id UUID NOT NULL,
  device_id UUID NOT NULL,
  session_id_hash BYTEA NOT NULL CHECK (octet_length(session_id_hash) = 32),
  issued_at TIMESTAMPTZ NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ,
  FOREIGN KEY (vault_id, device_id, session_id_hash)
    REFERENCES live_ticket_sessions(vault_id, device_id, session_id_hash) ON DELETE CASCADE,
  CHECK (expires_at > issued_at)
);

CREATE INDEX live_tickets_session_idx
  ON live_tickets (vault_id, device_id, session_id_hash);
CREATE INDEX live_tickets_device_issued_idx
  ON live_tickets (vault_id, device_id, issued_at);
CREATE INDEX live_ticket_sessions_state_lease_idx
  ON live_ticket_sessions (state, lease_until);
CREATE INDEX live_ticket_sessions_tombstone_idx
  ON live_ticket_sessions (vault_id, device_id, state, closed_at)
  WHERE state IN ('closed', 'terminal');

ALTER TABLE live_ticket_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE live_tickets ENABLE ROW LEVEL SECURITY;

CREATE POLICY community_live_ticket_sessions_scope ON live_ticket_sessions
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY community_live_tickets_scope ON live_tickets
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));

DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($function$
    CREATE FUNCTION %I.community_resolve_live_ticket_vault(bytea)
    RETURNS TABLE(vault_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER
    SET search_path TO pg_catalog, public, pg_temp
    AS $body$
      SELECT ticket.vault_id
      FROM %I.live_tickets AS ticket
      WHERE octet_length($1) = 32 AND ticket.ticket_digest = $1
    $body$ $function$, schema_name, schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.community_resolve_live_ticket_vault(bytea) FROM PUBLIC', schema_name);
END $$;
