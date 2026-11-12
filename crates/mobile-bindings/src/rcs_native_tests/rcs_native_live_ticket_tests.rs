use super::group_fixture::{GROUP_MEMBERS, group_fixture, group_member_digest};
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output};
use std::thread;
use std::time::{Duration, Instant};

const SNAPSHOT_FRAME_CAP: usize = 8 * 1024 * 1024;
const COMMIT_FRAME_CAP: usize = 131 * 1024;
const IPC_TIMEOUT: Duration = Duration::from_secs(15);
const IPC_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(serde::Deserialize)]
struct GoObservations {
    route: String,
    target: String,
    snapshot: String,
    now_ms: i64,
}

#[derive(serde::Deserialize)]
struct GoBeginRequest {
    observation: String,
    now_ms: i64,
}

#[derive(serde::Deserialize)]
struct GoSummary {
    summary: String,
}

#[derive(Debug)]
enum FrameReadError {
    Io,
    Eof,
    ExceedsCap,
    Decode,
}

fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> Result<T, FrameReadError> {
    let mut frame = Vec::new();
    loop {
        let buffer = reader.fill_buf().map_err(|_| FrameReadError::Io)?;
        if buffer.is_empty() {
            return Err(FrameReadError::Eof);
        }
        let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') else {
            if frame.len() + buffer.len() > max_bytes {
                return Err(FrameReadError::ExceedsCap);
            }
            frame.extend_from_slice(buffer);
            let consumed = buffer.len();
            reader.consume(consumed);
            continue;
        };
        if frame.len() + newline > max_bytes {
            return Err(FrameReadError::ExceedsCap);
        }
        frame.extend_from_slice(&buffer[..newline]);
        reader.consume(newline + 1);
        return serde_json::from_slice(&frame).map_err(|_| FrameReadError::Decode);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child already reaped")
    }

    fn wait_with_output(mut self) -> io::Result<Output> {
        self.0
            .take()
            .expect("child already reaped")
            .wait_with_output()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn accept_with_deadline(
    listener: &TcpListener,
    child: &mut Child,
    deadline: Instant,
) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() != ErrorKind::WouldBlock => return Err(error),
            Err(_) => {}
        }
        if child.try_wait()?.is_some() {
            return Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "Go contract test exited before connecting",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                "Go contract test did not connect before the deadline",
            ));
        }
        thread::sleep(IPC_POLL_INTERVAL);
    }
}

fn write_frame(stream: &mut TcpStream, value: &impl serde::Serialize) {
    serde_json::to_writer(&mut *stream, value).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
}

#[test]
#[ignore = "launches the tagged Go live-ticket contract test; invoke explicitly in CI"]
fn same_live_go_ticket_uses_real_rust_facts_and_commit() {
    let fixture =
        fixture_with_google_conversation_id_and_peer("conversation-local", "+12025550123");
    run_live_ticket(fixture);
}

#[test]
#[ignore = "launches the tagged Go live-ticket contract test; invoke explicitly in CI"]
fn carrier_live_go_ticket_uses_real_rust_facts_and_commit() {
    let fixture = carrier_fixture_with_google_conversation_id_and_peer_on_sim(
        "conversation-carrier",
        "+12025550123",
        "android-subscription-1",
        1,
    );
    assert_eq!(
        fixture.observation.selected_sim_id,
        "android-subscription-1"
    );
    run_live_ticket(fixture);
}

