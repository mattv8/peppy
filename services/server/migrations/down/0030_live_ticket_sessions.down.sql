DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format('DROP FUNCTION %I.community_resolve_live_ticket_vault(bytea)', schema_name);
END $$;
DROP TABLE live_tickets;
DROP TABLE live_ticket_sessions;
DROP TABLE live_ticket_device_authority;
