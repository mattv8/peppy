-- Presence is deliberately separate from durable capabilities and wake routes.
CREATE TABLE device_presence (
  vault_id UUID NOT NULL,
  device_id UUID NOT NULL,
  last_seen_at TIMESTAMPTZ NOT NULL,
  PRIMARY KEY (vault_id, device_id),
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);

CREATE TABLE device_sockets (
  connection_id UUID PRIMARY KEY,
  vault_id UUID NOT NULL,
  device_id UUID NOT NULL,
  lease_until TIMESTAMPTZ NOT NULL,
  responsive BOOLEAN NOT NULL DEFAULT FALSE,
  FOREIGN KEY (vault_id, device_id) REFERENCES devices(vault_id, device_id) ON DELETE CASCADE
);
CREATE INDEX device_sockets_active_by_device
  ON device_sockets (vault_id, device_id, lease_until);
CREATE INDEX device_sockets_responsive_by_device
  ON device_sockets (vault_id, device_id, lease_until) WHERE responsive;

-- Existing capability reports are the best historical observation available.
INSERT INTO device_presence (vault_id, device_id, last_seen_at)
  SELECT vault_id, device_id, updated_at FROM device_capabilities
  ON CONFLICT (vault_id, device_id) DO UPDATE
    SET last_seen_at = GREATEST(device_presence.last_seen_at, EXCLUDED.last_seen_at);
