ALTER TABLE vaults ADD COLUMN draft_change_seq BIGINT NOT NULL DEFAULT 0;
ALTER TABLE upload_reservations ADD COLUMN draft_id UUID;
ALTER TABLE attachments ADD COLUMN draft_id UUID;
CREATE INDEX attachments_draft_id ON attachments(draft_id) WHERE draft_id IS NOT NULL;

CREATE TABLE draft_incarnations (vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE, draft_id UUID NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY(vault_id,draft_id));
CREATE TABLE draft_branches (vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE, draft_id UUID NOT NULL, branch_id UUID NOT NULL, head_revision_id UUID NOT NULL, state TEXT NOT NULL CHECK(state IN ('live','closed')), close_reason TEXT CHECK(close_reason IN ('superseded','resolved','expired')), expires_at TIMESTAMPTZ NOT NULL, change_seq BIGINT NOT NULL, PRIMARY KEY(vault_id,branch_id));
CREATE INDEX draft_branches_change_seq ON draft_branches(vault_id,change_seq);
CREATE INDEX draft_branches_live_expiry ON draft_branches(expires_at) WHERE state='live';
CREATE TABLE draft_revisions (vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE, draft_id UUID NOT NULL, revision_id UUID NOT NULL, branch_id UUID NOT NULL, parent_revision_id UUID, producer_device_id UUID NOT NULL, sealed_record BYTEA, attachment_ids UUID[] NOT NULL, authored BOOLEAN NOT NULL, conflict BOOLEAN NOT NULL, accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at_at_accept TIMESTAMPTZ NOT NULL, PRIMARY KEY(vault_id,draft_id,revision_id));
CREATE TABLE draft_terminals (vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE, draft_id UUID NOT NULL, reason TEXT NOT NULL CHECK(reason IN ('sent','discarded','expired')), command_id UUID, terminated_at TIMESTAMPTZ NOT NULL DEFAULT now(), change_seq BIGINT NOT NULL, PRIMARY KEY(vault_id,draft_id));
CREATE INDEX draft_terminals_change_seq ON draft_terminals(vault_id,change_seq);
CREATE TABLE draft_attachment_holds (vault_id UUID NOT NULL REFERENCES vaults(vault_id) ON DELETE CASCADE, draft_id UUID NOT NULL, attachment_id UUID NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('staging','draft','send')), expires_at TIMESTAMPTZ NOT NULL, PRIMARY KEY(vault_id,draft_id,attachment_id,kind));
CREATE INDEX draft_attachment_holds_expiry ON draft_attachment_holds(expires_at);
CREATE INDEX draft_attachment_holds_attachment ON draft_attachment_holds(vault_id,attachment_id);

ALTER TABLE storage_deletions DROP CONSTRAINT storage_deletions_reason_check;
ALTER TABLE storage_deletions ADD CONSTRAINT storage_deletions_reason_check CHECK (reason IN ('upload_attempt','superseded_upload','expired_reservation','public_copy','finalized_attachment','vault_deleted','retention_cleanup','draft_expired'));

ALTER TABLE draft_incarnations ENABLE ROW LEVEL SECURITY; ALTER TABLE draft_branches ENABLE ROW LEVEL SECURITY; ALTER TABLE draft_revisions ENABLE ROW LEVEL SECURITY; ALTER TABLE draft_terminals ENABLE ROW LEVEL SECURITY; ALTER TABLE draft_attachment_holds ENABLE ROW LEVEL SECURITY;
DO $$ DECLARE table_name text; schema_name text := current_schema(); BEGIN FOREACH table_name IN ARRAY ARRAY['draft_incarnations','draft_branches','draft_revisions','draft_terminals','draft_attachment_holds'] LOOP EXECUTE format('CREATE POLICY %I ON %I.%I USING (vault_id::text = nullif(current_setting(''peppy.vault_id'', true), '''')) WITH CHECK (vault_id::text = nullif(current_setting(''peppy.vault_id'', true), ''''))','community_' || table_name || '_scope',schema_name,table_name); END LOOP; END $$;
