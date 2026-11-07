use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

const SNAPSHOT_FRAME_CAP: usize = 8 * 1024 * 1024;
const COMMIT_FRAME_CAP: usize = 131 * 1024;
const IPC_TIMEOUT: Duration = Duration::from_secs(15);

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

fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl BufRead, max_bytes: usize) -> T {
    let mut frame = Vec::new();
    loop {
        let buffer = reader.fill_buf().unwrap();
        let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') else {
            assert!(
                frame.len() + buffer.len() <= max_bytes,
                "IPC frame exceeds cap"
            );
            frame.extend_from_slice(buffer);
            let consumed = buffer.len();
            reader.consume(consumed);
            continue;
        };
        assert!(frame.len() + newline <= max_bytes, "IPC frame exceeds cap");
        frame.extend_from_slice(&buffer[..newline]);
        reader.consume(newline + 1);
        return serde_json::from_slice(&frame).unwrap();
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let go_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/android/googlemessages");
    let path = std::env::var_os("PATH").unwrap();
    let home = std::env::var_os("HOME").unwrap();
    let child = Command::new("go")
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
        .unwrap();

    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(IPC_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IPC_TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    write_frame(
        &mut writer,
        &serde_json::json!({ "command_id": fixture.command_id }),
    );

    let observations: GoObservations = read_frame(&mut reader, SNAPSHOT_FRAME_CAP);
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

    let begin: GoBeginRequest = read_frame(&mut reader, SNAPSHOT_FRAME_CAP);
    let committed = preparation.begin(begin.observation, begin.now_ms).unwrap();
    assert!(committed.commit_json().len() <= COMMIT_FRAME_CAP);
    write_frame(
        &mut writer,
        &serde_json::json!({ "commit": committed.commit_json() }),
    );
    let summary: GoSummary = read_frame(&mut reader, COMMIT_FRAME_CAP);
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
