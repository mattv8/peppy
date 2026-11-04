-- Transactional for the same reason as 0023; holds a SHARE lock on compacted_records during the build.
CREATE UNIQUE INDEX compacted_records_origin_tag_unique
  ON compacted_records(vault_id, origin_tag) WHERE origin_tag IS NOT NULL;
