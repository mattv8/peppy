DROP TABLE phone_invitations;
DROP TABLE device_admissions;
DROP TABLE vault_admission_keys;
DROP TABLE instance_sessions;
DROP TABLE instance_admin;
ALTER TABLE devices DROP COLUMN kind;
ALTER TABLE devices DROP COLUMN name;
