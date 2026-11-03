CREATE TABLE relay_admissions (
    admission_kind TEXT NOT NULL CHECK (admission_kind IN ('registration', 'wake')),
    source TEXT NOT NULL,
    window_started_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    PRIMARY KEY (admission_kind, source, window_started_at)
);

CREATE INDEX relay_admissions_expiry_idx ON relay_admissions (window_started_at);
