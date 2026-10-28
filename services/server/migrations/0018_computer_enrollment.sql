CREATE TABLE computer_enrollments (
  id UUID PRIMARY KEY,
  vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE,
  owner_device_id UUID NOT NULL,
  origin TEXT NOT NULL,
  claim_token_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(claim_token_digest) = 32),
  server_nonce BYTEA NOT NULL CHECK (octet_length(server_nonce) = 32),
  recipient_device_id UUID NOT NULL,
  recipient_kind TEXT NOT NULL CHECK (recipient_kind IN ('browser', 'desktop')),
  recipient_name TEXT NOT NULL CHECK (octet_length(recipient_name) BETWEEN 1 AND 128),
  recipient_signing_public_key BYTEA NOT NULL CHECK (octet_length(recipient_signing_public_key) = 32),
  recipient_transfer_public_key BYTEA NOT NULL CHECK (octet_length(recipient_transfer_public_key) = 32),
  recipient_credential_digest BYTEA NOT NULL CHECK (octet_length(recipient_credential_digest) = 32),
  recipient_commitment BYTEA NOT NULL CHECK (octet_length(recipient_commitment) = 32),
  profile_fingerprint CHAR(64) NOT NULL,
  key_epoch INTEGER NOT NULL CHECK (key_epoch > 0),
  state TEXT NOT NULL CHECK (state IN ('waiting', 'approved', 'consumed', 'cancelled', 'expired')),
  owner_transfer_public_key BYTEA CHECK (owner_transfer_public_key IS NULL OR octet_length(owner_transfer_public_key) = 32),
  encrypted_key_package BYTEA,
  transfer_nonce BYTEA CHECK (transfer_nonce IS NULL OR octet_length(transfer_nonce) = 24),
  owner_proof BYTEA CHECK (owner_proof IS NULL OR octet_length(owner_proof) = 32),
  expires_at TIMESTAMPTZ NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  FOREIGN KEY (vault_id, owner_device_id) REFERENCES devices(vault_id, device_id)
);
CREATE INDEX computer_enrollments_owner_active_idx ON computer_enrollments (vault_id, owner_device_id, expires_at)
  WHERE state IN ('waiting', 'approved');
