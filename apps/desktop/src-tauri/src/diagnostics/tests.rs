use std::fs;
use std::path::{Path, PathBuf};

use super::recorder::{
    beside, is_valid_record, DiagnosticsLog, Failure, CURRENT_FILE, MAX_FILE_BYTES,
    MAX_TOTAL_BYTES, PREVIOUS_FILE, STAGING_PREFIX, STAGING_SUFFIX,
};
use super::{
    format_record, live_socket_reason, record_to, DiagnosticsExportResult, DiagnosticsState,
    ExportOutcome, EVENT_LIVE_SOCKET, EVENT_LIVE_TYPING, FIELD_COUNT, FIELD_REASON,
};

const COMMANDS: [&str; 4] = [
    "get_diagnostics_state",
    "set_diagnostics_enabled",
    "clear_diagnostics_log",
    "export_diagnostics_log",
];

fn directory(root: &Path) -> PathBuf {
    root.join("diagnostics")
}

fn recording_log(root: &Path) -> DiagnosticsLog {
    recording_log_with_header(root, "schema=2 platform=desktop test".to_owned())
}

fn recording_log_with_header(root: &Path, header: String) -> DiagnosticsLog {
    let log = DiagnosticsLog::new(directory(root), header);
    log.set_enabled(true);
    log
}

fn valid(index: u64) -> String {
    format!("event=29 session=1 timestamp_ms={index} trace=0 29=1 19=1")
}

/// A valid record padded with fields to at most `size` bytes.
fn sized(index: u64, size: usize) -> String {
    let mut line = format!("event=29 session=1 timestamp_ms={index} trace=0");
    while line.len() + 5 <= size {
        line.push_str(" 4=11");
    }
    line
}

fn index_of(line: &str) -> u64 {
    line.split(' ')
        .find_map(|field| field.strip_prefix("timestamp_ms="))
        .unwrap()
        .parse()
        .unwrap()
}

fn lines_of(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The record lines of an exported snapshot, without its header and state record.
fn records_in(path: &Path) -> Vec<String> {
    lines_of(path).into_iter().skip(2).collect()
}

/// Staged export snapshots present in a log directory.
fn staged_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory(root)) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(STAGING_PREFIX) && name.ends_with(STAGING_SUFFIX)
                })
        })
        .collect()
}

/// Writes lines of about 500 bytes until the next one would exceed the file cap. Returns the next
/// index.
fn fill_to_cap(path: &Path, start: u64) -> u64 {
    let mut body = String::new();
    let mut index = start;
    loop {
        let line = sized(index, 500);
        if body.len() + line.len() + 1 > MAX_FILE_BYTES as usize {
            break;
        }
        body.push_str(&line);
        body.push('\n');
        index += 1;
    }
    fs::write(path, body).unwrap();
    index
}

#[test]
fn record_pattern_matches_the_shared_format_only() {
    for ok in [
        "event=29 session=1 timestamp_ms=0 trace=0",
        "event=24 session=0 timestamp_ms=0 trace=0 34=3 35=1",
        "event=30 session=9 timestamp_ms=12 trace=0 29=6 4=-5 5=2",
    ] {
        assert!(is_valid_record(ok), "{ok}");
    }
    for bad in [
        "",
        "event=29 session=1 timestamp_ms=0",
        "event=x session=1 timestamp_ms=0 trace=0",
        "event=29 session=1 timestamp_ms=0 trace=0 ",
        "event=29 session=1 timestamp_ms=0 trace=0  4=1",
        "event=29 session=1 timestamp_ms=0 trace=0 4=",
        "event=29 session=1 timestamp_ms=0 trace=0 4=--1",
        "event=29 session=-1 timestamp_ms=0 trace=0",
        "event=29 session=1 timestamp_ms=0 trace=0 4=1\n",
        "event=29 session=1 timestamp_ms=0 trace=0 4=1=2",
        "event=29 session=1 timestamp_ms=0 trace=0 Four=1",
    ] {
        assert!(!is_valid_record(bad), "{bad:?}");
    }
}

#[test]
fn format_record_builds_the_shared_line() {
    let line = format_record(
        EVENT_LIVE_TYPING,
        7,
        42,
        &[(FIELD_REASON, 3), (FIELD_COUNT, -1)],
    );
    assert_eq!(
        line,
        "event=29 session=7 timestamp_ms=42 trace=0 29=3 19=-1"
    );
    assert!(is_valid_record(&line));
}

