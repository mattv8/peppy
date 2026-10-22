-- Generated wake-job identifiers and lease state cannot be restored.
DROP INDEX device_wake_jobs_lease_due;
DROP INDEX device_wake_jobs_job_id;
ALTER TABLE device_wake_jobs
  DROP COLUMN last_attempt_at,
  DROP COLUMN lease_until,
  DROP COLUMN lease_token,
  DROP COLUMN opaque_nonce,
  DROP COLUMN job_id;
ALTER TABLE device_wake_routes DROP COLUMN generation;
