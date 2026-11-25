//! Opt-in desktop diagnostics log, the counterpart of Android's `DiagnosticsLog`. Records are
//! numbers only (no identifiers, content or values derived from user data), bounded and validated; the writer
//! lives in `recorder.rs`. Other modules record through `record`, which costs one atomic load while
//! recording is off.

mod recorder;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use peppy_client_core::{DatabaseFault, DatabaseFaultKind, LiveTypingCause};
use serde::Serialize;
use tauri::Manager;

use crate::error::{BridgeError, BridgeResult};
use crate::sync::blocking;
use recorder::{DiagnosticsLog, Failure, StagedExport, Status, MAX_TOTAL_BYTES};

pub const EVENT_DIAGNOSTIC_STATE: u32 = 24;
pub const EVENT_LIVE_TYPING: u32 = 29;
pub const EVENT_LIVE_SOCKET: u32 = 30;
pub const EVENT_TYPING_SEND: u32 = 31;
pub const EVENT_TYPING_INBOUND: u32 = 32;
pub const EVENT_HEAD_TYPING: u32 = 33;
pub const EVENT_DATABASE_FAULT: u32 = 36;

pub const FIELD_DELAY_MS: u32 = 4;
pub const FIELD_ATTEMPT: u32 = 5;
pub const FIELD_COUNT: u32 = 19;
pub const FIELD_OUTCOME: u32 = 28;
pub const FIELD_REASON: u32 = 29;
pub const FIELD_DROPPED: u32 = 34;
pub const FIELD_ENABLED: u32 = 35;
pub const FIELD_SQLITE_CODE: u32 = 70;
pub const FIELD_SITE: u32 = 71;
pub const FIELD_LINE: u32 = 72;
pub const FIELD_AGE_MS: u32 = 73;

/// `reason` values of event 30 (`live_socket`).
pub mod live_socket_reason {
    pub const CONNECTING: i64 = 1;
    pub const HELLO_OK: i64 = 2;
    pub const HELLO_REJECTED: i64 = 3;
    pub const CONNECT_FAILED: i64 = 4;
    pub const CLOSED: i64 = 5;
    /// Carries `FIELD_DELAY_MS` and `FIELD_ATTEMPT`.
    pub const BACKOFF: i64 = 6;
    pub const CONTEXT_WAKE: i64 = 7;
    pub const TYPING_ENABLED: i64 = 8;
    pub const TYPING_NO_CONTEXT: i64 = 9;
    pub const TYPING_DISABLED: i64 = 10;
    pub const SUSPEND_DETECTED: i64 = 11;
}

/// `outcome` values of event 31 (`typing_send`).
pub mod typing_send_outcome {
    pub const SEALED: i64 = 1;
    pub const NOT_SEALED: i64 = 2;
    pub const NO_TARGET: i64 = 3;
    pub const TRANSMIT_REFUSED: i64 = 4;
    pub const NOT_CURRENT: i64 = 5;
}

/// `outcome` values of event 32 (`typing_inbound`).
pub mod typing_inbound_outcome {
    pub const DISPLAY_STARTED: i64 = 1;
    pub const DISPLAY_STOPPED: i64 = 2;
    pub const REPLY_SENT: i64 = 3;
    /// Carries `FIELD_COUNT`.
    pub const FOLLOW_UPS_SENT: i64 = 4;
    pub const NOTHING: i64 = 5;
    pub const NOT_CURRENT: i64 = 6;
    pub const REPLY_REFUSED: i64 = 7;
}

/// `outcome` values of event 33 (`head_typing`).
pub mod head_typing_outcome {
    pub const HIDDEN: i64 = 0;
    pub const SHOWN: i64 = 1;
}

const DIRECTORY: &str = "diagnostics";
const PREFERENCE_FILE: &str = "enabled";

static ACTIVE: OnceLock<DiagnosticsLog> = OnceLock::new();
static SESSION: OnceLock<u64> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Records seen by a test capture on this thread: the event code and its `(field, value)` pairs.
#[cfg(test)]
pub(crate) type Captured = Vec<(u32, Vec<(u32, i64)>)>;

