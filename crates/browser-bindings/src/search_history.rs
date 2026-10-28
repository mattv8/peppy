use super::*;
use peppy_client_core::{HistorySearchSyncFacts, HistorySearchWork};
use peppy_desktop_api::{
    HistorySearchAttemptInput, HistorySearchSnapshot, StartHistorySearchInput,
};

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "get_history_search"
            | "start_history_search"
            | "cancel_history_search"
            | "_worker_history_search_sync_context"
            | "_worker_observe_history_search_sync"
            | "_worker_history_search_work"
    )
}

impl BrowserCore {
    pub(super) fn search_history_command(
        &self,
        command: &str,
        args: Value,
    ) -> Result<Value, Failure> {
        match command {
            "get_history_search" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Input {
                    attempt_id: Option<String>,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                let attempt_id = input
                    .attempt_id
                    .map(|attempt_id| HistorySearchAttemptInput { attempt_id }.parse_attempt_id())
                    .transpose()
                    .map_err(|_| invalid())?;
                snapshot(
                    self.client()?
                        .history_search_status(attempt_id)
                        .map_err(core)?,
                )
            }
            "start_history_search" => {
                let input: StartHistorySearchInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let operation_id = input.parse_operation_id().map_err(|_| invalid())?;
                snapshot(
                    self.client()?
                        .start_history_search(operation_id)
                        .map_err(core)?,
                )
            }
            "cancel_history_search" => {
                let input: HistorySearchAttemptInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let attempt_id = input.parse_attempt_id().map_err(|_| invalid())?;
                snapshot(
                    self.client()?
                        .cancel_history_search(attempt_id)
                        .map_err(core)?,
                )
            }
            "_worker_history_search_sync_context" => {
                serde_json::to_value(self.client()?.history_search_sync_context().map_err(core)?)
                    .map_err(|_| core(CoreError::Database))
            }
            "_worker_observe_history_search_sync" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Input {
                    attempt_id: String,
                    facts: HistorySearchSyncFacts,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                let attempt_id = HistorySearchAttemptInput {
                    attempt_id: input.attempt_id,
                }
                .parse_attempt_id()
                .map_err(|_| invalid())?;
                snapshot(
                    self.client()?
                        .observe_history_search_sync(attempt_id, input.facts)
                        .map_err(core)?,
                )
            }
            "_worker_history_search_work" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Input {
                    attempt_id: String,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                let attempt_id = HistorySearchAttemptInput {
                    attempt_id: input.attempt_id,
                }
                .parse_attempt_id()
                .map_err(|_| invalid())?;
                Ok(
                    match self
                        .client()?
                        .history_search_work(attempt_id)
                        .map_err(core)?
                    {
                        HistorySearchWork::WakeSync => json!({"kind":"wakeSync"}),
                        HistorySearchWork::AdvanceIndex { limit } => {
                            json!({"kind":"advanceIndex","limit":limit})
                        }
                        HistorySearchWork::RequestPhone {
                            request_id,
                            target_device_id,
                        } => {
                            json!({"kind":"requestPhone","requestId":request_id,"targetDeviceId":target_device_id})
                        }
                        HistorySearchWork::Wait => json!({"kind":"wait"}),
                    },
                )
            }
            _ => Err(unknown_command()),
        }
    }
}

fn snapshot(status: peppy_client_core::HistorySearchStatus) -> Result<Value, Failure> {
    serde_json::to_value(HistorySearchSnapshot::from_core(status))
        .map_err(|_| core(CoreError::Database))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_routes_reject_noncanonical_or_extra_renderer_input_and_hide_worker_routes() {
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        for args in [
            json!({"operationId":"018F2C5E-6A3B-7C4D-8E5F-0123456789AB"}),
            json!({"operationId":"018f2c5e-6a3b-7c4d-8e5f-0123456789ab","query":"private"}),
        ] {
            let response: Value = serde_json::from_str(
                &core.dispatch(&json!({"command":"start_history_search","args":args}).to_string()),
            )
            .unwrap();
            assert_eq!(response["error"]["code"], "invalid-request");
        }
        let private: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"_worker_history_search_work","args":{}}"#),
        )
        .unwrap();
        assert_eq!(private["error"]["code"], "invalid-request");
    }

    #[test]
    fn worker_proof_and_work_wire_shapes_are_camel_case_and_numeric() {
        let facts: HistorySearchSyncFacts = serde_json::from_value(json!({"proof":{
            "context":{"startedAtMs":10,"snapshotGeneration":2},
            "highWaterCursor":"0","nextAfterPresent":true,"terminalNextAfter":true
        }}))
        .unwrap();
        assert_eq!(facts.proof.unwrap().context.started_at_ms, 10);
        assert_eq!(
            json!({"kind":"requestPhone","requestId":"request","targetDeviceId":"device"})["kind"],
            "requestPhone"
        );
        assert!(handles("get_history_search"));
        assert!(handles("_worker_history_search_sync_context"));
    }
}
