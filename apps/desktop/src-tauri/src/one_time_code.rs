//! Copy-code writes and marks read, in that order. Clipboard failure leaves the item unread.
//! Callers (the in-app command and popup actions) own window and session admission.
use crate::{error::core_error, BridgeError, BridgeResult, ConversationId, Session};
use peppy_client_core::OneTimeCodeTarget;

pub(crate) fn write_clipboard_text(code: &str) -> Result<(), ()> {
    if crate::sensitive_clipboard::write_sensitive_text(code) {
        Ok(())
    } else {
        Err(())
    }
}

/// Message targets follow the caller's composer scope; notification targets are main-window only.
pub(crate) fn require_target_window(target: &OneTimeCodeTarget, label: &str) -> BridgeResult<()> {
    match target {
        OneTimeCodeTarget::Message { .. } => crate::require_main_or_composer(label),
        OneTimeCodeTarget::Notification { .. } => crate::require_main(label),
    }
}

pub(crate) fn copy_and_mark(
    session: &Session,
    target: &OneTimeCodeTarget,
    conversation_scope: Option<ConversationId>,
) -> BridgeResult<()> {
    copy_and_mark_with(session, target, conversation_scope, write_clipboard_text)
}

pub(crate) fn copy_and_mark_with(
    session: &Session,
    target: &OneTimeCodeTarget,
    conversation_scope: Option<ConversationId>,
    write: impl FnOnce(&str) -> Result<(), ()>,
) -> BridgeResult<()> {
    let matched = session
        .client
        .one_time_code_for(target)
        .map_err(core_error)?
        .ok_or_else(|| BridgeError::new("no-code", "This item no longer has a one-time code."))?;
    if let Some(scope) = conversation_scope {
        let scope_id = scope.to_string();
        if matched.conversation_id.as_deref() != Some(scope_id.as_str()) {
            return Err(BridgeError::new(
                "window-context",
                "A composer window can only copy codes from its own conversation.",
            ));
        }
    }
    write(&matched.code).map_err(|_| {
        BridgeError::new(
            "clipboard-unavailable",
            "Could not copy the one-time code to the clipboard.",
        )
    })?;
    match target {
        OneTimeCodeTarget::Message { message_id } => {
            session.mark_seen_scoped(std::slice::from_ref(message_id), conversation_scope)
        }
        OneTimeCodeTarget::Notification { target } => {
            session
                .client
                .mark_notifications_seen(vec![target.clone()])
                .map_err(core_error)?;
            session.notify();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{fixture, open, unlock};
    use peppy_client_core::IncomingSms;
    use std::cell::{Cell, RefCell};

    const CODE_BODY: &str = "Your verification code is 482913";

    fn capture(
        session: &Session,
        sender: &str,
        body: &str,
        provider_id: &str,
    ) -> (ConversationId, String) {
        let captured = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: sender.into(),
                body: body.into(),
                provider_message_id: Some(provider_id.into()),
                imported: false,
            })
            .unwrap();
        let message_id = session.client.messages(captured.conversation_id).unwrap()[0]
            .payload
            .record
            .message_id
            .to_string();
        (captured.conversation_id, message_id)
    }

    fn unread(session: &Session, conversation: ConversationId) -> u64 {
        session
            .client
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|item| item.conversation_id == conversation)
            .unwrap()
            .unread_count
    }

    #[test]
    fn copy_writes_the_code_then_marks_the_message_read() {
        let f = fixture();
        let session = open(&f, &f.binding, &[]);
        unlock(&session, &f);
        let (conversation, message_id) = capture(&session, "+15555550201", CODE_BODY, "copy-ok");
        let written = RefCell::new(Vec::new());
        copy_and_mark_with(
            &session,
            &OneTimeCodeTarget::Message {
                message_id: message_id.clone(),
            },
            None,
            |code| {
                written.borrow_mut().push(code.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*written.borrow(), ["482913"]);
        assert_eq!(unread(&session, conversation), 0);
    }

    #[test]
    fn clipboard_failure_does_not_mark_the_message_read() {
        let f = fixture();
        let session = open(&f, &f.binding, &[]);
        unlock(&session, &f);
        let (conversation, message_id) = capture(&session, "+15555550202", CODE_BODY, "copy-fail");
        let error = copy_and_mark_with(
            &session,
            &OneTimeCodeTarget::Message { message_id },
            None,
            |_| Err(()),
        )
        .unwrap_err();
        assert_eq!(error.code, "clipboard-unavailable");
        assert!(!error.message.contains("482913"));
        assert_eq!(unread(&session, conversation), 1);
    }

    #[test]
    fn composer_foreign_conversation_is_rejected_before_any_clipboard_write() {
        let f = fixture();
        let session = open(&f, &f.binding, &[]);
        unlock(&session, &f);
        let (source, message_id) = capture(&session, "+15555550203", CODE_BODY, "copy-foreign");
        let (other, _) = capture(&session, "+15555550204", "Hello there", "copy-other");
        assert_ne!(source, other);
        let writes = Cell::new(0);
        let error = copy_and_mark_with(
            &session,
            &OneTimeCodeTarget::Message { message_id },
            Some(other),
            |_| {
                writes.set(writes.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "window-context");
        assert_eq!(writes.get(), 0);
        assert_eq!(unread(&session, source), 1);
    }

    #[test]
    fn composer_own_conversation_copies_and_marks_read() {
        let f = fixture();
        let session = open(&f, &f.binding, &[]);
        unlock(&session, &f);
        let (conversation, message_id) = capture(&session, "+15555550205", CODE_BODY, "copy-own");
        copy_and_mark_with(
            &session,
            &OneTimeCodeTarget::Message { message_id },
            Some(conversation),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(unread(&session, conversation), 0);
    }

    #[test]
    fn missing_code_writes_nothing_and_reports_no_code() {
        let f = fixture();
        let session = open(&f, &f.binding, &[]);
        unlock(&session, &f);
        let (conversation, message_id) =
            capture(&session, "+15555550206", "Lunch at noon?", "copy-none");
        let writes = Cell::new(0);
        let error = copy_and_mark_with(
            &session,
            &OneTimeCodeTarget::Message { message_id },
            None,
            |_| {
                writes.set(writes.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "no-code");
        assert_eq!(writes.get(), 0);
        assert_eq!(unread(&session, conversation), 1);
    }

    #[test]
    fn copy_targets_are_windowed_by_kind() {
        let notification = OneTimeCodeTarget::Notification {
            target: peppy_client_core::NotificationTarget {
                source_device_id: "phone".into(),
                notification_key: "key".into(),
                lifetime: "life".into(),
            },
        };
        let message = OneTimeCodeTarget::Message {
            message_id: "id".into(),
        };
        let composer = format!("composer-{}", ConversationId::new());
        assert!(require_target_window(&notification, crate::tray::MAIN).is_ok());
        assert!(require_target_window(&notification, &composer).is_err());
        assert!(require_target_window(&notification, "head-example").is_err());
        assert!(require_target_window(&message, crate::tray::MAIN).is_ok());
        assert!(require_target_window(&message, &composer).is_ok());
        assert!(require_target_window(&message, "head-example").is_err());
    }
}
