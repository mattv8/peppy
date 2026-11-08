DO $$
DECLARE
  schema_name text := current_schema();
  has_pending_jobs boolean;
BEGIN
  -- Cleanup jobs are durable work; refuse a rollback that would discard them.
  EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I.mcp_cleanup_jobs)', schema_name)
    INTO has_pending_jobs;
  IF has_pending_jobs THEN
    RAISE EXCEPTION 'cannot roll back mcp cleanup jobs while pending jobs exist';
  END IF;

  EXECUTE format('DROP TRIGGER mcp_operations_enqueue_cleanup_before_delete ON %I.mcp_operations', schema_name);
  EXECUTE format('DROP TRIGGER mcp_devices_enqueue_cleanup_before_delete ON %I.mcp_devices', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_operations_enqueue_cleanup_before_delete()', schema_name);
  EXECUTE format('DROP FUNCTION %I.mcp_enqueue_cleanup_job()', schema_name);
  EXECUTE format('DROP TABLE %I.mcp_cleanup_jobs', schema_name);
END $$;