#[test]
fn oversize_and_invalid_records_are_dropped_and_counted() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(sized(1, 600));
    log.accept("not a record".to_owned());
    assert_eq!(log.status().dropped, 2);
    assert_eq!(log.queued_records(), 0);
}

#[test]
fn rotation_keeps_each_file_at_the_cap_and_the_total_within_four_mebibytes() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    for chunk in 0..30u64 {
        for index in 0..200 {
            log.accept(sized(chunk * 200 + index, 500));
        }
        log.process_pending();
    }
    let directory = directory(temporary.path());
    let previous = fs::metadata(directory.join(PREVIOUS_FILE)).unwrap().len();
    let current = fs::metadata(directory.join(CURRENT_FILE)).unwrap().len();
    assert!(previous <= MAX_FILE_BYTES);
    assert!(
        previous > MAX_FILE_BYTES - 512,
        "rotates only when the next record would not fit"
    );
    assert!(current <= MAX_FILE_BYTES);
    let status = log.status();
    assert_eq!(status.retained_bytes, previous + current);
    assert!(status.retained_bytes <= MAX_TOTAL_BYTES);
    assert_eq!(status.dropped, 0);
    let newest = lines_of(&directory.join(CURRENT_FILE));
    assert!(newest.last().unwrap().contains("timestamp_ms=5999"));
}

#[test]
fn repair_drops_invalid_oversize_and_unterminated_records() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = directory(temporary.path());
    fs::create_dir_all(&directory).unwrap();
    let (first, second, third) = (valid(1), valid(2), valid(3));
    let previous = format!(
        "{first}\nnot a record\n{}\n{second}\nevent=29 ses",
        sized(4, 600)
    );
    fs::write(directory.join(PREVIOUS_FILE), &previous).unwrap();
    fs::write(directory.join(CURRENT_FILE), format!("{third}\n")).unwrap();

    let log = recording_log(temporary.path());
    log.process_pending();

    assert_eq!(
        fs::read_to_string(directory.join(PREVIOUS_FILE)).unwrap(),
        format!("{first}\n{second}\n")
    );
    assert_eq!(
        fs::read_to_string(directory.join(CURRENT_FILE)).unwrap(),
        format!("{third}\n")
    );
    let expected = (first.len() + second.len() + third.len() + 3) as u64;
    assert_eq!(log.status().retained_bytes, expected);
}

#[test]
fn repair_trims_an_over_cap_file_to_its_newest_records() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = directory(temporary.path());
    fs::create_dir_all(&directory).unwrap();
    let lines: Vec<String> = (0..4400).map(|index| sized(index, 500)).collect();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(directory.join(CURRENT_FILE), &body).unwrap();

    let log = recording_log(temporary.path());
    log.process_pending();

    let repaired = fs::read_to_string(directory.join(CURRENT_FILE)).unwrap();
    assert!(repaired.len() as u64 <= MAX_FILE_BYTES);
    let kept: Vec<&str> = repaired.lines().collect();
    assert_eq!(kept.last().copied(), lines.last().map(String::as_str));
    assert_ne!(kept.first().copied(), lines.first().map(String::as_str));
    assert_eq!(log.status().retained_bytes, repaired.len() as u64);
}

#[test]
fn records_accepted_before_a_disable_or_clear_are_discarded() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    for index in 0..3 {
        log.accept(valid(index));
    }
    log.set_enabled(false);
    log.process_pending();
    assert_eq!(log.status().dropped, 3);
    assert!(!directory(temporary.path()).join(CURRENT_FILE).exists());

    log.set_enabled(true);
    log.accept(valid(9));
    log.process_pending();
    let directory = directory(temporary.path());
    assert_eq!(lines_of(&directory.join(CURRENT_FILE)), vec![valid(9)]);

    log.accept(valid(10));
    log.accept(valid(11));
    assert_eq!(log.clear(), Ok(()));
    assert_eq!(log.status().dropped, 5);
    assert!(!directory.join(CURRENT_FILE).exists());
    assert!(log.status().enabled, "clear keeps recording on");
}

#[test]
fn a_batch_dequeued_before_a_disable_is_not_appended() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    for index in 0..3 {
        log.accept(valid(index));
    }
    // The barrier runs after the writer took the batch and before it appends.
    log.process_one_with_hook(|log| log.set_enabled(false));
    assert!(!directory(temporary.path()).join(CURRENT_FILE).exists());
    assert_eq!(log.status().dropped, 3);
    log.process_pending();
    assert_eq!(log.status().dropped, 3);
}

