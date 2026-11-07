use super::*;
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
