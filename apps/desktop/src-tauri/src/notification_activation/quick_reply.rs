use super::{
    recovery_store::{RecoveryItem, RecoveryReservation, RecoveryState},
    Destination, PopupAction, PopupActions, PostError,
};
use crate::{
    error::{BridgeError, BridgeResult},
    AppState,
};
use peppy_client_core::{
    BannerCandidate, GatewayRoute, NotificationPreview, OneTimeCodeTarget, Transport,
};
use peppy_desktop_api::{
    account_access::{AccountAccessResult, AccountNoticeKindDto},
    GatewayView,
};
use serde::{Deserialize, Serialize};
use std::{
    str::FromStr,
    sync::{atomic::Ordering, Arc, Mutex},
};
use tauri::{Manager, State, WebviewWindow};

#[cfg(any(target_os = "macos", target_os = "windows"))]
type Presentation = Arc<Mutex<Option<super::owner::PopupVersion>>>;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
type Presentation = ();

#[derive(Clone)]
struct ReplyCapability {
    app: tauri::AppHandle,
    popup_id: String,
    command_id: String,
    session_generation: String,
    message_id: String,
    conversation_id: String,
    selection: Option<ReplySelection>,
    copy_target: Option<OneTimeCodeTarget>,
    reservation: Arc<RecoveryReservation>,
    policy_cancel: tokio_util::sync::CancellationToken,
    generation_cancel: tokio_util::sync::CancellationToken,
    popup_cancel: Arc<PopupCancellation>,
    presentation: Presentation,
}

#[derive(Clone)]
struct ReplySelection {
    route: Option<GatewayRoute>,
    transport: Transport,
    member_revision: Option<String>,
}

fn select_reply(session: &crate::session::Session, conversation: &str) -> Option<ReplySelection> {
    let context = session
        .client
        .quick_reply_context(conversation.parse().ok()?)
        .ok()?;
    let saved = session
        .client
        .compose_drafts()
        .ok()?
        .into_iter()
        .find(|draft| draft.conversation_id.to_string() == conversation)
        .and_then(|draft| draft.route);
    let (gateways, known) = session.gateways();
    Some(ReplySelection {
        route: select_route(&gateways, known, saved.as_ref()),
        transport: context.transport,
        member_revision: context
            .expected_member_revision
            .map(|value| value.to_string()),
    })
}

struct RevealedMessage {
    title: String,
    body: String,
    selection: ReplySelection,
}

fn lookup_reveal_message(
    session: &crate::session::Session,
    message_id: peppy_client_core::MessageId,
    conversation_id: &str,
) -> Option<RevealedMessage> {
    let message = session.client.message_by_id(message_id).ok().flatten()?;
    if message.payload.record.conversation_id.to_string() != conversation_id {
        return None;
    }
    let title = message
        .payload
        .sender_address
        .as_deref()
        .map(|sender| crate::contacts::banner_title(&session.client, sender, None))
        .unwrap_or_else(|| "New message".into());
    Some(RevealedMessage {
        title,
        body: message.payload.body,
        selection: select_reply(session, conversation_id)?,
    })
}

fn select_route(
    gateways: &[GatewayView],
    known: bool,
    saved: Option<&GatewayRoute>,
) -> Option<GatewayRoute> {
    // Keep the saved identity even if it vanished; submission must refuse, never switch SIMs.
    if let Some(saved) = saved {
        return Some(saved.clone());
    }
    let [gateway] = gateways else {
        return None;
    };
    if !known {
        return None;
    }
    let gateway_device_id = gateway.id.parse().ok()?;
    Some(if gateway.sim_id.is_empty() {
        GatewayRoute::RcsLink { gateway_device_id }
    } else {
        GatewayRoute::Carrier {
            gateway_device_id,
            subscription_id: gateway.sim_id.clone(),
        }
    })
}

struct PopupCancellation {
    owner: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
}

