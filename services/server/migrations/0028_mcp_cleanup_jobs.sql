CREATE TABLE mcp_cleanup_jobs (
  vault_id uuid NOT NULL,
  worker_device_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  attempts integer NOT NULL DEFAULT 0,
  last_attempt_at timestamptz,
  PRIMARY KEY (vault_id, worker_device_id)
);

DO $$
DECLARE schema_name text := current_schema();
BEGIN
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_enqueue_cleanup_job() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, pg_temp
    AS $body$
    BEGIN
      INSERT INTO %I.mcp_cleanup_jobs(vault_id, worker_device_id)
      VALUES (OLD.vault_id, OLD.device_id)
      ON CONFLICT (vault_id, worker_device_id) DO NOTHING;
      RETURN OLD;
    END;
    $body$
  $function$, schema_name, schema_name);
  EXECUTE format(
    'CREATE TRIGGER mcp_devices_enqueue_cleanup_before_delete BEFORE DELETE ON %I.mcp_devices FOR EACH ROW EXECUTE FUNCTION %I.mcp_enqueue_cleanup_job()',
    schema_name, schema_name
  );
  EXECUTE format($function$
    CREATE FUNCTION %I.mcp_operations_enqueue_cleanup_before_delete() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, pg_temp
    AS $body$
    BEGIN
      IF OLD.state='activated' OR OLD.cleanup_pending THEN
        INSERT INTO %I.mcp_cleanup_jobs(vault_id, worker_device_id)
        VALUES (OLD.vault_id, OLD.worker_device_id)
        ON CONFLICT (vault_id, worker_device_id) DO NOTHING;
      END IF;
      RETURN OLD;
    END;
    $body$
  $function$, schema_name, schema_name);
  EXECUTE format(
    'CREATE TRIGGER mcp_operations_enqueue_cleanup_before_delete BEFORE DELETE ON %I.mcp_operations FOR EACH ROW EXECUTE FUNCTION %I.mcp_operations_enqueue_cleanup_before_delete()',
    schema_name, schema_name
  );
  EXECUTE format('REVOKE ALL ON TABLE %I.mcp_cleanup_jobs FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_enqueue_cleanup_job() FROM PUBLIC', schema_name);
  EXECUTE format('REVOKE ALL ON FUNCTION %I.mcp_operations_enqueue_cleanup_before_delete() FROM PUBLIC', schema_name);
END;
$$;
