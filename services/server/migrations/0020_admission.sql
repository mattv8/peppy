-- Distributed fixed 60-second admission counters.  The API takes a
-- transaction-scoped advisory lock before updating them, so the existing
-- 10/source and 100/global thresholds apply across replicas.
CREATE TABLE instance_admission_windows (
  window_started TIMESTAMPTZ PRIMARY KEY,
  count INTEGER NOT NULL CHECK (count >= 0)
);

CREATE TABLE instance_admission_sources (
  window_started TIMESTAMPTZ NOT NULL REFERENCES instance_admission_windows(window_started) ON DELETE CASCADE,
  operation TEXT NOT NULL CHECK (operation IN ('claim', 'claim-replay', 'login', 'create-vault', 'vault-request', 'phone-claim', 'computer-status', 'computer-confirm', 'manual-challenge', 'manual-proof')),
  source_digest BYTEA NOT NULL CHECK (octet_length(source_digest) = 32),
  count INTEGER NOT NULL CHECK (count >= 0),
  PRIMARY KEY (window_started, operation, source_digest)
);

CREATE INDEX instance_admission_sources_window_idx ON instance_admission_sources(window_started);