impl Drop for PopupCancellation {
    fn drop(&mut self) {
        self.stop.cancel();
        self.owner.cancel();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuickReplySettings {
    enabled: bool,
    supported: bool,
}

fn require_unlocked(state: &AppState) -> BridgeResult<()> {
    if state.app_locked.load(Ordering::Acquire)
        || !crate::app_lock_actions_allowed(
            state.app_lock_checking.load(Ordering::Acquire),
            state.app_lock_storage_available.load(Ordering::Acquire),
        )
    {
        return Err(BridgeError::new("app-locked", "Unlock Peppy to continue."));
    }
    Ok(())
}

/// Posts an ordinary banner unless this exact message can safely carry a native reply capability.
/// The notification drain owns candidate acknowledgement; this function never changes read state.
pub(crate) struct BannerPresentation {
    pub destination: Destination,
    pub title: String,
    pub body: String,
    pub icon: Option<crate::notification_icon::Icon>,
    pub copy: Option<OneTimeCodeTarget>,
}

pub(crate) fn post_candidate(
    app: &tauri::AppHandle,
    session: &crate::session::Session,
    candidate: Option<&BannerCandidate>,
    banner: BannerPresentation,
) -> Result<(), PostError> {
    let actions = candidate.and_then(|candidate| actions_for_candidate(app, session, candidate));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some((popup_id, cancel, presentation, actions)) = actions {
        return submit_popup(
            app,
            session,
            PopupRequest {
                request_id: popup_id,
                cancel,
                presentation,
                sound: true,
                banner,
                actions,
            },
        );
    }
    super::post(
        app,
        session,
        banner.destination,
        banner.title,
        banner.body,
        banner.icon,
        banner.copy,
    )
}

fn actions_for_candidate(
    app: &tauri::AppHandle,
    session: &crate::session::Session,
    candidate: &BannerCandidate,
) -> Option<(
    String,
    tokio_util::sync::CancellationToken,
    Presentation,
    PopupActions,
)> {
    let state = app.state::<AppState>();
    let _effect = state
        .notification_activation
        .effect_gate
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if candidate.kind != "message"
        || require_unlocked(&state).is_err()
        || !state.notifications.quick_reply_enabled()
        || !state.notification_activation.quick_reply_supported()
        || !super::current(Some(session), &session.generation)
        || !super::unlocked(&state, session)
    {
        return None;
    }
    let conversation_id = candidate.conversation_id.clone()?;
    let preferences = session
        .notification_preferences(&state.notifications)
        .ok()?;
    let reveal = preferences.preview != NotificationPreview::Full;
    let selection = if reveal {
        None
    } else {
        Some(select_reply(session, &conversation_id)?)
    };
    let reservation = state.notification_activation.recovery.reserve()?;
    let policy_cancel = state.notifications.quick_reply_policy_cancel();
    let generation_cancel = state
        .notification_activation
        .quick_reply_generation_cancel();
    let popup_cancel = Arc::new(PopupCancellation {
        owner: tokio_util::sync::CancellationToken::new(),
        stop: tokio_util::sync::CancellationToken::new(),
    });
    let wake_owner: Arc<dyn Fn() + Send + Sync> = {
        let app = app.clone();
        Arc::new(move || app.state::<AppState>().notification_activation.wake())
    };
    watch_popup_cancellation(
        session.cancel.clone(),
        policy_cancel.clone(),
        generation_cancel.clone(),
        popup_cancel.owner.clone(),
        popup_cancel.stop.clone(),
        wake_owner,
    );
    let capability = Arc::new(ReplyCapability {
        app: app.clone(),
        popup_id: uuid::Uuid::new_v4().simple().to_string(),
        command_id: uuid::Uuid::new_v4().to_string(),
        session_generation: session.generation.clone(),
        message_id: candidate.id.clone(),
        conversation_id,
        selection,
        copy_target: candidate
            .one_time_code
            .as_ref()
            .map(|_| OneTimeCodeTarget::Message {
                message_id: candidate.id.clone(),
            }),
        reservation,
        policy_cancel,
        generation_cancel,
        popup_cancel,
        presentation: {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                Arc::new(Mutex::new(None))
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                ()
            }
        },
    });
    Some((
        capability.popup_id.clone(),
        capability.popup_cancel.owner.clone(),
        capability.presentation.clone(),
        popup_actions(capability, reveal),
    ))
}

fn watch_popup_cancellation(
    session_cancel: tokio_util::sync::CancellationToken,
    policy_cancel: tokio_util::sync::CancellationToken,
    generation_cancel: tokio_util::sync::CancellationToken,
    owner_cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
    wake_owner: Arc<dyn Fn() + Send + Sync>,
) {
    drop(tauri::async_runtime::spawn(async move {
        let cancelled = tokio::select! {
            _ = session_cancel.cancelled() => true,
            _ = policy_cancel.cancelled() => true,
            _ = generation_cancel.cancelled() => true,
            _ = stop.cancelled() => false,
        };
        if cancelled {
            owner_cancel.cancel();
            wake_owner();
        }
    }));
}

fn popup_actions(capability: Arc<ReplyCapability>, reveal: bool) -> PopupActions {
    PopupActions {
        reveal,
        reply: !reveal,
        handler: Arc::new(move |action| handle_popup_action(capability.clone(), action)),
    }
}

fn handle_popup_action(capability: Arc<ReplyCapability>, action: PopupAction) {
    match action {
        PopupAction::Reveal => reveal_message(capability),
        PopupAction::Reply(body)
            if body.len() <= super::MAX_REPLY_BYTES && !body.trim().is_empty() =>
        {
            queue_reply(capability, body);
        }
        PopupAction::Open | PopupAction::Copy | PopupAction::Reply(_) => {}
    }
}

fn retire_failed_reveal(capability: &ReplyCapability) {
    capability.popup_cancel.owner.cancel();
    capability
        .app
        .state::<AppState>()
        .notification_activation
        .wake();
}

fn reveal_message(capability: Arc<ReplyCapability>) {
    drop(tauri::async_runtime::spawn(async move {
        let state = capability.app.state::<AppState>();
        let Ok(_operation) = crate::admit_update_operation(&state) else {
            retire_failed_reveal(&capability);
            return;
        };
        let Some(session) = current_reply_session(&state, &capability).await else {
            retire_failed_reveal(&capability);
            return;
        };
        let Some(message_id) = peppy_client_core::MessageId::from_str(&capability.message_id).ok()
        else {
            retire_failed_reveal(&capability);
            return;
        };
        // The original message lookup can take the core DB mutex. Never perform it while
        // retaining the async session mutex, because replacement callbacks may need that lock.
        let message = crate::blocking({
            let session = session.clone();
            let conversation_id = capability.conversation_id.clone();
            move || {
                Ok(lookup_reveal_message(
                    &session,
                    message_id,
                    &conversation_id,
                ))
            }
        })
        .await
        .ok()
        .flatten();
        let Some(RevealedMessage {
            title,
            body,
            selection,
        }) = message
        else {
            retire_failed_reveal(&capability);
            return;
        };
        let destination = Destination::Conversation {
            conversation_id: capability.conversation_id.clone(),
        };
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let Some(replacement_session) = current_reply_session(&state, &capability).await
        else {
            retire_failed_reveal(&capability);
            return;
        };
        let capability = Arc::new(ReplyCapability {
            selection: Some(selection),
            ..(*capability).clone()
        });
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let replacement_capability = capability.clone();
            let replacement = tauri::async_runtime::spawn_blocking(move || {
                submit_popup(
                    &replacement_capability.app,
                    &replacement_session,
                    PopupRequest {
                        request_id: replacement_capability.popup_id.clone(),
                        cancel: replacement_capability.popup_cancel.owner.clone(),
                        presentation: replacement_capability.presentation.clone(),
                        sound: false,
                        banner: BannerPresentation {
                            destination,
                            title,
                            body,
                            icon: Some(crate::notification_icon::Icon::Message),
                            copy: replacement_capability.copy_target.clone(),
                        },
                        actions: popup_actions(replacement_capability.clone(), false),
                    },
                )
            })
            .await;
            if !matches!(replacement, Ok(Ok(()))) {
                retire_failed_reveal(&capability);
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        retire_failed_reveal(&capability);
    }));
}

fn queue_reply(capability: Arc<ReplyCapability>, body: String) {
    // Owner callbacks are intentionally content-free and return immediately; core work runs here.
    // The session guard is acquired before the effect gate and neither guard spans an await.
    drop(tauri::async_runtime::spawn(async move {
        let state = capability.app.state::<AppState>();
        let _operation = match crate::admit_update_operation(&state) {
            Ok(operation) => operation,
            Err(_) => return,
        };
        let Some(session) = current_reply_session(&state, &capability).await else {
            return;
        };
        let Some(input) = quick_reply_input(&capability, &body) else {
            finish_rejected_reply(&state, &capability, &session, body, false).await;
            return;
        };
        // Reconcile the stable command ID before account or gateway preflight. This is read-only
        // and must win over a now-stale route or account notice after a lost queue result.
        let reconciliation = crate::blocking({
            let client = session.client.clone();
            let input = input.clone();
            move || {
                Ok(peppy_desktop_api::quick_reply::reconcile_quick_reply(
                    &client, input,
                ))
            }
        })
        .await;
        match reconciliation {
            Ok(Ok(Some(_))) => {
                session.request_work();
                state.record_recent(&session.binding, &capability.conversation_id);
                (state.notifier)();
                return;
            }
            Ok(Err(error)) if reconciliation_is_uncertain(&error) => {
                finish_rejected_reply(&state, &capability, &session, body, true).await;
                return;
            }
            Err(_) | Ok(Err(_)) => {
                finish_rejected_reply(&state, &capability, &session, body, true).await;
                return;
            }
            Ok(Ok(None)) => {}
        }

        // The device-authenticated account read is deliberately outside session/effect locks.
        // The final blocking admission below rechecks every authority after this await.
        let account = crate::account_access::status(session.clone()).await;
        if !account_allows_reply(account.as_ref().ok()) {
            finish_rejected_reply(&state, &capability, &session, body, false).await;
            return;
        }

        let queued = crate::blocking({
            let app = capability.app.clone();
            let capability = capability.clone();
            let input = input.clone();
            move || {
                let state = app.state::<AppState>();
                // Retirement takes these in this order. Use blocking_lock here, never while
                // awaiting, so owner callbacks cannot deadlock behind a posted notification.
                let session_guard = state.session.blocking_lock();
                let Some(current) = session_guard
                    .as_deref()
                    .filter(|current| reply_capability_current(&state, &capability, current))
                else {
                    return Ok(QueueResult::Unavailable);
                };
                let _effect_gate = state.notification_activation.effect_gate.lock().unwrap();
                if !reply_capability_current(&state, &capability, current) {
                    return Ok(QueueResult::Unavailable);
                }
                let (gateways, known) = current.gateways();
                match peppy_desktop_api::quick_reply::queue_quick_reply(
                    &current.client,
                    input,
                    known.then_some(gateways.as_slice()),
                ) {
                    Ok(_) => Ok(QueueResult::Queued),
                    Err(error) if reconciliation_is_uncertain(&error) => Ok(QueueResult::Uncertain),
                    Err(_) => Ok(QueueResult::Rejected),
                }
            }
        })
        .await
        .unwrap_or(QueueResult::Uncertain);

        if queued == QueueResult::Queued {
            session.request_work();
            state.record_recent(&session.binding, &capability.conversation_id);
            (state.notifier)();
            return;
        }
        let queued = if queued == QueueResult::Uncertain {
            let client = session.client.clone();
            match crate::blocking(move || {
                Ok(peppy_desktop_api::quick_reply::reconcile_quick_reply(
                    &client, input,
                ))
            })
            .await
            {
                Ok(Ok(Some(_))) => {
                    session.request_work();
                    session.notify();
                    return;
                }
                Ok(Ok(None)) => QueueResult::Rejected,
                _ => QueueResult::Uncertain,
            }
        } else {
            queued
        };
        finish_rejected_reply(
            &state,
            &capability,
            &session,
            body,
            queued != QueueResult::Rejected,
        )
        .await;
    }));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueResult {
    Queued,
    Rejected,
    Uncertain,
    Unavailable,
}

fn quick_reply_input(
    capability: &ReplyCapability,
    body: &str,
) -> Option<peppy_desktop_api::quick_reply::QuickReplyInput> {
    let selection = capability.selection.as_ref()?;
    let (gateway_id, sim_id) = match selection.route.as_ref()? {
        GatewayRoute::Carrier {
            gateway_device_id,
            subscription_id,
        } => (gateway_device_id.to_string(), Some(subscription_id.clone())),
        GatewayRoute::RcsLink { gateway_device_id } => (gateway_device_id.to_string(), None),
    };
    Some(peppy_desktop_api::quick_reply::QuickReplyInput {
        command_id: capability.command_id.clone(),
        conversation_id: capability.conversation_id.clone(),
        body: body.into(),
        gateway_id,
        sim_id,
        expected_transport: selection.transport,
        expected_member_revision: selection.member_revision.clone(),
    })
}

async fn current_reply_session(
    state: &AppState,
    capability: &ReplyCapability,
) -> Option<Arc<crate::session::Session>> {
    state
        .session
        .lock()
        .await
        .as_ref()
        .filter(|session| reply_capability_current(state, capability, session))
        .cloned()
}

fn reply_capability_current(
    state: &AppState,
    capability: &ReplyCapability,
    session: &crate::session::Session,
) -> bool {
    require_unlocked(state).is_ok()
        && !capability.policy_cancel.is_cancelled()
        && !capability.generation_cancel.is_cancelled()
        && !capability.popup_cancel.owner.is_cancelled()
        && super::current(Some(session), &capability.session_generation)
        && super::unlocked(state, session)
        && state.notifications.quick_reply_enabled()
        && state.notification_activation.quick_reply_supported()
}

fn account_allows_reply(account: Option<&AccountAccessResult>) -> bool {
    match account {
        Some(AccountAccessResult::None | AccountAccessResult::Unsupported) => true,
        Some(AccountAccessResult::Notice { notice }) => matches!(
            notice.kind,
            AccountNoticeKindDto::PaymentFailedGrace | AccountNoticeKindDto::EndingScheduled
        ),
        Some(AccountAccessResult::TemporarilyUnavailable) | None => false,
    }
}

fn reconciliation_is_uncertain(error: &peppy_desktop_api::compose::ComposeError) -> bool {
    matches!(
        error,
        peppy_desktop_api::compose::ComposeError::Core(
            peppy_client_core::Error::Database | peppy_client_core::Error::ExistingCommandConflict
        )
    )
}

async fn finish_rejected_reply(
    state: &AppState,
    capability: &ReplyCapability,
    session: &Arc<crate::session::Session>,
    body: String,
    uncertain: bool,
) {
    let stored = {
        let guard = state.session.lock().await;
        let _effect = state
            .notification_activation
            .effect_gate
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if guard
            .as_deref()
            .is_none_or(|current| !reply_capability_current(state, capability, current))
        {
            return;
        }
        state.notification_activation.recovery.record(
            capability.reservation.clone(),
            RecoveryItem {
                id: capability.command_id.clone(),
                session_generation: capability.session_generation.clone(),
                conversation_id: capability.conversation_id.clone(),
                body,
                reason: if uncertain {
                    "checking-reply-status"
                } else if capability
                    .selection
                    .as_ref()
                    .is_none_or(|selection| selection.route.is_none())
                {
                    "needs-route-choice"
                } else {
                    "reply-not-queued"
                }
                .into(),
                state: if uncertain {
                    RecoveryState::Uncertain
                } else {
                    RecoveryState::Ready
                },
            },
        )
    };
    if !stored {
        return;
    }
    let app = capability.app.clone();
    let session = session.clone();
    let conversation_id = capability.conversation_id.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        super::post(
            &app,
            &session,
            Destination::Conversation { conversation_id },
            "Peppy".into(),
            if uncertain {
                "Checking reply status. Do not resend yet."
            } else {
                "Reply not queued. Open Peppy to review it."
            }
            .into(),
            None,
            None,
        )
    })
    .await;
    (state.notifier)();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_watcher_wakes_when_generation_was_cancelled_before_it_started() {
        let session = tokio_util::sync::CancellationToken::new();
        let policy = tokio_util::sync::CancellationToken::new();
        let generation = tokio_util::sync::CancellationToken::new();
        let owner = tokio_util::sync::CancellationToken::new();
        let stop = tokio_util::sync::CancellationToken::new();
        let (woke, wake) = std::sync::mpsc::sync_channel(1);
        generation.cancel();

        watch_popup_cancellation(
            session,
            policy,
            generation,
            owner.clone(),
            stop,
            Arc::new(move || {
                let _ = woke.try_send(());
            }),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), owner.cancelled())
            .await
            .expect("watcher must propagate cancellation");
        wake.recv_timeout(std::time::Duration::from_secs(1))
            .expect("watcher must wake owner after propagation");
    }

