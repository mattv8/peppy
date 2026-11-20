ALTER TABLE mcp_grants ADD COLUMN fence_pending boolean NOT NULL DEFAULT false,
  ADD COLUMN fence_attempts integer NOT NULL DEFAULT 0, ADD COLUMN fence_next_at timestamptz;
CREATE INDEX mcp_grants_fence_pending_idx ON mcp_grants (fence_next_at) WHERE fence_pending;
