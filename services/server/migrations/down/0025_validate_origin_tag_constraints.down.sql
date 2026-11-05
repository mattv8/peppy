ALTER TABLE encrypted_records DROP CONSTRAINT encrypted_records_origin_tag_length_check;
ALTER TABLE encrypted_records
  ADD CONSTRAINT encrypted_records_origin_tag_length_check
  CHECK (origin_tag IS NULL OR octet_length(origin_tag) = 32) NOT VALID;
ALTER TABLE compacted_records DROP CONSTRAINT compacted_records_origin_tag_length_check;
ALTER TABLE compacted_records
  ADD CONSTRAINT compacted_records_origin_tag_length_check
  CHECK (origin_tag IS NULL OR octet_length(origin_tag) = 32) NOT VALID;
