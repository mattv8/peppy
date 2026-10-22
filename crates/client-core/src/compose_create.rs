use crate::{
    Client, ComposeDraft, ComposeDraftUpdate, ConversationId, DraftId, Error,
    conversation_for_compose_recipients, is_draft_only, json, load_compose_draft, params,
    require_address, require_body, require_subscription, to_i64,
};
use rusqlite::TransactionBehavior;

impl Client {
    /// Atomically opens or saves an initial compose draft without exposing a temporary blank row.
    pub fn save_new_compose_draft(
        &self,
        update: ComposeDraftUpdate,
    ) -> Result<ComposeDraft, Error> {
        require_body(&update.text, true)?;
        if update.recipients.len() > crate::MAX_DRAFT_RECIPIENTS {
            return Err(Error::InvalidRequest("draft allows at most 20 recipients"));
        }
        if update.attachment_ids.len() > crate::MAX_DRAFT_ATTACHMENTS {
            return Err(Error::InvalidRequest("draft allows at most 10 attachments"));
        }
        update
            .recipients
            .iter()
            .try_for_each(|address| require_address(address))?;
        if let Some(route) = &update.route {
            require_subscription(route)?;
        }

        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let draft = if update.recipients.is_empty() {
            save_recipientless_draft(&tx, update)?
        } else {
            save_addressed_draft(&tx, update)?
        };
        tx.commit()?;
        Ok(draft)
    }
}

fn save_recipientless_draft(
    conn: &rusqlite::Connection,
    update: ComposeDraftUpdate,
) -> Result<ComposeDraft, Error> {
    let existing = crate::recipientless_draft(conn, None)?;
    let Some(existing) = existing else {
        return insert_initial_draft(conn, ConversationId::new(), update);
    };
    if is_blank_payload(&update) {
        return establish_recipientless_intent(conn, existing);
    }
    if !is_blank_draft(&existing) {
        return Err(moved_to(&existing));
    }
    update_existing_draft(conn, existing, update)
}

fn save_addressed_draft(
    conn: &rusqlite::Connection,
    update: ComposeDraftUpdate,
) -> Result<ComposeDraft, Error> {
    let region = crate::conversation_match::identity_region(conn)?;
    let target = conversation_for_compose_recipients(
        conn,
        &update.recipients,
        ConversationId::new(),
        &region,
    )?;
    let Some(conversation_id) = target else {
        return insert_initial_draft(conn, ConversationId::new(), update);
    };
    let existing = load_compose_draft(conn, "conversation_id", &conversation_id.to_string())?;
    let Some(existing) = existing else {
        return insert_initial_draft(conn, conversation_id, update);
    };
    if is_blank_payload(&update) {
        return establish_addressed_intent(conn, existing, update.recipients);
    }
    if !is_blank_draft(&existing) {
        return Err(moved_to(&existing));
    }
    update_existing_draft(conn, existing, update)
}

fn establish_recipientless_intent(
    conn: &rusqlite::Connection,
    draft: ComposeDraft,
) -> Result<ComposeDraft, Error> {
    if !is_draft_only(conn, draft.draft_id)? {
        conn.execute(
            "UPDATE compose_drafts SET draft_only=1 WHERE draft_id=?",
            params![draft.draft_id.to_string()],
        )?;
    }
    Ok(draft)
}

fn establish_addressed_intent(
    conn: &rusqlite::Connection,
    mut draft: ComposeDraft,
    recipients: Vec<String>,
) -> Result<ComposeDraft, Error> {
    if is_draft_only(conn, draft.draft_id)? && draft.recipients == recipients {
        return Ok(draft);
    }
    draft.revision = draft.revision.checked_add(1).ok_or(Error::Database)?;
    draft.recipients = recipients;
    conn.execute(
        "UPDATE compose_drafts SET recipients=?,revision=?,draft_only=1 WHERE draft_id=?",
        params![
            json(&draft.recipients)?,
            to_i64(draft.revision)?,
            draft.draft_id.to_string(),
        ],
    )?;
    Ok(draft)
}

fn insert_initial_draft(
    conn: &rusqlite::Connection,
    conversation_id: ConversationId,
    update: ComposeDraftUpdate,
) -> Result<ComposeDraft, Error> {
    let draft = ComposeDraft {
        draft_id: DraftId::new(),
        conversation_id,
        text: update.text,
        recipients: update.recipients,
        attachment_ids: update.attachment_ids,
        route: update.route,
        revision: 0,
    };
    conn.execute(
        "INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision,draft_only) VALUES(?,?,?,?,?,?,0,1)",
        params![
            draft.draft_id.to_string(),
            draft.conversation_id.to_string(),
            draft.text,
            json(&draft.recipients)?,
            json(&draft.attachment_ids)?,
            draft.route.as_ref().map(json).transpose()?,
        ],
    )?;
    Ok(draft)
}

fn update_existing_draft(
    conn: &rusqlite::Connection,
    existing: ComposeDraft,
    update: ComposeDraftUpdate,
) -> Result<ComposeDraft, Error> {
    let revision = existing.revision.checked_add(1).ok_or(Error::Database)?;
    conn.execute(
        "UPDATE compose_drafts SET text=?,recipients=?,attachment_ids=?,route=?,revision=?,draft_only=1 WHERE draft_id=?",
        params![
            update.text,
            json(&update.recipients)?,
            json(&update.attachment_ids)?,
            update.route.as_ref().map(json).transpose()?,
            to_i64(revision)?,
            existing.draft_id.to_string(),
        ],
    )?;
    Ok(ComposeDraft {
        draft_id: existing.draft_id,
        conversation_id: existing.conversation_id,
        text: update.text,
        recipients: update.recipients,
        attachment_ids: update.attachment_ids,
        route: update.route,
        revision,
    })
}

fn is_blank_payload(draft: &ComposeDraftUpdate) -> bool {
    draft.text.is_empty() && draft.attachment_ids.is_empty() && draft.route.is_none()
}

fn is_blank_draft(draft: &ComposeDraft) -> bool {
    draft.text.is_empty() && draft.attachment_ids.is_empty() && draft.route.is_none()
}

fn moved_to(draft: &ComposeDraft) -> Error {
    Error::DraftMoved {
        draft_id: draft.draft_id,
        conversation_id: draft.conversation_id,
        current_revision: draft.revision,
    }
}