#[cfg(test)]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Captured>> = const { std::cell::RefCell::new(None) };
}

/// Keeps this thread's capture on. Dropping it, including while a test panics, turns the capture off.
#[cfg(test)]
pub(crate) struct CaptureGuard;

#[cfg(test)]
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Turns recording on for the current thread only, so other tests see it off.
#[cfg(test)]
pub(crate) fn start_capture() -> CaptureGuard {
    CAPTURE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    CaptureGuard
}

/// Ends the capture and returns what it recorded.
#[cfg(test)]
pub(crate) fn finish_capture(guard: CaptureGuard) -> Captured {
    let records = CAPTURE
        .with(|slot| slot.borrow_mut().take())
        .unwrap_or_default();
    drop(guard);
    records
}

/// Whether recording is on. One atomic load when no log is installed or it is off.
pub fn enabled() -> bool {
    #[cfg(test)]
    if CAPTURE.with(|slot| slot.borrow().is_some()) {
        return true;
    }
    ACTIVE.get().is_some_and(DiagnosticsLog::enabled)
}

/// Records the last SQLite failure converted on this thread, if one was present.
pub fn record_database_fault(fault: Option<DatabaseFault>) {
    if !enabled() {
        return;
    }
    let Some(fault) = fault else {
        record(EVENT_DATABASE_FAULT, &[(FIELD_REASON, 0)]);
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    let mut fields = vec![(FIELD_REASON, fault.kind as i64)];
    if let Some(code) = fault.sqlite_extended_code {
        fields.push((FIELD_SQLITE_CODE, i64::from(code)));
    }
    fields.extend([
        (FIELD_SITE, i64::from(fault.site)),
        (FIELD_LINE, i64::from(fault.line)),
        (
            FIELD_AGE_MS,
            now.saturating_sub(fault.recorded_at_ms).max(0),
        ),
    ]);
    record(EVENT_DATABASE_FAULT, &fields);
}

/// Records `event` with `fields` (`(field code, value)` pairs). Returns immediately when recording
/// is off; never blocks or panics.
pub fn record(event: u32, fields: &[(u32, i64)]) {
    #[cfg(test)]
    if CAPTURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(records) = slot.as_mut() else {
            return false;
        };
        records.push((event, fields.to_vec()));
        true
    }) {
        return;
    }
    if let Some(log) = ACTIVE.get() {
        record_to(log, event, fields);
    }
}

fn record_to(log: &DiagnosticsLog, event: u32, fields: &[(u32, i64)]) {
    if !log.enabled() {
        return;
    }
    log.accept(format_record(event, session_token(), elapsed_ms(), fields));
}

fn format_record(event: u32, session: u64, timestamp_ms: u64, fields: &[(u32, i64)]) -> String {
    let mut line = format!("event={event} session={session} timestamp_ms={timestamp_ms} trace=0");
    for (code, value) in fields {
        let _ = write!(line, " {code}={value}");
    }
    line
}

/// Random per-process token: correlates records within one run and is included in exports.
fn session_token() -> u64 {
    *SESSION.get_or_init(|| (uuid::Uuid::new_v4().as_u128() as u64 >> 1).max(1))
}

/// Monotonic milliseconds since the first call (pinned to startup by `install`).
fn elapsed_ms() -> u64 {
    let started = *STARTED.get_or_init(Instant::now);
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Creates the recorder under the app's local data directory and starts its writer. Recording
/// stays off unless the persisted preference says otherwise. Creates no log files while off.
pub fn install(app: &tauri::AppHandle) {
    if ACTIVE.get().is_some() {
        return;
    }
    let Ok(local) = app.path().app_local_data_dir() else {
        return;
    };
    let directory = local.join(DIRECTORY);
    let _ = elapsed_ms();
    let log = DiagnosticsLog::new(directory.clone(), header());
    log.set_enabled(read_preference(&directory));
    log.spawn_worker();
    let _ = ACTIVE.set(log);
}

fn read_preference(directory: &Path) -> bool {
    fs::read(directory.join(PREFERENCE_FILE)).is_ok_and(|bytes| bytes == b"1")
}

fn write_preference(directory: &Path, enabled: bool) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let bytes: &[u8] = if enabled { b"1" } else { b"0" };
    crate::fsutil::write_private_atomic(&directory.join(PREFERENCE_FILE), bytes)
}

