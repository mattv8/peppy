CREATE TABLE mcp_devices (
  vault_id uuid NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  device_id uuid NOT NULL,
  cleanup_pending boolean NOT NULL DEFAULT false,
  last_synced_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (vault_id, device_id),
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);

CREATE TABLE mcp_grants (
  id uuid PRIMARY KEY,
  vault_id uuid NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  device_id uuid NOT NULL,
  client_name text NOT NULL CHECK (char_length(client_name) BETWEEN 1 AND 128),
  scopes text[] NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  last_used_at timestamptz,
  revoked_at timestamptz,
  FOREIGN KEY (vault_id, device_id) REFERENCES mcp_devices(vault_id, device_id) ON DELETE CASCADE
);
CREATE INDEX mcp_grants_live_vault_idx ON mcp_grants(vault_id, created_at) WHERE revoked_at IS NULL;

CREATE TABLE mcp_operations (
  id uuid PRIMARY KEY,
  vault_id uuid NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  owner_device_id uuid NOT NULL,
  worker_device_id uuid NOT NULL,
  state text NOT NULL CHECK (state IN ('prepared', 'activated', 'completed', 'revoked')),
  cleanup_pending boolean NOT NULL DEFAULT false,
  activation_fingerprint bytea CHECK (activation_fingerprint IS NULL OR octet_length(activation_fingerprint) = 32),
  completion_fingerprint bytea CHECK (completion_fingerprint IS NULL OR octet_length(completion_fingerprint) = 32),
  created_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL DEFAULT (now() + interval '300 seconds'),
  FOREIGN KEY (vault_id, owner_device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX mcp_operations_live_owner_idx ON mcp_operations(vault_id, owner_device_id) WHERE state IN ('prepared', 'activated');

ALTER TABLE mcp_devices ENABLE ROW LEVEL SECURITY;
ALTER TABLE mcp_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE mcp_operations ENABLE ROW LEVEL SECURITY;
CREATE POLICY mcp_devices_scope ON mcp_devices
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY mcp_grants_scope ON mcp_grants
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
CREATE POLICY mcp_operations_scope ON mcp_operations
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
REVOKE ALL ON TABLE mcp_devices, mcp_grants, mcp_operations FROM PUBLIC;
