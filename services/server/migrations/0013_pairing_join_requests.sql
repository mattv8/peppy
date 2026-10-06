CREATE TABLE pairing_join_requests (
  join_request_id UUID PRIMARY KEY,
  poll_secret_digest BYTEA NOT NULL CHECK (octet_length(poll_secret_digest) = 32),
  vault_id UUID REFERENCES vaults(vault_id) ON DELETE CASCADE,
  offered_by_device_id UUID,
  sealed_intent_token BYTEA CHECK (sealed_intent_token IS NULL OR octet_length(sealed_intent_token) <= 256),
  intent_digest BYTEA CHECK (intent_digest IS NULL OR octet_length(intent_digest) = 32),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  offered_at TIMESTAMPTZ
);
CREATE INDEX pairing_join_requests_expires_idx ON pairing_join_requests (expires_at);
