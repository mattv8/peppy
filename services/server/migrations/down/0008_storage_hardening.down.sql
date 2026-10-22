-- Deleted fenced reservations and queued deletion history cannot be restored.
DROP INDEX event_log_created_at;
DROP INDEX public_attachment_copies_quota;
DROP INDEX public_attachment_copies_expiry;
ALTER TABLE public_attachment_copies
  DROP COLUMN purged_at,
  DROP COLUMN retired_at,
  DROP COLUMN ready_at;
DROP INDEX upload_reservations_expired;
DROP TABLE storage_deletions;