/// Header line of every export, written by the desktop app. Lists the events, fields and reason
/// tables that the Android header also uses, so both exports read the same way.
fn header() -> String {
    let mut header = format!(
        "schema=2 platform=desktop version={}",
        env!("CARGO_PKG_VERSION")
    );
    header.push_str(" events=24:diagnostic_state,29:live_typing(reason=cause,count=delta),30:live_socket,31:typing_send,32:typing_inbound,33:head_typing,36:database_fault(reason=kind)");
    header.push_str(
        " fields=4:delay_ms,5:attempt,19:count,28:outcome,29:reason,34:dropped,35:enabled,70:sqlite_code,71:site,72:line,73:age_ms",
    );
    header.push_str(" live_socket_reasons=1:connecting,2:hello_ok,3:hello_rejected,4:connect_failed,5:closed,6:backoff,7:context_wake,8:typing_enabled,9:typing_no_context,10:typing_disabled,11:suspend_detected");
    header.push_str(
        " typing_send_outcomes=1:sealed,2:not_sealed,3:no_target,4:transmit_refused,5:not_current",
    );
    header.push_str(" typing_inbound_outcomes=1:display_started,2:display_stopped,3:reply_sent,4:follow_ups_sent,5:nothing,6:not_current,7:reply_refused");
    header.push_str(" head_typing_outcomes=0:hidden,1:shown");
    append_core_cause_table(&mut header);
    header.push_str(" database_fault_kinds=0:none");
    for kind in DatabaseFaultKind::ALL {
        let _ = write!(header, ",{}:{}", kind as u16, kind.name());
    }
    header
}