#[test]
fn clear_publishes_zero_retained_bytes_before_it_replies() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.spawn_worker();
    for index in 0..3 {
        log.accept(valid(index));
    }
    // The reply to a stage is the barrier: the records are on disk and their size is published.
    let staged = log.stage_export().unwrap();
    assert!(log.status().retained_bytes > 0);
    assert_eq!(log.clear(), Ok(()));
    assert_eq!(log.status().retained_bytes, 0);
    assert!(staged_files(temporary.path()).is_empty());
    let destination = temporary.path().join("late.log.txt");
    assert_eq!(
        log.persist_export(staged, &destination),
        Err(Failure::Storage)
    );
}

#[test]
fn a_full_queue_drops_new_records_and_counts_them() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    for index in 0..256 {
        log.accept(valid(index));
    }
    assert_eq!(log.queued_records(), 256);
    assert_eq!(log.status().dropped, 0);
    log.accept(valid(256));
    assert_eq!(log.status().dropped, 1);
    assert_eq!(log.queued_records(), 256);
    log.process_pending();
    assert_eq!(
        lines_of(&directory(temporary.path()).join(CURRENT_FILE)).len(),
        256
    );
}

#[test]
fn export_writes_the_header_state_record_and_records() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.accept("invalid".to_owned());
    log.accept(valid(2));
    let destination = temporary.path().join("out.log.txt");

    assert_eq!(log.export_to(&destination), Ok(()));

    let lines = lines_of(&destination);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("schema=2 platform=desktop"));
    assert_eq!(
        lines[1],
        "event=24 session=0 timestamp_ms=0 trace=0 34=1 35=1"
    );
    assert_eq!(lines[2], valid(1));
    assert_eq!(lines[3], valid(2));
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn export_at_the_cap_drops_the_oldest_whole_records_and_keeps_the_newest() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = directory(temporary.path());
    fs::create_dir_all(&directory).unwrap();
    let next = fill_to_cap(&directory.join(PREVIOUS_FILE), 0);
    let total = fill_to_cap(&directory.join(CURRENT_FILE), next);
    // A header this long pushes header + state + records past the 4 MiB cap.
    let header = format!("schema=2 platform=desktop test {}", "x".repeat(3000));
    let log = recording_log_with_header(temporary.path(), header.clone());
    let destination = temporary.path().join("out.log.txt");

    assert_eq!(log.export_to(&destination), Ok(()));

    let bytes = fs::read(&destination).unwrap();
    assert!(bytes.len() as u64 <= MAX_TOTAL_BYTES);
    assert!(
        bytes.len() as u64 > MAX_TOTAL_BYTES - 4096,
        "the snapshot fills the cap"
    );
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], header);
    assert!(lines[1].starts_with("event=24 "));
    let records = &lines[2..];
    assert!(records.iter().all(|line| is_valid_record(line)));
    let indices: Vec<u64> = records.iter().map(|line| index_of(line)).collect();
    assert!(indices[0] > 0, "the oldest records were dropped");
    assert_eq!(
        *indices.last().unwrap(),
        total - 1,
        "the newest record survived"
    );
    assert!(
        indices.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "only whole, consecutive records"
    );
}

#[test]
fn export_to_an_unwritable_destination_leaves_no_file_and_reports_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    let destination = temporary.path().join("missing").join("out.log.txt");

    assert_eq!(log.export_to(&destination), Err(Failure::Destination));

    assert!(!destination.exists());
    assert!(staged_files(temporary.path()).is_empty());
    assert!(
        !log.status().storage_error,
        "a destination failure is not a storage error"
    );
}

#[test]
fn export_accepts_a_220_character_destination_name() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    let destination = temporary
        .path()
        .join(format!("{}.log.txt", "d".repeat(220)));

    assert_eq!(log.export_to(&destination), Ok(()));
    assert!(destination.exists());
}

#[test]
fn cross_volume_temporaries_stay_within_the_file_name_limit() {
    let root = Path::new("/var/tmp/peppy-diagnostics-names");
    for name in [
        "n".repeat(220),
        "é".repeat(150),
        format!("{}.log.txt", "d".repeat(250)),
    ] {
        let destination = root.join(name);
        let temporary = beside(&destination);
        let file_name = temporary.file_name().unwrap().to_string_lossy();
        assert!(file_name.len() <= 255, "{} bytes", file_name.len());
        assert_eq!(temporary.parent(), destination.parent());
        assert!(file_name.starts_with('.') && file_name.ends_with(".tmp"));
    }
}

