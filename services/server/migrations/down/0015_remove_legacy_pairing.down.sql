CREATE TABLE pairing_challenges (
  challenge_digest BYTEA PRIMARY KEY,
  vault_id UUID NOT NULL REFERENCES vaults(vault_id),
  requested_device_id UUID NOT NULL,
  requested_public_key JSONB NOT NULL,
  approved_by_device_id UUID NOT NULL,
  profile_fingerprint CHAR(64) NOT NULL,
  key_epoch INTEGER NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ,
  requested_role TEXT NOT NULL DEFAULT 'device'
    CHECK (requested_role IN ('device', 'gateway'))
);

CREATE TABLE pairing_intents (
  intent_digest BYTEA PRIMARY KEY CHECK (octet_length(intent_digest) = 32),
  vault_id UUID NOT NULL REFERENCES vaults(vault_id),
  origin TEXT NOT NULL,
  created_by_device_id UUID NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  claimed_at TIMESTAMPTZ,
  claimed_device_id UUID,
  claimed_public_key JSONB,
  claimed_key_digest CHAR(64),
  claim_secret_digest BYTEA CHECK (claim_secret_digest IS NULL OR octet_length(claim_secret_digest) = 32),
  challenge_token TEXT,
  challenge_digest BYTEA CHECK (challenge_digest IS NULL OR octet_length(challenge_digest) = 32),
  requested_role TEXT CHECK (requested_role IN ('device', 'gateway')),
  approved_at TIMESTAMPTZ
);
CREATE INDEX pairing_intents_expiry ON pairing_intents(expires_at) WHERE approved_at IS NULL;

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