fn run_live_ticket(fixture: Fixture) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let go_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/android/googlemessages");
    let path = std::env::var_os("PATH").unwrap();
    let home = std::env::var_os("HOME").unwrap();
    let mut child = ChildGuard::new(
        Command::new("go")
            .args([
                "test",
                "-race",
                "-tags",
                "rcs_native_contract",
                "./gmlink",
                "-run",
                "^TestNativeRcsLiveCoreRoundTrip$",
                "-count=1",
                "-timeout",
                "60s",
            ])
            .current_dir(go_dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("PEPPY_RCS_NATIVE_LIVE_ADDR", address)
            .spawn()
            .unwrap(),
    );

    let stream =
        accept_with_deadline(&listener, child.child_mut(), Instant::now() + IPC_TIMEOUT).unwrap();
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(IPC_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IPC_TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    write_frame(
        &mut writer,
        &serde_json::json!({ "command_id": fixture.command_id }),
    );

    let observations: GoObservations = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    assert!(!observations.route.is_empty());
    assert!(!observations.target.is_empty());
    assert!(!observations.snapshot.is_empty());
    let observation: RcsMappingObservation = serde_json::from_str(&observations.target).unwrap();
    with_client(&fixture.gateway.client, |core| {
        assert!(matches!(
            core.verify_rcs_mapping(&observation, observations.now_ms)?,
            peppy_client_core::RcsMappingOutcome::Mapped(_)
        ));
        Ok(())
    })
    .unwrap();
    let preparation = gateway_prepare_native_rcs_send(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        observations.now_ms,
    )
    .unwrap();
    write_frame(
        &mut writer,
        &serde_json::json!({ "facts": preparation.facts_json().unwrap() }),
    );

    let begin: GoBeginRequest = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let committed = preparation.begin(begin.observation, begin.now_ms).unwrap();
    assert!(committed.commit_json().len() <= COMMIT_FRAME_CAP);
    write_frame(
        &mut writer,
        &serde_json::json!({ "commit": committed.commit_json() }),
    );
    let summary: GoSummary = read_frame(&mut reader, COMMIT_FRAME_CAP).unwrap();
    assert_eq!(summary.summary, "accepted");
    assert!(matches!(
        preparation.facts_json(),
        Err(MobileBindingsError::Conflict)
    ));

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Go contract test failed: {:?}",
        output
    );
}

#[test]
#[ignore = "launches the tagged Go live media-ticket contract test; invoke explicitly in CI"]
fn same_live_go_ticket_uses_real_rust_media_commit() {
    let fixture = one_to_one_media_fixture();
    run_live_media_ticket(
        fixture,
        "^TestNativeRcsLiveMediaCoreRoundTrip$",
        "caption first",
    );
}

#[test]
#[ignore = "launches the tagged Go live media-ticket contract test; invoke explicitly in CI"]
fn same_live_go_ticket_accepts_media_without_a_caption() {
    let fixture = one_to_one_media_fixture_with("", 1, "android-subscription-1", None);
    run_live_media_ticket(fixture, "^TestNativeRcsLiveMediaCoreRoundTrip$", "");
}