#[test]
fn a_failed_write_sets_storage_error_until_the_next_enable() {
    let temporary = tempfile::tempdir().unwrap();
    // A file where the directory should be makes every append fail.
    fs::write(directory(temporary.path()), b"blocked").unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    assert!(log.status().storage_error);

    fs::remove_file(directory(temporary.path())).unwrap();
    log.set_enabled(true);
    assert!(!log.status().storage_error);
}

#[test]
fn clear_removes_retained_logs_and_staged_export_and_keeps_recording_on() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let _staged = log.stage_export().unwrap();
    let directory = directory(temporary.path());
    assert_eq!(staged_files(temporary.path()).len(), 1);
    let dropped = log.status().dropped;

    assert_eq!(log.clear(), Ok(()));

    let status = log.status();
    assert_eq!(status.retained_bytes, 0);
    assert!(status.enabled);
    assert!(!status.storage_error);
    assert_eq!(status.dropped, dropped);
    assert!(!directory.join(CURRENT_FILE).exists());
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn out_of_order_completion_persists_each_exports_own_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let first = log.stage_export().unwrap();
    log.accept(valid(2));
    log.process_pending();
    let second = log.stage_export().unwrap();
    assert_eq!(staged_files(temporary.path()).len(), 2);

    // The second export finishes first.
    let second_destination = temporary.path().join("second.log.txt");
    assert_eq!(log.persist_export(second, &second_destination), Ok(()));
    assert_eq!(records_in(&second_destination), vec![valid(1), valid(2)]);

    let first_destination = temporary.path().join("first.log.txt");
    assert_eq!(log.persist_export(first, &first_destination), Ok(()));
    assert_eq!(records_in(&first_destination), vec![valid(1)]);
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn a_clear_between_stages_fails_the_older_save_and_leaves_the_newer_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let older = log.stage_export().unwrap();

    assert_eq!(log.clear(), Ok(()));
    log.accept(valid(2));
    log.process_pending();
    let newer = log.stage_export().unwrap();

    // The older save panel finishes after the clear: its snapshot is gone and must not be replaced.
    let older_destination = temporary.path().join("older.log.txt");
    assert_eq!(
        log.persist_export(older, &older_destination),
        Err(Failure::Storage)
    );
    assert!(!older_destination.exists());
    assert_eq!(
        staged_files(temporary.path()).len(),
        1,
        "the newer snapshot is untouched"
    );

    let newer_destination = temporary.path().join("newer.log.txt");
    assert_eq!(log.persist_export(newer, &newer_destination), Ok(()));
    assert_eq!(records_in(&newer_destination), vec![valid(2)]);
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn discarding_an_export_removes_only_its_own_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let cancelled = log.stage_export().unwrap();
    log.accept(valid(2));
    log.process_pending();
    let kept = log.stage_export().unwrap();

    log.discard_staged(cancelled);
    assert_eq!(staged_files(temporary.path()).len(), 1);

    let destination = temporary.path().join("kept.log.txt");
    assert_eq!(log.persist_export(kept, &destination), Ok(()));
    assert_eq!(records_in(&destination), vec![valid(1), valid(2)]);
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn discarding_a_cleared_export_leaves_a_newer_snapshot_alone() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let cleared = log.stage_export().unwrap();
    assert_eq!(log.clear(), Ok(()));
    log.accept(valid(2));
    log.process_pending();
    let newer = log.stage_export().unwrap();

    log.discard_staged(cleared);
    assert_eq!(staged_files(temporary.path()).len(), 1);
    let destination = temporary.path().join("newer.log.txt");
    assert_eq!(log.persist_export(newer, &destination), Ok(()));
    assert_eq!(records_in(&destination), vec![valid(2)]);
}

#[test]
fn dropping_an_unconsumed_handle_removes_only_its_own_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let abandoned = log.stage_export().unwrap();
    log.accept(valid(2));
    log.process_pending();
    let kept = log.stage_export().unwrap();
    assert_eq!(staged_files(temporary.path()).len(), 2);

    drop(abandoned);
    log.process_pending();
    assert_eq!(staged_files(temporary.path()).len(), 1);

    let destination = temporary.path().join("kept.log.txt");
    assert_eq!(log.persist_export(kept, &destination), Ok(()));
    assert_eq!(records_in(&destination), vec![valid(1), valid(2)]);
    assert!(staged_files(temporary.path()).is_empty());
}

#[test]
fn dropping_a_consumed_handle_queues_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let persisted = log.stage_export().unwrap();
    let destination = temporary.path().join("out.log.txt");
    assert_eq!(log.persist_export(persisted, &destination), Ok(()));
    let discarded = log.stage_export().unwrap();
    log.discard_staged(discarded);

    assert_eq!(
        log.queued_items(),
        0,
        "consumed handles leave no discard behind"
    );
    assert!(staged_files(temporary.path()).is_empty());
    assert_eq!(records_in(&destination), vec![valid(1)]);
}

