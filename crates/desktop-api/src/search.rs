use peppy_client_core::{
    LocalSearchHit, LocalSearchPage, Message, MessageWindow, SearchIndexProgress, SearchIndexState,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum SearchKind {
    All,
    Conversation,
    Message,
    Contact,
}

impl From<SearchKind> for peppy_client_core::SearchKind {
    fn from(value: SearchKind) -> Self {
        match value {
            SearchKind::All => Self::All,
            SearchKind::Conversation => Self::Conversation,
            SearchKind::Message => Self::Message,
            SearchKind::Contact => Self::Contact,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "LocalSearchRequest", rename_all = "camelCase")]
pub struct LocalSearchRequestView {
    pub query: String,
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<SearchKind>,
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "AdvanceSearchIndexInput", rename_all = "camelCase")]
pub struct AdvanceSearchIndexInputView {
    #[serde(default)]
    #[ts(optional)]
    pub budget_rows: Option<usize>,
}

impl From<LocalSearchRequestView> for peppy_client_core::LocalSearchRequest {
    fn from(value: LocalSearchRequestView) -> Self {
        Self {
            query: value.query,
            kind: value.kind.map(Into::into),
            cursor: value.cursor,
            limit: value.limit,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename = "LocalSearchHit", rename_all = "camelCase")]
pub enum LocalSearchHitView {
    Conversation {
        conversation_id: String,
        title: String,
        subtitle: String,
    },
    Message {
        conversation_id: String,
        message_id: String,
        title: String,
        snippet: String,
        timestamp: String,
    },
    Contact {
        book_id: String,
        contact_id: String,
        title: String,
        subtitle: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "SearchHasMore", rename_all = "camelCase")]
pub struct SearchHasMoreView {
    pub conversation: bool,
    pub message: bool,
    pub contact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "SearchIndexState", rename_all = "lowercase")]
pub enum SearchIndexStateView {
    Ready,
    Indexing,
}

impl From<SearchIndexState> for SearchIndexStateView {
    fn from(value: SearchIndexState) -> Self {
        match value {
            SearchIndexState::Ready => Self::Ready,
            SearchIndexState::Indexing => Self::Indexing,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "LocalSearchPage", rename_all = "camelCase")]
pub struct LocalSearchPageView {
    pub query: String,
    pub items: Vec<LocalSearchHitView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
    pub index_state: SearchIndexStateView,
    pub has_more: SearchHasMoreView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "SearchIndexProgress", rename_all = "camelCase")]
pub struct SearchIndexProgressView {
    pub index_state: SearchIndexStateView,
    pub processed: usize,
    pub remaining: usize,
}

pub fn local_search_page_view(page: LocalSearchPage) -> LocalSearchPageView {
    LocalSearchPageView {
        query: page.query,
        items: page
            .items
            .into_iter()
            .map(|hit| match hit {
                LocalSearchHit::Conversation {
                    conversation_id,
                    title,
                    subtitle,
                } => LocalSearchHitView::Conversation {
                    conversation_id,
                    title,
                    subtitle,
                },
                LocalSearchHit::Message {
                    conversation_id,
                    message_id,
                    title,
                    snippet,
                    timestamp,
                } => LocalSearchHitView::Message {
                    conversation_id,
                    message_id,
                    title,
                    snippet,
                    timestamp,
                },
                LocalSearchHit::Contact {
                    book_id,
                    contact_id,
                    title,
                    subtitle,
                } => LocalSearchHitView::Contact {
                    book_id,
                    contact_id,
                    title,
                    subtitle,
                },
            })
            .collect(),
        next_cursor: page.next_cursor,
        index_state: page.index_state.into(),
        has_more: SearchHasMoreView {
            conversation: page.has_more.conversation,
            message: page.has_more.message,
            contact: page.has_more.contact,
        },
    }
}

pub fn search_index_progress_view(progress: SearchIndexProgress) -> SearchIndexProgressView {
    SearchIndexProgressView {
        index_state: progress.index_state.into(),
        processed: progress.processed,
        remaining: progress.remaining,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(
    tag = "direction",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
#[ts(rename = "MessageWindowRequest", rename_all = "lowercase")]
pub enum MessageWindowRequestView {
    Around {
        conversation_id: String,
        message_id: String,
    },
    Older {
        conversation_id: String,
        message_id: String,
    },
    Newer {
        conversation_id: String,
        message_id: String,
    },
    Refresh {
        conversation_id: String,
        message_ids: Vec<String>,
    },
}

impl From<MessageWindowRequestView> for peppy_client_core::MessageWindowRequest {
    fn from(value: MessageWindowRequestView) -> Self {
        match value {
            MessageWindowRequestView::Around {
                conversation_id,
                message_id,
            } => Self::Around {
                conversation_id,
                message_id,
            },
            MessageWindowRequestView::Older {
                conversation_id,
                message_id,
            } => Self::Older {
                conversation_id,
                message_id,
            },
            MessageWindowRequestView::Newer {
                conversation_id,
                message_id,
            } => Self::Newer {
                conversation_id,
                message_id,
            },
            MessageWindowRequestView::Refresh {
                conversation_id,
                message_ids,
            } => Self::Refresh {
                conversation_id,
                message_ids,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "MessageWindow", rename_all = "camelCase")]
pub struct MessageWindowView<T> {
    pub conversation_id: String,
    pub messages: Vec<T>,
    pub has_older: bool,
    pub has_newer: bool,
}

pub fn message_window_view<T>(
    window: MessageWindow,
    mut message_view: impl FnMut(Message) -> T,
) -> MessageWindowView<T> {
    MessageWindowView {
        conversation_id: window.conversation_id,
        messages: window.messages.into_iter().map(&mut message_view).collect(),
        has_older: window.has_older,
        has_newer: window.has_newer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_view;
    use peppy_client_core::{
        ConversationId, DeviceId, Direction, MessageId, MessagePayload, MessageRecord,
        SourceSequence, Transport,
    };
    use serde_json::json;

    #[test]
    fn local_search_hit_variants_serialize_camel_case_identifiers() {
        let cases = [
            (
                LocalSearchHitView::Conversation {
                    conversation_id: "conversation".into(),
                    title: "Conversation".into(),
                    subtitle: "Subtitle".into(),
                },
                json!({"kind":"conversation","conversationId":"conversation","title":"Conversation","subtitle":"Subtitle"}),
            ),
            (
                LocalSearchHitView::Message {
                    conversation_id: "conversation".into(),
                    message_id: "message".into(),
                    title: "Message".into(),
                    snippet: "Snippet".into(),
                    timestamp: "1".into(),
                },
                json!({"kind":"message","conversationId":"conversation","messageId":"message","title":"Message","snippet":"Snippet","timestamp":"1"}),
            ),
            (
                LocalSearchHitView::Contact {
                    book_id: "book".into(),
                    contact_id: "contact".into(),
                    title: "Contact".into(),
                    subtitle: "Subtitle".into(),
                },
                json!({"kind":"contact","bookId":"book","contactId":"contact","title":"Contact","subtitle":"Subtitle"}),
            ),
        ];

        for (hit, expected) in cases {
            assert_eq!(serde_json::to_value(hit).unwrap(), expected);
        }
    }

    #[test]
    fn request_dtos_deserialize_wire_casing_and_convert_to_core() {
        let request: LocalSearchRequestView = serde_json::from_value(json!({
            "query": "alice",
            "kind": "message",
            "cursor": "opaque",
            "limit": 6,
        }))
        .unwrap();
        let core_request: peppy_client_core::LocalSearchRequest = request.into();
        assert_eq!(
            core_request.kind,
            Some(peppy_client_core::SearchKind::Message)
        );
        assert_eq!(core_request.limit, Some(6));

        let request: MessageWindowRequestView = serde_json::from_value(json!({
            "direction": "refresh",
            "conversationId": "conversation",
            "messageIds": ["message"],
        }))
        .unwrap();
        assert!(matches!(
            peppy_client_core::MessageWindowRequest::from(request),
            peppy_client_core::MessageWindowRequest::Refresh { conversation_id, message_ids }
                if conversation_id == "conversation" && message_ids == ["message"]
        ));
    }

    #[test]
    fn message_window_mapper_accepts_the_existing_message_view() {
        let message = Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: MessageId::new(),
                    conversation_id: ConversationId::new(),
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec![],
                subject: None,
                body: "Hello".into(),
                transport: Transport::Sms,
                direction: Direction::Outgoing,
                imported: false,
                sent_at_ms: Some(2),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state: None,
            status_at_ms: None,
        };
        let window = message_window_view(
            MessageWindow {
                conversation_id: message.payload.record.conversation_id.to_string(),
                messages: vec![message],
                has_older: true,
                has_newer: false,
            },
            |message| message_view(&message, vec![]),
        );

        let value = serde_json::to_value(window).unwrap();
        assert_eq!(value["messages"][0]["body"], "Hello");
        assert!(value["messages"][0].get("id").is_some());
        assert_eq!(value["hasOlder"], true);
        assert_eq!(value["hasNewer"], false);
    }
}