#[test]
fn one_command_sends_two_media_indexes_with_distinct_permits_and_only_one_caption() {
    let fixture =
        one_to_one_media_fixture_with("caption first", 2, "android-subscription-7", Some(1_000));
    let media_candidates =
        gateway_pending_rcs_media_commands(fixture.gateway.clone(), 1_000).unwrap();
    assert!(
        media_candidates
            .iter()
            .any(|candidate| candidate.command_id == fixture.command_id)
    );
    assert!(
        !gateway_pending_native_rcs_commands(fixture.gateway.clone())
            .unwrap()
            .iter()
            .any(|candidate| candidate.command_id == fixture.command_id)
    );

    let mut tmp_ids = Vec::new();
    let mut bodies = Vec::new();
    for expected_index in 0..2 {
        let attachment = gateway_next_rcs_media_attachment(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            1_000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(attachment.attachment_index, expected_index);
        gateway_mark_rcs_media_uploading(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            expected_index,
        )
        .unwrap();
        gateway_mark_rcs_media_reference_ready(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            expected_index,
            uploaded_media_reference(&attachment),
            1_000,
        )
        .unwrap();
        let preparation = gateway_issue_rcs_media_permit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            expected_index,
            1_000,
        )
        .unwrap();
        let committed = preparation
            .begin(serde_json::to_string(&fixture.observation).unwrap(), 1_000)
            .unwrap();
        let commit: serde_json::Value = serde_json::from_str(&committed.commit_json()).unwrap();
        tmp_ids.push(committed.tmp_id());
        bodies.push(commit["message_body"].as_str().unwrap().to_owned());
        assert_eq!(commit["media"]["key"].as_array().unwrap().len(), 32);
        gateway_record_rcs_media_commit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            expected_index,
        )
        .unwrap();
        gateway_record_rcs_media_outcome(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            expected_index,
            "ok".into(),
            1_001,
        )
        .unwrap();
    }

    assert_ne!(tmp_ids[0], tmp_ids[1]);
    assert_eq!(bodies, ["caption first", ""]);
    assert!(
        gateway_next_rcs_media_attachment(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            1_001,
        )
        .unwrap()
        .is_none()
    );
    assert!(
        !gateway_pending_rcs_media_commands(fixture.gateway.clone(), 1_001)
            .unwrap()
            .iter()
            .any(|candidate| candidate.command_id == fixture.command_id)
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    record_linked_gateway(&fixture.gateway, &fixture.gateway_device_id, now);
    with_client(&fixture.gateway.client, |core| {
        let conversation = fixture.conversation_id.parse().unwrap();
        let owner = core.begin_rcs_link_ownership(conversation, 0)?;
        let before = core.messages(conversation)?.len();
        let parent = core
            .messages(conversation)?
            .into_iter()
            .find(|message| message.payload.record.attachments.len() == 2)
            .unwrap()
            .payload
            .record
            .message_id;
        for (index, tmp_id) in tmp_ids.iter().enumerate() {
            let record = peppy_client_core::RcsProjectedRecord {
                google_message_id: format!("media-echo-{index}"),
                google_conversation_id: "conversation-local".into(),
                direction: peppy_client_core::Direction::Outgoing,
                body: bodies[index].clone(),
                sent_at_ms: Some(now + 1_000),
                tmp_id: Some(tmp_id.clone()),
                rcs_reported: true,
                e2ee_reported: None,
                alternate_message_ids: Vec::new(),
                attachments: vec![peppy_client_core::RcsIncomingAttachment {
                    original_index: 0,
                    media_type: "image/jpeg".into(),
                    display_name: "echo.jpg".into(),
                    plaintext_bytes: 12,
                    too_large: false,
                    media_ref: "private-echo-reference".into(),
                }],
            };
            let mut wrong = record.clone();
            wrong.google_conversation_id = "another-google-conversation".into();
            assert_eq!(
                core.ingest_rcs_projected_record(conversation, owner.ownership_generation, wrong)?,
                peppy_client_core::RcsReceiveOutcome::Rejected(
                    peppy_client_core::RcsReceiveCode::IdentityConflict
                )
            );
            assert_eq!(
                core.ingest_rcs_projected_record(
                    conversation,
                    owner.ownership_generation,
                    record.clone()
                )?,
                peppy_client_core::RcsReceiveOutcome::OutgoingMatched
            );
            assert_eq!(
                core.ingest_rcs_receive_observation(
                    conversation,
                    owner.ownership_generation,
                    record,
                    None,
                    peppy_client_core::RcsObservationProvenance::Historical
                )?,
                peppy_client_core::RcsReceiveOutcome::Duplicate
            );
            assert!(matches!(
                core.apply_rcs_projected_status(
                    conversation,
                    owner.ownership_generation,
                    peppy_client_core::RcsStatusApplication {
                        google_message_id: format!("media-echo-{index}"),
                        google_conversation_id: "conversation-local".into(),
                        tmp_id: None,
                        status: peppy_client_core::RcsProjectedStatus::Delivered,
                    }
                )?,
                peppy_client_core::RcsReceiveOutcome::StatusApplied
                    | peppy_client_core::RcsReceiveOutcome::StatusUnchanged
            ));
            if index == 0 {
                assert!(
                    core.rcs_message_status(parent)?.is_none(),
                    "one delivered slot must not mark the album delivered"
                );
            }
        }
        assert_eq!(
            core.rcs_message_status(parent)?.unwrap().status,
            peppy_client_core::RcsMessageStatus::Delivered
        );
        assert_eq!(core.messages(conversation)?.len(), before);
        assert!(core.pending_rcs_media_receive_tasks()?.is_empty());
        Ok(())
    })
    .unwrap();
}