#[test]
fn startup_removes_orphaned_snapshots_and_keeps_the_retained_logs() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = directory(temporary.path());
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("{STAGING_PREFIX}orphan{STAGING_SUFFIX}")),
        format!("{}\n", valid(7)),
    )
    .unwrap();
    fs::write(directory.join(CURRENT_FILE), format!("{}\n", valid(8))).unwrap();
    fs::write(directory.join(PREVIOUS_FILE), format!("{}\n", valid(9))).unwrap();
    // Recording is off: the startup pass still runs.
    let log = DiagnosticsLog::new(directory.clone(), "schema=2".to_owned());

    log.process_pending();

    assert!(staged_files(temporary.path()).is_empty());
    assert_eq!(lines_of(&directory.join(CURRENT_FILE)), vec![valid(8)]);
    assert_eq!(lines_of(&directory.join(PREVIOUS_FILE)), vec![valid(9)]);
}

#[test]
fn startup_creates_nothing_when_the_directory_is_absent() {
    let temporary = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::new(directory(temporary.path()), "schema=2".to_owned());
    log.process_pending();
    assert!(!directory(temporary.path()).exists());
}

#[test]
fn a_snapshot_whose_requester_is_gone_leaves_no_staged_file() {
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.stage_with_lost_requester();
    assert!(staged_files(temporary.path()).is_empty());
    assert_eq!(log.queued_items(), 0);
    assert!(log.status().dropped == 0 && !log.status().storage_error);
}

#[cfg(unix)]
#[test]
fn log_files_and_staged_snapshots_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let log = recording_log(temporary.path());
    log.accept(valid(1));
    log.process_pending();
    let _staged = log.stage_export().unwrap();
    let mut paths = staged_files(temporary.path());
    paths.push(directory(temporary.path()).join(CURRENT_FILE));
    for path in paths {
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
    }
}

#[test]
fn record_is_a_no_op_while_recording_is_off() {
    let temporary = tempfile::tempdir().unwrap();
    let log = DiagnosticsLog::new(directory(temporary.path()), "schema=2".to_owned());
    record_to(
        &log,
        EVENT_LIVE_SOCKET,
        &[(FIELD_REASON, live_socket_reason::CONNECTING)],
    );
    log.process_pending();
    assert_eq!(log.queued_records(), 0);
    assert_eq!(log.status().dropped, 0);
    assert!(!directory(temporary.path()).exists());
}

#[test]
fn state_and_export_results_serialize_to_the_contract_shapes() {
    let state = DiagnosticsState {
        enabled: true,
        retained_bytes: 10,
        limit_bytes: MAX_TOTAL_BYTES,
        dropped_events: 2,
        storage_error: false,
    };
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        serde_json::json!({
            "enabled": true,
            "retainedBytes": 10,
            "limitBytes": 4_194_304,
            "droppedEvents": 2,
            "storageError": false,
        })
    );
    let saved = DiagnosticsExportResult {
        outcome: ExportOutcome::Saved,
    };
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::json!({ "outcome": "saved" })
    );
}

#[test]
fn diagnostics_commands_are_declared_registered_and_granted_to_the_main_window_only() {
    const BUILD: &str = include_str!("../../build.rs");
    const MAIN: &str = include_str!("../../capabilities/default.json");
    const COMPOSER: &str = include_str!("../../capabilities/composer.json");
    const MEDIA: &str = include_str!("../../capabilities/media.json");
    const REGISTRY: &str = include_str!("../lib.rs");
    const SOURCE: &str = include_str!("../diagnostics.rs");
    for command in COMMANDS {
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert!(
            BUILD.contains(&format!("\"{command}\",")),
            "{command} is not declared"
        );
        assert!(MAIN.contains(&permission), "main lacks {command}");
        assert!(
            !COMPOSER.contains(&permission),
            "composer can call {command}"
        );
        assert!(!MEDIA.contains(&permission), "media can call {command}");
        assert!(
            REGISTRY.contains(&format!("diagnostics::{command},")),
            "{command} is not registered"
        );
        assert!(
            !REGISTRY.contains(&format!("\"{command}\"")),
            "{command} must not be allowed while locked"
        );
    }
    assert!(
        SOURCE
            .matches("crate::require_main(window.label())?;")
            .count()
            == COMMANDS.len()
    );
}
