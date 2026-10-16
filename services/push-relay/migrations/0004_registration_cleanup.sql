CREATE INDEX relay_registrations_unconfirmed_expiry_idx
    ON relay_registrations (expires_at)
    WHERE confirmed_at IS NULL;
