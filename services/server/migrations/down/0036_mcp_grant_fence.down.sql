DROP INDEX IF EXISTS mcp_grants_fence_pending_idx;
ALTER TABLE mcp_grants DROP COLUMN IF EXISTS fence_next_at,
  DROP COLUMN IF EXISTS fence_attempts, DROP COLUMN IF EXISTS fence_pending;