    fn gateway(id: &str, sim_id: &str, sms: bool, mms: bool) -> GatewayView {
        GatewayView {
            id: id.into(),
            name: id.into(),
            sim_id: sim_id.into(),
            online: true,
            simulated: false,
            supports_sms: sms,
            supports_mms: mms,
            availability: Default::default(),
            last_seen_age_seconds: None,
            presence: None,
            wake_route: false,
            mms_content_version: None,
            mms_max_bytes: None,
            mms_limit_source: None,
            mms_max_recipients: None,
            capability_note: None,
        }
    }

    #[test]
    fn saved_draft_route_wins_over_other_current_routes() {
        let selected = uuid::Uuid::new_v4().to_string();
        let route = GatewayRoute::Carrier {
            gateway_device_id: selected.parse().unwrap(),
            subscription_id: "saved-sim".into(),
        };
        let gateways = [
            gateway(&uuid::Uuid::new_v4().to_string(), "other", true, true),
            gateway(&selected, "saved-sim", true, true),
        ];

        assert_eq!(select_route(&gateways, true, Some(&route)), Some(route));
    }

    #[test]
    fn ambiguous_or_missing_current_route_is_not_selected() {
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        let gateways = [
            gateway(&first, "one", true, false),
            gateway(&second, "two", true, false),
        ];
        assert!(select_route(&gateways, true, None).is_none());
        assert!(select_route(&gateways, false, None).is_none());
    }