#[test]
fn unknown_media_index_is_terminal_and_the_next_index_can_continue() {
    let fixture =
        one_to_one_media_fixture_with("caption first", 2, "android-subscription-7", Some(1_000));
    gateway_pending_rcs_media_commands(fixture.gateway.clone(), 1_000).unwrap();
    let first = gateway_next_rcs_media_attachment(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        1_000,
    )
    .unwrap()
    .unwrap();
    gateway_mark_rcs_media_uploading(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        first.attachment_index,
    )
    .unwrap();
    gateway_mark_rcs_media_reference_ready(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        first.attachment_index,
        uploaded_media_reference(&first),
        1_000,
    )
    .unwrap();
    gateway_issue_rcs_media_permit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        first.attachment_index,
        1_000,
    )
    .unwrap()
    .begin(serde_json::to_string(&fixture.observation).unwrap(), 1_000)
    .unwrap();
    gateway_record_rcs_media_outcome(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        first.attachment_index,
        "outcome_unknown".into(),
        1_001,
    )
    .unwrap();
    assert!(matches!(
        gateway_issue_rcs_media_permit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            first.attachment_index,
            1_001,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let next = gateway_next_rcs_media_attachment(fixture.gateway, fixture.command_id, 1_001)
        .unwrap()
        .unwrap();
    assert_eq!(next.attachment_index, 1);
}

#[test]
#[ignore = "launches the tagged Go group live-ticket contract test; invoke explicitly in CI"]
fn same_live_go_group_ticket_uses_real_rust_facts_and_commit() {
    run_live_group_ticket("^TestNativeRcsLiveGroupCoreRoundTrip$", "accepted");
}

#[test]
#[ignore = "launches the tagged Go group live media-ticket contract test; invoke explicitly in CI"]
fn same_live_go_group_ticket_uses_real_rust_media_commit() {
    let fixture = super::group_fixture::group_media_fixture(&GROUP_MEMBERS);
    run_live_group_media_ticket(fixture);
}

fn one_to_one_media_fixture() -> Fixture {
    one_to_one_media_fixture_with("caption first", 1, "android-subscription-1", None)
}

fn one_to_one_media_fixture_with(
    body: &str,
    attachment_count: usize,
    subscription_id: &str,
    linked_at_ms: Option<i64>,
) -> Fixture {
    one_to_one_media_fixture_with_type(
        body,
        attachment_count,
        subscription_id,
        linked_at_ms,
        "image/jpeg",
    )
}

fn one_to_one_media_fixture_with_type(
    body: &str,
    attachment_count: usize,
    subscription_id: &str,
    linked_at_ms: Option<i64>,
    mime: &str,
) -> Fixture {
    let mut fixture =
        fixture_with_google_conversation_id_and_peer("conversation-local", "+12025550123");
    let now_ms = linked_at_ms.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    });
    record_linked_gateway(&fixture.gateway, &fixture.gateway_device_id, now_ms);
    let attachments = (0..attachment_count)
        .map(|index| {
            let source = fixture
                ._temp_dir
                .path()
                .join(format!("one-to-one-media-{index}.jpg"));
            std::fs::write(&source, format!("one to one media fixture {index}")).unwrap();
            let attachment = with_client(&fixture.sender.client, |core| {
                let attachment = core.prepare_attachment(
                    &source,
                    mime,
                    &format!("one-to-one-media-{index}.jpg"),
                )?;
                core.mark_attachment_uploaded(
                    attachment.attachment_id,
                    &Uuid::new_v4().to_string(),
                )?;
                Ok(attachment)
            })
            .unwrap();
            (source, attachment)
        })
        .collect::<Vec<_>>();
    let queued = with_client(&fixture.sender.client, |core| {
        core.queue_mms(
            OutgoingMms {
                conversation_id: fixture.conversation_id.parse().unwrap(),
                recipients: vec!["+12025550123".into()],
                body: body.into(),
                attachment_ids: attachments
                    .iter()
                    .map(|(_, attachment)| attachment.attachment_id)
                    .collect(),
                subject: None,
            },
            GatewayRoute::Carrier {
                gateway_device_id: fixture.gateway_device_id.parse().unwrap(),
                subscription_id: subscription_id.into(),
            },
        )
    })
    .unwrap();
    fixture.command_id = queued.command_id.to_string();
    fixture.next_cursor = deliver_from(&fixture.sender, &fixture.gateway, fixture.next_cursor);
    for (_, attachment) in attachments {
        let downloaded = fixture
            ._temp_dir
            .path()
            .join(format!("downloaded-{}.cipher", attachment.attachment_id));
        install_sender_ciphertext_on_gateway(
            &fixture.sender,
            &fixture.gateway,
            attachment.attachment_id.to_string(),
            &downloaded,
        );
    }
    fixture
}

