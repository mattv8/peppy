CREATE TABLE vault_owners (
  vault_id UUID PRIMARY KEY REFERENCES vaults(vault_id) ON DELETE CASCADE,
  username TEXT NOT NULL UNIQUE CHECK (username ~ '^[a-z0-9][a-z0-9._-]{0,31}$'),
  password_hash TEXT NOT NULL,
  is_admin BOOLEAN NOT NULL DEFAULT FALSE,
  operation_id UUID NOT NULL UNIQUE,
  request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX vault_owners_single_admin_idx ON vault_owners (is_admin) WHERE is_admin;
INSERT INTO vault_owners (vault_id, username, password_hash, is_admin, operation_id, request_digest, created_at)
  SELECT vault_id, 'admin', password_hash, TRUE, operation_id, request_digest, created_at FROM instance_admin;
DROP TABLE instance_admin;
DELETE FROM instance_sessions;
ALTER TABLE instance_sessions ADD COLUMN vault_id UUID NOT NULL REFERENCES vault_owners(vault_id) ON DELETE CASCADE;
CREATE TABLE vault_invites (
  id UUID PRIMARY KEY, token_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(token_digest) = 32),
  created_by UUID NOT NULL REFERENCES vault_owners(vault_id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK (state IN ('active', 'used', 'revoked')), expires_at TIMESTAMPTZ NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX vault_invites_active_idx ON vault_invites (expires_at) WHERE state = 'active';
CREATE TABLE vault_requests (
  id UUID PRIMARY KEY, invite_id UUID NOT NULL UNIQUE REFERENCES vault_invites(id) ON DELETE CASCADE,
  display_name TEXT NOT NULL CHECK (octet_length(display_name) BETWEEN 1 AND 64),
  client_kind TEXT NOT NULL CHECK (client_kind IN ('browser', 'desktop', 'android', 'ios')),
  secret_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(secret_digest) = 32),
  state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'denied', 'completed')),
  expires_at TIMESTAMPTZ NOT NULL, decided_at TIMESTAMPTZ,
  vault_id UUID REFERENCES vaults(vault_id) ON DELETE SET NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX vault_requests_open_idx ON vault_requests (expires_at) WHERE state IN ('pending', 'approved');
