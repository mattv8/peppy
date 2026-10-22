-- Lossy: non-admin vault owners retain their vaults but lose web logins.
CREATE TABLE instance_admin (
  singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
  vault_id UUID NOT NULL UNIQUE REFERENCES vaults(vault_id) ON DELETE CASCADE,
  operation_id UUID NOT NULL UNIQUE,
  request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
  password_hash TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO instance_admin (singleton, vault_id, operation_id, request_digest, password_hash, created_at)
  SELECT TRUE, vault_id, operation_id, request_digest, password_hash, created_at FROM vault_owners WHERE is_admin;
DROP TABLE vault_requests;
DROP TABLE vault_invites;
DELETE FROM instance_sessions;
ALTER TABLE instance_sessions DROP COLUMN vault_id;
DROP TABLE vault_owners;