#[test]
fn heic_preflight_is_visible_idempotent_and_does_not_block_a_jpeg_command() {
    let mut fixture = one_to_one_media_fixture_with_type(
        "heic",
        1,
        "android-subscription-7",
        Some(1_000),
        "image/heic",
    );
    let source = fixture._temp_dir.path().join("valid.jpg");
    std::fs::write(&source, b"jpeg fixture").unwrap();
    let (attachment, queued) = with_client(&fixture.sender.client, |core| {
        let attachment = core.prepare_attachment(&source, "image/jpeg", "valid.jpg")?;
        core.mark_attachment_uploaded(attachment.attachment_id, &Uuid::new_v4().to_string())?;
        let queued = core.queue_mms(
            OutgoingMms {
                conversation_id: fixture.conversation_id.parse().unwrap(),
                recipients: vec!["+12025550123".into()],
                body: "jpeg".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            GatewayRoute::Carrier {
                gateway_device_id: fixture.gateway_device_id.parse().unwrap(),
                subscription_id: "android-subscription-7".into(),
            },
        )?;
        Ok((attachment, queued))
    })
    .unwrap();
    fixture.next_cursor = deliver_from(&fixture.sender, &fixture.gateway, fixture.next_cursor + 1);
    install_sender_ciphertext_on_gateway(
        &fixture.sender,
        &fixture.gateway,
        attachment.attachment_id.to_string(),
        &fixture._temp_dir.path().join("valid.cipher"),
    );
    for _ in 0..3 {
        let candidates =
            gateway_pending_rcs_media_commands(fixture.gateway.clone(), 1_000).unwrap();
        assert!(
            candidates
                .iter()
                .any(|item| item.command_id == queued.command_id.to_string())
        );
        assert!(
            !candidates
                .iter()
                .any(|item| item.command_id == fixture.command_id)
        );
    }
    with_client(&fixture.gateway.client, |core| {
        let message = core
            .messages(fixture.conversation_id.parse().unwrap())?
            .into_iter()
            .find(|message| message.payload.body == "heic")
            .unwrap();
        let status = core
            .rcs_media_outgoing_status(message.payload.record.message_id, 0)?
            .unwrap();
        assert_eq!(status.state, peppy_client_core::RcsMediaSyncState::NotSent);
        assert_eq!(
            status.reason.map(|reason| reason.code()),
            Some("not_allowed_mime")
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn expired_group_media_proof_remains_discoverable_for_native_reproof() {
    let fixture = super::group_fixture::group_media_fixture(&GROUP_MEMBERS);
    let observation = super::group_fixture::group_observation(&GROUP_MEMBERS, 1_000);
    assert!(fixture.advance_route(&observation, 1_000));
    assert_eq!(
        fixture
            .ingest(
                &super::group_fixture::group_snapshot(
                    super::group_fixture::GROUP_GOOGLE_ID,
                    &GROUP_MEMBERS,
                    1_000
                ),
                1_000
            )
            .linked,
        1
    );
    let candidates = gateway_pending_rcs_media_commands(fixture.gateway.clone(), 100_000).unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.command_id == fixture.command_id)
    );
}

#[test]
fn attachment_first_group_creation_uses_one_permit_for_all_outcomes() {
    for (code, expected) in [
        (
            "created_rcs_group",
            peppy_client_core::GroupCreateOutcome::Pending,
        ),
        ("not_rcs", peppy_client_core::GroupCreateOutcome::NotRcs),
        ("unknown", peppy_client_core::GroupCreateOutcome::Unknown),
    ] {
        let fixture = super::group_fixture::group_media_fixture(&GROUP_MEMBERS);
        with_client(&fixture.gateway.client, |core| {
            let command = fixture.command_id.parse().unwrap();
            assert!(core.pending_commands()?.iter().any(|item| item.command_id == command && !item.message.record.attachments.is_empty()));
            assert!(matches!(core.begin_rcs_group_create(fixture.conversation_id, command, 1_000)?, peppy_client_core::GroupCreateBegin::Permit(_)));
            assert_eq!(core.commit_rcs_group_create(command, &serde_json::json!({"code":code,"google_conversation_id":(code == "created_rcs_group").then_some(super::group_fixture::GROUP_GOOGLE_ID)}).to_string())?, expected);
            assert!(!matches!(core.begin_rcs_group_create(fixture.conversation_id, command, 1_001), Ok(peppy_client_core::GroupCreateBegin::Permit(_))));
            Ok(())
        }).unwrap();
    }
}

fn uploaded_media_reference(attachment: &NativeRcsMediaAttachment) -> String {
    serde_json::json!({
        "media_id": Uuid::new_v4().to_string(),
        "key": STANDARD.encode([7_u8; 32]),
        "mime": attachment.mime,
        "size": attachment.size,
        "name": attachment.name,
    })
    .to_string()
}

fn run_live_media_ticket(fixture: Fixture, go_test: &str, expected_body: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let go_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/android/googlemessages");
    let path = std::env::var_os("PATH").unwrap();
    let home = std::env::var_os("HOME").unwrap();
    let mut child = ChildGuard::new(
        Command::new("go")
            .args([
                "test",
                "-race",
                "-tags",
                "rcs_native_contract",
                "./gmlink",
                "-run",
                go_test,
                "-count=1",
                "-timeout",
                "60s",
            ])
            .current_dir(go_dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("PEPPY_RCS_NATIVE_LIVE_ADDR", address)
            .spawn()
            .unwrap(),
    );
    let stream =
        accept_with_deadline(&listener, child.child_mut(), Instant::now() + IPC_TIMEOUT).unwrap();
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(IPC_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IPC_TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    write_frame(
        &mut writer,
        &serde_json::json!({ "command_id": fixture.command_id }),
    );
    let observations: GoObservations = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let observation: RcsMappingObservation = serde_json::from_str(&observations.target).unwrap();
    let mapping = with_client(&fixture.gateway.client, |core| {
        core.verify_rcs_mapping(&observation, observations.now_ms)
    })
    .unwrap();
    assert!(matches!(
        mapping,
        peppy_client_core::RcsMappingOutcome::Mapped(_)
    ));
    let candidates =
        gateway_pending_rcs_media_commands(fixture.gateway.clone(), observations.now_ms).unwrap();
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.command_id.as_str())
        .collect::<Vec<_>>();
    assert!(
        candidate_ids.contains(&fixture.command_id.as_str()),
        "mapped media command {} absent from public candidates {candidate_ids:?}",
        fixture.command_id
    );
    let attachment = gateway_next_rcs_media_attachment(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        observations.now_ms,
    )
    .unwrap()
    .unwrap();
    gateway_mark_rcs_media_uploading(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
    )
    .unwrap();
    gateway_mark_rcs_media_reference_ready(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        uploaded_media_reference(&attachment),
        observations.now_ms,
    )
    .unwrap();
    let preparation = gateway_issue_rcs_media_permit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        observations.now_ms,
    )
    .unwrap();
    let duplicate_preparation = gateway_issue_rcs_media_permit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        observations.now_ms,
    )
    .unwrap();
    write_frame(
        &mut writer,
        &serde_json::json!({ "facts": preparation.facts_json().unwrap() }),
    );
    let begin: GoBeginRequest = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let committed = preparation
        .begin(begin.observation.clone(), begin.now_ms)
        .unwrap();
    assert!(matches!(
        duplicate_preparation.begin(begin.observation, begin.now_ms),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        with_client(&fixture.gateway.client, |core| core.begin_send_attempt_at(
            fixture.command_id.parse().unwrap(),
            begin.now_ms,
        )),
        Ok(PermitDecision::Blocked(
            PermitBlock::CarrierOnlyProofUnavailable
        ))
    ));
    let commit: serde_json::Value = serde_json::from_str(&committed.commit_json()).unwrap();
    assert_eq!(commit["message_body"], expected_body);
    assert_eq!(commit["media"]["key"].as_array().unwrap().len(), 32);
    gateway_record_rcs_media_commit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
    )
    .unwrap();
    assert!(matches!(
        gateway_record_rcs_media_commit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            attachment.attachment_index,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    write_frame(
        &mut writer,
        &serde_json::json!({ "commit": committed.commit_json() }),
    );
    let summary: GoSummary = read_frame(&mut reader, COMMIT_FRAME_CAP).unwrap();
    assert_eq!(summary.summary, "accepted");
    assert!(matches!(
        gateway_issue_rcs_media_permit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            attachment.attachment_index,
            begin.now_ms
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    gateway_record_rcs_media_outcome(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        if summary.summary == "accepted" {
            "ok".into()
        } else {
            summary.summary.clone()
        },
        begin.now_ms,
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Go media contract test failed: {:?}",
        output
    );
}

fn run_live_group_media_ticket(fixture: super::group_fixture::GroupFixture) {
    run_live_group_ticket_with_media(
        fixture,
        "^TestNativeRcsLiveGroupMediaCoreRoundTrip$",
        "accepted",
    );
}

fn run_live_group_ticket_with_media(
    fixture: super::group_fixture::GroupFixture,
    go_test: &str,
    expected_summary: &str,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let go_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/android/googlemessages");
    let path = std::env::var_os("PATH").unwrap();
    let home = std::env::var_os("HOME").unwrap();
    let mut child = ChildGuard::new(
        Command::new("go")
            .args([
                "test",
                "-race",
                "-tags",
                "rcs_native_contract",
                "./gmlink",
                "-run",
                go_test,
                "-count=1",
                "-timeout",
                "60s",
            ])
            .current_dir(go_dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("PEPPY_RCS_NATIVE_LIVE_ADDR", address)
            .spawn()
            .unwrap(),
    );
    let stream =
        accept_with_deadline(&listener, child.child_mut(), Instant::now() + IPC_TIMEOUT).unwrap();
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(IPC_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IPC_TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    write_frame(
        &mut writer,
        &serde_json::json!({ "command_id": fixture.command_id }),
    );
    let observations: GoObservations = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let route: RcsMappingObservation = serde_json::from_str(&observations.route).unwrap();
    let observation: RcsMappingObservation = serde_json::from_str(&observations.target).unwrap();
    assert!(fixture.advance_route(&route, observations.now_ms));
    assert_eq!(
        fixture
            .ingest(&observations.snapshot, observations.now_ms)
            .linked,
        1
    );
    assert_eq!(
        fixture.verify(&observation, observations.now_ms),
        NativeRcsGroupVerification::Proven
    );
    let candidates =
        gateway_pending_rcs_media_commands(fixture.gateway.clone(), observations.now_ms).unwrap();
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.command_id.as_str())
        .collect::<Vec<_>>();
    assert!(
        candidate_ids.contains(&fixture.command_id.as_str()),
        "proved group media command {} absent from public candidates {candidate_ids:?}",
        fixture.command_id
    );
    let attachment = gateway_next_rcs_media_attachment(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        observations.now_ms,
    )
    .unwrap()
    .unwrap();
    gateway_mark_rcs_media_uploading(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
    )
    .unwrap();
    gateway_mark_rcs_media_reference_ready(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        uploaded_media_reference(&attachment),
        observations.now_ms,
    )
    .unwrap();
    let preparation = gateway_issue_rcs_media_permit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        observations.now_ms,
    )
    .unwrap();
    let duplicate_preparation = gateway_issue_rcs_media_permit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        observations.now_ms,
    )
    .unwrap();
    write_frame(
        &mut writer,
        &serde_json::json!({ "facts": preparation.facts_json().unwrap() }),
    );
    let begin: GoBeginRequest = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let committed = preparation
        .begin(begin.observation.clone(), begin.now_ms)
        .unwrap();
    assert!(matches!(
        duplicate_preparation.begin(begin.observation, begin.now_ms),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let commit: serde_json::Value = serde_json::from_str(&committed.commit_json()).unwrap();
    assert_eq!(commit["message_body"], "hello group");
    assert_eq!(commit["target"]["class"], "Group");
    assert_eq!(commit["media"]["key"].as_array().unwrap().len(), 32);
    gateway_record_rcs_media_commit(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
    )
    .unwrap();
    assert!(matches!(
        gateway_record_rcs_media_commit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            attachment.attachment_index,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    write_frame(
        &mut writer,
        &serde_json::json!({ "commit": committed.commit_json() }),
    );
    let summary: GoSummary = read_frame(&mut reader, COMMIT_FRAME_CAP).unwrap();
    assert_eq!(summary.summary, expected_summary);
    assert!(matches!(
        gateway_issue_rcs_media_permit(
            fixture.gateway.clone(),
            fixture.command_id.clone(),
            attachment.attachment_index,
            begin.now_ms
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    gateway_record_rcs_media_outcome(
        fixture.gateway.clone(),
        fixture.command_id.clone(),
        attachment.attachment_index,
        if summary.summary == "accepted" {
            "ok".into()
        } else {
            summary.summary.clone()
        },
        begin.now_ms,
    )
    .unwrap();
    if expected_summary == "members_changed" {
        assert!(
            gateway_next_rcs_media_attachment(
                fixture.gateway.clone(),
                fixture.command_id.clone(),
                begin.now_ms,
            )
            .unwrap()
            .is_none()
        );
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Go group media contract test failed: {:?}",
        output
    );
}

#[test]
#[ignore = "launches the tagged Go group live-ticket contract test; invoke explicitly in CI"]
fn same_live_go_group_media_change_refuses_every_remaining_index() {
    let fixture = super::group_fixture::group_media_fixture_with_count(&GROUP_MEMBERS, 2);
    run_live_group_ticket_with_media(
        fixture,
        "^TestNativeRcsLiveGroupRosterChangedRoundTrip$",
        "members_changed",
    );
}

#[test]
#[ignore = "launches the tagged Go group live-ticket contract test; invoke explicitly in CI"]
fn same_live_go_group_ticket_refuses_a_changed_roster_before_dispatch() {
    run_live_group_ticket(
        "^TestNativeRcsLiveGroupRosterChangedRoundTrip$",
        "members_changed",
    );
}

/// Drives one Go-bound group ticket through Rust's group permit. Go owns the phone side and the
/// dispatch count; Rust owns the verification, facts, begin, and commit projection.
fn run_live_group_ticket(go_test: &str, expected_summary: &str) {
    let fixture = group_fixture(&GROUP_MEMBERS);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let go_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/android/googlemessages");
    let path = std::env::var_os("PATH").unwrap();
    let home = std::env::var_os("HOME").unwrap();
    let mut child = ChildGuard::new(
        Command::new("go")
            .args([
                "test",
                "-race",
                "-tags",
                "rcs_native_contract",
                "./gmlink",
                "-run",
                go_test,
                "-count=1",
                "-timeout",
                "60s",
            ])
            .current_dir(go_dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("PEPPY_RCS_NATIVE_LIVE_ADDR", address)
            .spawn()
            .unwrap(),
    );

    let stream =
        accept_with_deadline(&listener, child.child_mut(), Instant::now() + IPC_TIMEOUT).unwrap();
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(IPC_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IPC_TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    write_frame(
        &mut writer,
        &serde_json::json!({ "command_id": fixture.command_id }),
    );

    let observations: GoObservations = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    assert!(!observations.route.is_empty());
    assert!(!observations.snapshot.is_empty());
    let route: RcsMappingObservation = serde_json::from_str(&observations.route).unwrap();
    let observation: RcsMappingObservation = serde_json::from_str(&observations.target).unwrap();
    // The production order for a group-only link: Go's route advances the link context, then Go's
    // snapshot creates the proof. No 1:1 verification runs.
    assert!(fixture.advance_route(&route, observations.now_ms));
    let report = fixture.ingest(&observations.snapshot, observations.now_ms);
    assert_eq!(report.linked, 1);
    assert_eq!(
        fixture.verify(&observation, observations.now_ms),
        NativeRcsGroupVerification::Proven
    );
    let preparation = fixture.prepare(observations.now_ms);
    assert_eq!(
        preparation.expected_member_digest().as_deref(),
        Some(group_member_digest(&GROUP_MEMBERS).as_str()),
        "Rust's member digest must equal the digest Go sent"
    );
    write_frame(
        &mut writer,
        &serde_json::json!({ "facts": preparation.facts_json().unwrap() }),
    );

    let begin: GoBeginRequest = read_frame(&mut reader, SNAPSHOT_FRAME_CAP).unwrap();
    let committed = preparation.begin(begin.observation, begin.now_ms).unwrap();
    assert!(committed.commit_json().len() <= COMMIT_FRAME_CAP);
    write_frame(
        &mut writer,
        &serde_json::json!({ "commit": committed.commit_json() }),
    );
    let summary: GoSummary = read_frame(&mut reader, COMMIT_FRAME_CAP).unwrap();
    assert_eq!(summary.summary, expected_summary);
    assert!(matches!(
        preparation.facts_json(),
        Err(MobileBindingsError::Conflict)
    ));

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Go group contract test failed: {:?}",
        output
    );
}

#[test]
fn frame_reader_rejects_eof_before_a_frame() {
    let mut reader = BufReader::new(&b""[..]);
    assert!(matches!(
        read_frame::<serde_json::Value>(&mut reader, SNAPSHOT_FRAME_CAP),
        Err(FrameReadError::Eof)
    ));
}

#[test]
fn frame_reader_rejects_eof_mid_frame() {
    let mut reader = BufReader::new(&br#"{"partial":true}"#[..]);
    assert!(matches!(
        read_frame::<serde_json::Value>(&mut reader, SNAPSHOT_FRAME_CAP),
        Err(FrameReadError::Eof)
    ));
}

#[test]
fn accept_deadline_reaps_a_child_that_exits_without_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut child = ChildGuard::new(Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap());
    let error = accept_with_deadline(
        &listener,
        child.child_mut(),
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
}
