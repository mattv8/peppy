DROP TABLE public_attachment_copies;
DROP INDEX upload_reservations_expiry_cleanup;
ALTER TABLE upload_reservations DROP COLUMN deleting_at;
