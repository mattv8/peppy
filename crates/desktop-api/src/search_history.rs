use serde::{Deserialize, Serialize};
use ts_rs::TS;

impl HistorySearchSnapshot {
    /// Projects the core's query-free semantic state into the generated bridge DTO.
    pub fn from_core(status: peppy_client_core::HistorySearchStatus) -> Self {
        Self {
            attempt_id: status.attempt_id.map(|id| id.to_string()),
            state: match status.state {
                peppy_client_core::HistorySearchState::Idle => HistorySearchState::Idle,
                peppy_client_core::HistorySearchState::CatchingUp => HistorySearchState::CatchingUp,
                peppy_client_core::HistorySearchState::WaitingForPhone => {
                    HistorySearchState::WaitingForPhone
                }
                peppy_client_core::HistorySearchState::Indexing => HistorySearchState::Indexing,
                peppy_client_core::HistorySearchState::Partial => HistorySearchState::Partial,
                peppy_client_core::HistorySearchState::Completed => HistorySearchState::Completed,
                peppy_client_core::HistorySearchState::Blocked => HistorySearchState::Blocked,
                peppy_client_core::HistorySearchState::Failed => HistorySearchState::Failed,
                peppy_client_core::HistorySearchState::Cancelled => HistorySearchState::Cancelled,
            },
            reason: status.reason.map(|reason| match reason {
                peppy_client_core::HistorySearchReason::Offline => HistorySearchReason::Offline,
                peppy_client_core::HistorySearchReason::Permission => {
                    HistorySearchReason::Permission
                }
                peppy_client_core::HistorySearchReason::Capacity => HistorySearchReason::Capacity,
                peppy_client_core::HistorySearchReason::NeedsUnlock => {
                    HistorySearchReason::NeedsUnlock
                }
                peppy_client_core::HistorySearchReason::Unsupported => {
                    HistorySearchReason::Unsupported
                }
                peppy_client_core::HistorySearchReason::Timeout => HistorySearchReason::Timeout,
            }),
            can_search_further: status.can_search_further,
            progress_version: status.progress_version,
            index_version: status.index_version,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "HistorySearchState", rename_all = "camelCase")]
pub enum HistorySearchState {
    Idle,
    CatchingUp,
    WaitingForPhone,
    Indexing,
    Partial,
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "HistorySearchReason", rename_all = "camelCase")]
pub enum HistorySearchReason {
    Offline,
    Permission,
    Capacity,
    NeedsUnlock,
    Unsupported,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "HistorySearchSnapshot", rename_all = "camelCase")]
pub struct HistorySearchSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attempt_id: Option<String>,
    pub state: HistorySearchState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<HistorySearchReason>,
    #[serde(default)]
    pub can_search_further: bool,
    pub progress_version: String,
    pub index_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "StartHistorySearchInput", rename_all = "camelCase")]
pub struct StartHistorySearchInput {
    pub operation_id: String,
}

impl StartHistorySearchInput {
    pub fn parse_operation_id(&self) -> Result<uuid::Uuid, &'static str> {
        parse_canonical_uuid(&self.operation_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "HistorySearchAttemptInput", rename_all = "camelCase")]
pub struct HistorySearchAttemptInput {
    pub attempt_id: String,
}

impl HistorySearchAttemptInput {
    pub fn parse_attempt_id(&self) -> Result<uuid::Uuid, &'static str> {
        parse_canonical_uuid(&self.attempt_id)
    }
}

