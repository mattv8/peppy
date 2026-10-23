ALTER TABLE devices
  ADD COLUMN name TEXT,
  ADD COLUMN kind TEXT CHECK (kind IS NULL OR kind IN ('browser', 'desktop', 'android', 'ios'));

CREATE TABLE instance_admin (
  singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
  vault_id UUID NOT NULL UNIQUE REFERENCES vaults(vault_id) ON DELETE CASCADE,
  operation_id UUID NOT NULL UNIQUE,
  request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
  password_hash TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE instance_sessions (
  token_digest BYTEA PRIMARY KEY CHECK (octet_length(token_digest) = 32),
  csrf_digest BYTEA CHECK (csrf_digest IS NULL OR octet_length(csrf_digest) = 32),
  expires_at TIMESTAMPTZ NOT NULL,
  revoked_at TIMESTAMPTZ
);
CREATE INDEX instance_sessions_expiry_idx ON instance_sessions (expires_at) WHERE revoked_at IS NULL;

CREATE TABLE vault_admission_keys (
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  key_epoch INTEGER NOT NULL CHECK (key_epoch > 0),
  profile_fingerprint CHAR(64) NOT NULL,
  admission_public_key BYTEA NOT NULL CHECK (octet_length(admission_public_key) = 32),
  PRIMARY KEY (vault_id, key_epoch)
);

CREATE TABLE device_admissions (
  id UUID PRIMARY KEY,
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  device_id UUID NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('browser', 'desktop', 'android', 'ios')),
  name TEXT NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 128),
  requested_role TEXT NOT NULL CHECK (requested_role IN ('owner', 'device', 'gateway')),
  device_public_key BYTEA NOT NULL CHECK (octet_length(device_public_key) = 32),
  credential_digest BYTEA NOT NULL CHECK (octet_length(credential_digest) = 32),
  nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 32),
  origin TEXT NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ,
  failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts BETWEEN 0 AND 5),
  context_digest BYTEA NOT NULL CHECK (octet_length(context_digest) = 32),
  UNIQUE (vault_id, device_id, credential_digest)
);
CREATE INDEX device_admissions_expiry_idx ON device_admissions (expires_at) WHERE consumed_at IS NULL;

CREATE TABLE phone_invitations (
  id UUID PRIMARY KEY,
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  owner_device_id UUID NOT NULL,
  origin TEXT NOT NULL,
  method TEXT NOT NULL CHECK (method IN ('transfer', 'shared_secret')),
  token_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(token_digest) = 32),
  owner_transfer_public_key BYTEA NOT NULL CHECK (octet_length(owner_transfer_public_key) = 32),
  claim_secret_digest BYTEA CHECK (claim_secret_digest IS NULL OR octet_length(claim_secret_digest) = 32),
  claim_authenticator BYTEA CHECK (claim_authenticator IS NULL OR octet_length(claim_authenticator) = 32),
  claimed_device_id UUID,
  claimed_kind TEXT CHECK (claimed_kind IS NULL OR claimed_kind IN ('browser', 'desktop', 'android', 'ios')),
  claimed_name TEXT CHECK (claimed_name IS NULL OR octet_length(claimed_name) BETWEEN 1 AND 128),
  claimed_public_key BYTEA CHECK (claimed_public_key IS NULL OR octet_length(claimed_public_key) = 32),
  claimed_transfer_public_key BYTEA CHECK (claimed_transfer_public_key IS NULL OR octet_length(claimed_transfer_public_key) = 32),
  claimed_credential_digest BYTEA CHECK (claimed_credential_digest IS NULL OR octet_length(claimed_credential_digest) = 32),
  profile_fingerprint CHAR(64) NOT NULL,
  key_epoch INTEGER NOT NULL CHECK (key_epoch > 0),
  state TEXT NOT NULL CHECK (state IN ('waiting', 'claimed', 'approved', 'confirmed', 'consumed', 'expired', 'denied')),
  expires_at TIMESTAMPTZ NOT NULL,
  encrypted_key_package BYTEA,
  transfer_nonce BYTEA CHECK (transfer_nonce IS NULL OR octet_length(transfer_nonce) = 24),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  FOREIGN KEY (vault_id, owner_device_id) REFERENCES devices(vault_id, device_id)
);
CREATE INDEX phone_invitations_owner_active_idx ON phone_invitations (vault_id, owner_device_id, expires_at)
  WHERE state IN ('waiting', 'claimed', 'approved', 'confirmed');
