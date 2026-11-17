ALTER TABLE device_cursors
  ADD COLUMN reported_at TIMESTAMPTZ NOT NULL DEFAULT now();

INSERT INTO device_cursors(vault_id,device_id,receive_cursor,reported_at)
SELECT d.vault_id,d.device_id,0,now()
FROM devices AS d
LEFT JOIN device_cursors AS c ON c.vault_id=d.vault_id AND c.device_id=d.device_id
WHERE d.revoked_at IS NULL AND c.device_id IS NULL;