/// Accepts only the canonical form: lowercase, hyphenated, non-nil UUID.
/// Simple, braced, URN, uppercase, and nil spellings are rejected.
fn parse_canonical_uuid(value: &str) -> Result<uuid::Uuid, &'static str> {
    let uuid = uuid::Uuid::parse_str(value).map_err(|_| "invalid UUID")?;
    if value != uuid.hyphenated().to_string() {
        return Err("UUID must be lowercase hyphenated");
    }
    if uuid.is_nil() {
        return Err("UUID must not be nil");
    }
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CANONICAL: &str = "d2719a52-d647-49ec-bb81-6da2a24e8cc2";

    #[test]
    fn snapshot_serializes_camel_case_string_revisions() {
        let snapshot = HistorySearchSnapshot {
            attempt_id: Some("attempt".into()),
            state: HistorySearchState::Partial,
            reason: Some(HistorySearchReason::Timeout),
            can_search_further: true,
            progress_version: "18446744073709551615".into(),
            index_version: "index-revision".into(),
        };

        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "attemptId": "attempt",
                "state": "partial",
                "reason": "timeout",
                "canSearchFurther": true,
                "progressVersion": "18446744073709551615",
                "indexVersion": "index-revision",
            })
        );
    }

    #[test]
    fn snapshot_omits_absent_attempt_id_and_reason() {
        let snapshot = HistorySearchSnapshot {
            attempt_id: None,
            state: HistorySearchState::Idle,
            reason: None,
            can_search_further: false,
            progress_version: "0".into(),
            index_version: "index".into(),
        };

        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "state": "idle",
                "canSearchFurther": false,
                "progressVersion": "0",
                "indexVersion": "index",
            })
        );
    }

    #[test]
    fn start_input_requires_a_valid_uuid_operation_id() {
        let input: StartHistorySearchInput = serde_json::from_value(json!({
            "operationId": "not-a-uuid",
        }))
        .unwrap();

        assert!(input.parse_operation_id().is_err());
    }

    #[test]
    fn operation_id_accepts_only_canonical_lowercase_hyphenated_nonnil_uuid() {
        let input = StartHistorySearchInput {
            operation_id: CANONICAL.into(),
        };

        assert_eq!(
            input.parse_operation_id().unwrap().hyphenated().to_string(),
            CANONICAL
        );
    }

    #[test]
    fn operation_id_rejects_noncanonical_spellings_and_nil() {
        for value in noncanonical_values() {
            let input = StartHistorySearchInput {
                operation_id: value.clone(),
            };

            assert!(
                input.parse_operation_id().is_err(),
                "operation id {value} should be rejected"
            );
        }
    }

    #[test]
    fn start_input_does_not_accept_query_or_renderer_identity() {
        let result = serde_json::from_value::<StartHistorySearchInput>(json!({
            "operationId": CANONICAL,
            "query": "private search",
            "requesterId": "renderer-controlled",
        }));

        assert!(result.is_err());
    }

    #[test]
    fn attempt_input_requires_a_valid_uuid_attempt_id() {
        let input: HistorySearchAttemptInput = serde_json::from_value(json!({
            "attemptId": "not-a-uuid",
        }))
        .unwrap();

        assert!(input.parse_attempt_id().is_err());
    }

    #[test]
    fn attempt_id_accepts_only_canonical_lowercase_hyphenated_nonnil_uuid() {
        let input = HistorySearchAttemptInput {
            attempt_id: CANONICAL.into(),
        };

        assert_eq!(
            input.parse_attempt_id().unwrap().hyphenated().to_string(),
            CANONICAL
        );
    }

    #[test]
    fn attempt_id_rejects_noncanonical_spellings_and_nil() {
        for value in noncanonical_values() {
            let input = HistorySearchAttemptInput {
                attempt_id: value.clone(),
            };

            assert!(
                input.parse_attempt_id().is_err(),
                "attempt id {value} should be rejected"
            );
        }
    }

    #[test]
    fn attempt_input_does_not_accept_query_or_unknown_fields() {
        let result = serde_json::from_value::<HistorySearchAttemptInput>(json!({
            "attemptId": CANONICAL,
            "query": "private search",
        }));

        assert!(result.is_err());
    }

    #[test]
    fn core_status_projects_every_state_and_reason_into_the_bridge_dto() {
        use peppy_client_core::{
            HistorySearchReason as CoreReason, HistorySearchState as CoreState, HistorySearchStatus,
        };

        let states = [
            (CoreState::Idle, HistorySearchState::Idle),
            (CoreState::CatchingUp, HistorySearchState::CatchingUp),
            (
                CoreState::WaitingForPhone,
                HistorySearchState::WaitingForPhone,
            ),
            (CoreState::Indexing, HistorySearchState::Indexing),
            (CoreState::Partial, HistorySearchState::Partial),
            (CoreState::Completed, HistorySearchState::Completed),
            (CoreState::Blocked, HistorySearchState::Blocked),
            (CoreState::Failed, HistorySearchState::Failed),
            (CoreState::Cancelled, HistorySearchState::Cancelled),
        ];
        let reasons = [
            (CoreReason::Offline, HistorySearchReason::Offline),
            (CoreReason::Permission, HistorySearchReason::Permission),
            (CoreReason::Capacity, HistorySearchReason::Capacity),
            (CoreReason::NeedsUnlock, HistorySearchReason::NeedsUnlock),
            (CoreReason::Unsupported, HistorySearchReason::Unsupported),
            (CoreReason::Timeout, HistorySearchReason::Timeout),
        ];
        for (core_state, dto_state) in states {
            for (core_reason, dto_reason) in reasons
                .map(|pair| (Some(pair.0), Some(pair.1)))
                .into_iter()
                .chain([(None, None)])
            {
                let snapshot = HistorySearchSnapshot::from_core(HistorySearchStatus {
                    attempt_id: None,
                    state: core_state,
                    reason: core_reason,
                    can_search_further: true,
                    progress_version: "1".into(),
                    index_version: "2".into(),
                });
                assert_eq!(snapshot.state, dto_state);
                assert_eq!(snapshot.reason, dto_reason);
                assert_eq!(snapshot.index_version, "2");
            }
        }
    }

    #[test]
    fn legacy_snapshot_without_availability_is_not_actionable() {
        let snapshot: HistorySearchSnapshot = serde_json::from_value(json!({
            "state": "idle",
            "progressVersion": "progress",
            "indexVersion": "index",
        }))
        .unwrap();

        assert!(!snapshot.can_search_further);
    }

    fn noncanonical_values() -> Vec<String> {
        vec![
            "D2719A52-D647-49EC-BB81-6DA2A24E8CC2".into(),
            "d2719a52d64749ecbb816da2a24e8cc2".into(),
            "{d2719a52-d647-49ec-bb81-6da2a24e8cc2}".into(),
            "urn:uuid:d2719a52-d647-49ec-bb81-6da2a24e8cc2".into(),
            "00000000-0000-0000-0000-000000000000".into(),
        ]
    }
}
