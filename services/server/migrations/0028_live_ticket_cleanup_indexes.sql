CREATE INDEX live_tickets_expiry_reap_idx ON live_tickets (expires_at);
CREATE INDEX live_tickets_consumed_reap_idx
  ON live_tickets (consumed_at)
  WHERE consumed_at IS NOT NULL;
CREATE INDEX live_ticket_sessions_tombstone_reap_idx
  ON live_ticket_sessions (closed_at)
  WHERE state IN ('closed', 'terminal');