    #[test]
    fn reveal_lookup_keeps_exact_message_identity_after_banner_ack() {
        use peppy_client_core::{
            Client, ClientConfig, Cursor, DatabaseKey, DeviceId, IncomingSms, MessageId,
        };
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = crate::tests::open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        session
            .client
            .set_server_compaction_state(true, true)
            .unwrap();
        let phone = Client::open(
            ClientConfig {
                database_path: fixture.dir.path().join("reveal-phone.db"),
                vault_id: fixture.binding.vault_id.parse().unwrap(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        phone
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        phone.set_server_compaction_state(true, true).unwrap();
        let first = phone
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "body-a".into(),
                provider_message_id: Some("reveal-a".into()),
                imported: false,
            })
            .unwrap();
        phone
            .capture_incoming(IncomingSms {
                conversation_id: Some(first.conversation_id),
                sender_address: "+15555550100".into(),
                body: "body-b".into(),
                provider_message_id: Some("reveal-b".into()),
                imported: false,
            })
            .unwrap();
        for (index, envelope) in phone.pending_outbox().unwrap().iter().enumerate() {
            session
                .client
                .ingest(envelope, Cursor(index as u64 + 1))
                .unwrap();
        }
        session.client.apply_pending(100).unwrap();
        let candidates = session.client.pending_banner_candidates(10).unwrap();
        let candidate_a = candidates
            .iter()
            .find(|candidate| candidate.body == "body-a")
            .unwrap();
        session
            .client
            .ack_banner_candidates(vec![candidate_a.id.clone()])
            .unwrap();

        let revealed = lookup_reveal_message(
            &session,
            MessageId::from_str(&candidate_a.id).unwrap(),
            candidate_a.conversation_id.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(revealed.body, "body-a");
        assert!(lookup_reveal_message(
            &session,
            MessageId::new(),
            candidate_a.conversation_id.as_deref().unwrap(),
        )
        .is_none());
        assert!(lookup_reveal_message(
            &session,
            MessageId::from_str(&candidate_a.id).unwrap(),
            &uuid::Uuid::new_v4().to_string(),
        )
        .is_none());
    }

    #[test]
    fn account_warning_and_unsupported_allow_but_read_only_and_unavailable_refuse() {
        use peppy_desktop_api::account_access::{AccountNoticeDto, AccountNoticeSeverityDto};

        let notice = |kind| AccountAccessResult::Notice {
            notice: AccountNoticeDto {
                kind,
                severity: AccountNoticeSeverityDto::Warning,
                deadline: None,
                manage_url: None,
                dismissible: true,
                snooze_key: "test".into(),
            },
        };
        assert!(account_allows_reply(Some(
            &AccountAccessResult::Unsupported
        )));
        assert!(account_allows_reply(Some(&notice(
            AccountNoticeKindDto::PaymentFailedGrace
        ))));
        assert!(!account_allows_reply(Some(&notice(
            AccountNoticeKindDto::ReadOnlyPastDue
        ))));
        assert!(!account_allows_reply(Some(
            &AccountAccessResult::TemporarilyUnavailable
        )));
        assert!(!account_allows_reply(None));
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct PopupRequest {
    request_id: String,
    cancel: tokio_util::sync::CancellationToken,
    presentation: Presentation,
    sound: bool,
    banner: BannerPresentation,
    actions: PopupActions,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn submit_popup(
    app: &tauri::AppHandle,
    session: &crate::session::Session,
    request: PopupRequest,
) -> Result<(), PostError> {
    let PopupRequest {
        request_id,
        cancel,
        presentation,
        sound,
        banner,
        actions,
    } = request;
    let BannerPresentation {
        destination,
        title,
        body,
        icon,
        copy,
    } = banner;
    let state = app.state::<AppState>();
    // The owner owns presentation generations; its replacement API must retain this logical ID
    // when a Reveal action replaces a hidden banner.
    let guard = state.notification_activation.owner.lock().unwrap();
    let Some(owner) = guard.as_ref() else {
        return Err(PostError::Failed);
    };
    let post = super::owner::Post {
        activation: Some(peppy_desktop_api::NotificationActivation {
            request_id,
            session_generation: session.generation.clone(),
            destination,
        }),
        cancel,
        sound: sound && state.notifications.sound_enabled(),
        title,
        body,
        icon,
        copy,
        popup_actions: Some(actions),
    };
    let expected = presentation.lock().unwrap().clone();
    let acceptance = match expected {
        Some(expected) => owner.replace(expected, post),
        None => owner.submit(post),
    }?;
    let version = acceptance.version();
    drop(guard);
    acceptance.and_then(super::owner::Acceptance::wait)?;
    *presentation.lock().unwrap() = Some(version);
    Ok(())
}

#[tauri::command]
pub(crate) fn get_notification_quick_reply(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<QuickReplySettings> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    Ok(QuickReplySettings {
        enabled: state.notifications.quick_reply_enabled(),
        supported: state.notification_activation.quick_reply_supported(),
    })
}

#[tauri::command]
pub(crate) async fn set_notification_quick_reply(
    window: WebviewWindow,
    state: State<'_, AppState>,
    enabled: bool,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    require_unlocked(&state)?;
    if !state.notification_activation.quick_reply_supported() {
        return Err(BridgeError::new(
            "unsupported",
            "Quick reply is unavailable on this computer.",
        ));
    }
    let app = window.app_handle().clone();
    crate::blocking(move || {
        let state = app.state::<AppState>();
        let _effect = state
            .notification_activation
            .effect_gate
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        require_unlocked(&state)?;
        state.notifications.set_quick_reply_enabled(enabled)?;
        if !enabled {
            state
                .notification_activation
                .invalidate_quick_reply_under_gate();
        }
        Ok(())
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryItemView {
    id: String,
    conversation_id: String,
    body: String,
    reason: String,
    state: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveriesView {
    session_generation: String,
    items: Vec<RecoveryItemView>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RestoreInput {
    id: String,
    restore_id: String,
    session_generation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DiscardInput {
    id: String,
    session_generation: String,
}

fn recovery_session<'a>(
    state: &AppState,
    session: Option<&'a crate::session::Session>,
    generation: Option<&str>,
) -> BridgeResult<&'a crate::session::Session> {
    require_unlocked(state)?;
    let session = session.ok_or_else(BridgeError::no_session)?;
    let generation = generation.unwrap_or(&session.generation);
    if !super::current(Some(session), generation) || !super::unlocked(state, session) {
        return Err(BridgeError::new(
            "session-changed",
            "This reply is no longer available.",
        ));
    }
    Ok(session)
}

#[tauri::command]
pub(crate) async fn notification_reply_recoveries(
    window: WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
) -> BridgeResult<RecoveriesView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let guard = state.session.lock().await;
    let session = recovery_session(&state, guard.as_deref(), None)?;
    let items = state
        .notification_activation
        .recovery
        .list(&session.generation, &conversation_id)
        .into_iter()
        .map(|item| RecoveryItemView {
            id: item.id,
            conversation_id: item.conversation_id,
            body: item.body,
            reason: item.reason,
            state: match item.state {
                RecoveryState::Ready => "ready",
                RecoveryState::Claimed => "claimed",
                RecoveryState::Uncertain => "uncertain",
            },
        })
        .collect();
    require_unlocked(&state)?;
    Ok(RecoveriesView {
        session_generation: session.generation.clone(),
        items,
    })
}

#[tauri::command]
pub(crate) async fn claim_notification_reply_restore(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: RestoreInput,
) -> BridgeResult<bool> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let guard = state.session.lock().await;
    recovery_session(&state, guard.as_deref(), Some(&input.session_generation))?;
    Ok(state.notification_activation.recovery.claim(
        &input.session_generation,
        &input.id,
        &input.restore_id,
    ))
}

#[tauri::command]
pub(crate) async fn release_notification_reply_restore(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: RestoreInput,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let guard = state.session.lock().await;
    recovery_session(&state, guard.as_deref(), Some(&input.session_generation))?;
    if !state.notification_activation.recovery.release(
        &input.session_generation,
        &input.id,
        &input.restore_id,
    ) {
        return Err(BridgeError::new(
            "reply-claim",
            "This reply was not claimed by this operation.",
        ));
    }
    (state.notifier)();
    Ok(())
}

#[tauri::command]
pub(crate) async fn ack_notification_reply_restore(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: RestoreInput,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let guard = state.session.lock().await;
    recovery_session(&state, guard.as_deref(), Some(&input.session_generation))?;
    if !state.notification_activation.recovery.ack(
        &input.session_generation,
        &input.id,
        &input.restore_id,
    ) {
        return Err(BridgeError::new(
            "reply-claim",
            "This reply was not claimed by this operation.",
        ));
    }
    (state.notifier)();
    Ok(())
}

#[tauri::command]
pub(crate) async fn discard_notification_reply(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: DiscardInput,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let guard = state.session.lock().await;
    recovery_session(&state, guard.as_deref(), Some(&input.session_generation))?;
    state
        .notification_activation
        .recovery
        .discard(&input.session_generation, &input.id);
    (state.notifier)();
    Ok(())
}