/// Appends `live_typing_causes=code:name,...` in code order, from every cause the core exposes.
fn append_core_cause_table(header: &mut String) {
    header.push_str(" live_typing_causes=");
    for (index, cause) in LiveTypingCause::ALL.iter().enumerate() {
        if index > 0 {
            header.push(',');
        }
        let _ = write!(header, "{}:{}", *cause as u16, cause.name());
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn live_typing_causes_lists_every_core_cause_in_code_order() {
        let header = header();
        let listed = header
            .split_once(" live_typing_causes=")
            .map(|(_, rest)| rest.split(' ').next().unwrap_or_default())
            .expect("header carries the core cause table");
        let expected: Vec<String> = LiveTypingCause::ALL
            .iter()
            .map(|cause| format!("{}:{}", *cause as u16, cause.name()))
            .collect();
        assert_eq!(listed, expected.join(","));
        let codes: Vec<u16> = LiveTypingCause::ALL
            .iter()
            .map(|cause| *cause as u16)
            .collect();
        assert!(codes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(listed.starts_with("1:seal_started,2:seal_stopped,"));
    }

    #[test]
    fn fields_lists_exactly_the_field_codes_desktop_writes() {
        let header = header();
        let fields = header
            .split_once(" fields=")
            .map(|(_, rest)| rest.split(' ').next().unwrap_or_default())
            .expect("header carries the field table");
        assert_eq!(
            fields,
            "4:delay_ms,5:attempt,19:count,28:outcome,29:reason,34:dropped,35:enabled,70:sqlite_code,71:site,72:line,73:age_ms"
        );
    }

    #[test]
    fn database_fault_kinds_lists_every_core_kind_in_code_order() {
        let header = header();
        assert!(header.contains("36:database_fault(reason=kind)"));
        let listed = header
            .split_once(" database_fault_kinds=")
            .unwrap()
            .1
            .split(' ')
            .next()
            .unwrap();
        let expected = std::iter::once("0:none".to_owned())
            .chain(
                DatabaseFaultKind::ALL
                    .iter()
                    .map(|kind| format!("{}:{}", *kind as u16, kind.name())),
            )
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(listed, expected);
        assert_eq!(listed, "0:none,1:sqlite_failure,2:no_rows,3:decode,4:other");
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsState {
    pub enabled: bool,
    pub retained_bytes: u64,
    pub limit_bytes: u64,
    pub dropped_events: u64,
    pub storage_error: bool,
}

impl From<Status> for DiagnosticsState {
    fn from(status: Status) -> Self {
        Self {
            enabled: status.enabled,
            retained_bytes: status.retained_bytes,
            limit_bytes: MAX_TOTAL_BYTES,
            dropped_events: status.dropped,
            storage_error: status.storage_error,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum ExportOutcome {
    Saved,
    Cancelled,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExportResult {
    outcome: ExportOutcome,
}

fn active() -> BridgeResult<&'static DiagnosticsLog> {
    ACTIVE
        .get()
        .ok_or_else(|| BridgeError::new("diagnostics-unavailable", "Diagnostics is unavailable."))
}

fn storage_error() -> BridgeError {
    BridgeError::new(
        "diagnostics-storage",
        "The diagnostic log could not be written. Check free space and try again.",
    )
}

fn export_error(failure: Failure) -> BridgeError {
    match failure {
        Failure::Storage => storage_error(),
        Failure::Destination => BridgeError::new(
            "diagnostics-export",
            "The diagnostic log could not be saved to that location.",
        ),
    }
}

/// `peppy-diagnostics-YYYYMMDDTHHMMSSZ.log.txt`, in UTC.
fn export_file_name() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "peppy-diagnostics-{:04}{:02}{:02}T{:02}{:02}{:02}Z.log.txt",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Removes a staged snapshot the user did not save, off the async workers.
async fn discard_staged(log: &'static DiagnosticsLog, staged: StagedExport) {
    let _ = blocking(move || {
        log.discard_staged(staged);
        Ok(())
    })
    .await;
}

#[tauri::command]
pub async fn get_diagnostics_state(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::AppState>,
) -> BridgeResult<DiagnosticsState> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    Ok(active()?.status().into())
}

#[tauri::command]
pub async fn set_diagnostics_enabled(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::AppState>,
    enabled: bool,
) -> BridgeResult<DiagnosticsState> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let log = active()?;
    blocking(move || {
        write_preference(log.directory(), enabled).map_err(|_| storage_error())?;
        log.set_enabled(enabled);
        Ok(())
    })
    .await?;
    Ok(log.status().into())
}

#[tauri::command]
pub async fn clear_diagnostics_log(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::AppState>,
) -> BridgeResult<DiagnosticsState> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let log = active()?;
    blocking(move || log.clear().map_err(export_error)).await?;
    Ok(log.status().into())
}

/// Snapshots the log first, then asks for a destination. The webview never receives a path.
#[tauri::command]
pub async fn export_diagnostics_log(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> BridgeResult<DiagnosticsExportResult> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let log = active()?;
    // This export owns its snapshot until it is saved or discarded; other exports never touch it.
    let staged = blocking(move || log.stage_export().map_err(export_error)).await?;
    let destination =
        match crate::dialogs::save_file(&app, "Export diagnostic log", &export_file_name()).await {
            Ok(Some(destination)) => destination,
            Ok(None) => {
                discard_staged(log, staged).await;
                return Ok(DiagnosticsExportResult {
                    outcome: ExportOutcome::Cancelled,
                });
            }
            Err(error) => {
                discard_staged(log, staged).await;
                return Err(error);
            }
        };
    blocking(move || {
        log.persist_export(staged, &destination)
            .map_err(export_error)
    })
    .await?;
    Ok(DiagnosticsExportResult {
        outcome: ExportOutcome::Saved,
    })
}
