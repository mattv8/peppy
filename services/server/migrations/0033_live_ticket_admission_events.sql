CREATE TABLE live_ticket_admission_events (
  event_id UUID PRIMARY KEY,
  vault_id UUID NOT NULL,
  device_id UUID NOT NULL,
  event_kind TEXT NOT NULL CHECK (event_kind IN ('ticket_issue', 'session_create')),
  created_at TIMESTAMPTZ NOT NULL,
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);

CREATE INDEX live_ticket_admission_events_device_kind_created_idx
  ON live_ticket_admission_events (vault_id, device_id, event_kind, created_at);
CREATE INDEX live_ticket_admission_events_created_at_idx
  ON live_ticket_admission_events (created_at);

ALTER TABLE live_ticket_admission_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY community_live_ticket_admission_events_scope ON live_ticket_admission_events
  USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))
  WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''));
