//! Import benchmark: imports N real sealed records into a fresh client through the public calls a
//! host makes, and prints one `BENCH phase=<name> records=<n> ms=<ms> ...` line per phase.
//!
//! Ignored by default: it measures and checks correctness at scale, it does not gate timing.
//! Run in release mode, for example:
//! `PEPPY_BENCH_RECORDS=100000 cargo test --release --locked -p peppy-client-core --test integration snapshot_benchmark -- --ignored --nocapture --test-threads=1`
//!
//! Environment: `PEPPY_BENCH_RECORDS` (default 100000), `PEPPY_BENCH_APPLY_BATCH` (default 50, the
//! Android drain batch).
use crate::common::*;
use crate::snapshot_manifest::{entry, raw};
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEFAULT_RECORDS: usize = 100_000;
const DEFAULT_APPLY_BATCH: usize = 50;
/// Distinct senders, so the records spread over a few hundred conversations.
const CONVERSATIONS: usize = 300;
/// Captures uploaded per round while generating, so the source outbox never holds the whole vault.
const GENERATION_CHUNK: usize = 5_000;
/// A compaction-capable server: the import builds the authoritative projection, as hosts do.
const COMPACTION_GENERATION: u64 = 1;

#[derive(Clone, Copy)]
enum Mode {
    Full,
    Manifest,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Manifest => "manifest",
        }
    }
}

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a positive integer"))
    })
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn mean_ms(durations: &[Duration]) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    millis(durations.iter().sum::<Duration>()) / durations.len() as f64
}

fn timed<T>(call: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = call();
    (value, started.elapsed())
}

fn report(phase: &str, mode: Mode, records: usize, elapsed: Duration, extra: &str) {
    eprintln!(
        "BENCH phase={phase} records={records} ms={:.1} mode={} {extra}",
        millis(elapsed),
        mode.name()
    );
}

/// Per-call durations of one repeated host call.
#[derive(Default)]
struct Calls(Vec<Duration>);

impl Calls {
    fn total(&self) -> Duration {
        self.0.iter().sum()
    }

    fn summary(&self) -> String {
        let slowest = self.0.iter().copied().max().unwrap_or_default();
        format!(
            "calls={} mean_call_ms={:.2} max_call_ms={:.1}",
            self.0.len(),
            mean_ms(&self.0),
            millis(slowest)
        )
    }

    /// Mean of the first and last ten calls: growth with remaining rows shows as last > first.
    fn edges(&self) -> String {
        let len = self.0.len();
        format!(
            "first10_mean_ms={:.2} last10_mean_ms={:.2}",
            mean_ms(&self.0[..10.min(len)]),
            mean_ms(&self.0[len.saturating_sub(10)..])
        )
    }
}

/// Staging calls of one generation: `calls` holds every append; the split is for manifest runs.
#[derive(Default)]
struct Stage {
    calls: Calls,
    verify: Calls,
    download: Calls,
    encode: Duration,
}

impl Stage {
    fn push(&mut self, verify: bool, took: Duration) {
        self.calls.0.push(took);
        if verify {
            self.verify.0.push(took);
        } else {
            self.download.0.push(took);
        }
    }

    fn summary(&self) -> String {
        format!(
            "{} verify_calls={} download_calls={} encode_ms={:.1}",
            self.calls.summary(),
            self.verify.0.len(),
            self.download.0.len(),
            millis(self.encode)
        )
    }
}

fn bench_sms(index: usize) -> IncomingSms {
    let mut body = format!("bench message {index} ");
    let body_len = 100 + (index * 37) % 300;
    body.push_str(&"a".repeat(body_len.saturating_sub(body.len())));
    IncomingSms {
        conversation_id: None,
        sender_address: format!("+1555{:07}", index % CONVERSATIONS),
        body,
        provider_message_id: Some(format!("bench-{index}")),
        imported: false,
    }
}

/// Captures `records` SMS on a source device and drains its outbox into the vault log. Cursors
/// are 1-based positions in that log, exactly as the server assigns them.
fn generate(dir: &TempDir, vault: &Vault, records: usize) -> Vec<Envelope> {
    let cfg = config(dir, "bench-source", vault);
    let source = unlocked(&cfg, vault);
    source.set_server_origin_tags_supported(false).unwrap();
    let mut log = Vec::with_capacity(records);
    let mut captured = 0;
    while captured < records {
        let end = (captured + GENERATION_CHUNK).min(records);
        for index in captured..end {
            source.capture_incoming(bench_sms(index)).unwrap();
        }
        captured = end;
        loop {
            let batch = source.pending_outbox_batch(GENERATION_CHUNK).unwrap();
            if batch.is_empty() {
                break;
            }
            for envelope in batch {
                source.ack_outbox(envelope.envelope_id).unwrap();
                log.push(envelope);
            }
        }
    }
    assert!(
        log.len() >= records,
        "generation produced {} envelopes for {records} captures",
        log.len()
    );
    log.truncate(records);
    log
}

fn cursor_of(index: usize) -> Cursor {
    Cursor(index as u64 + 1)
}

/// Full import: the host downloads every record and sends pages of `MAX_SNAPSHOT_PAGE`.
fn stage_full(device: &Client, log: &[Envelope], generation: u64) -> Stage {
    let mut stage = Stage::default();
    for (page_index, page) in log.chunks(MAX_SNAPSHOT_PAGE).enumerate() {
        let first = page_index * MAX_SNAPSHOT_PAGE;
        let (records, encode) = timed(|| {
            page.iter()
                .enumerate()
                .map(|(offset, envelope)| RawSnapshotRecord {
                    cursor: cursor_of(first + offset),
                    envelope_json: serde_json::to_vec(envelope).unwrap(),
                })
                .collect::<Vec<_>>()
        });
        stage.encode += encode;
        let (result, took) = timed(|| device.append_snapshot_raw_page(generation, &records));
        result.unwrap();
        stage.push(false, took);
    }
    stage
}

/// Manifest import: every entry is missing (fresh device, empty journal), so every record is
/// downloaded in the run the manifest reports, as the host does.
fn stage_manifest(device: &Client, log: &[Envelope], generation: u64) -> Stage {
    let mut stage = Stage::default();
    let mut next = 0;
    while next < log.len() {
        let end = (next + MAX_SNAPSHOT_MANIFEST_PAGE).min(log.len());
        let entries: Vec<SnapshotManifestEntry> = log[next..end]
            .iter()
            .enumerate()
            .map(|(offset, envelope)| entry(cursor_of(next + offset).0, envelope))
            .collect();
        let (page, took) = timed(|| device.append_snapshot_manifest(generation, &entries));
        let page = page.unwrap();
        stage.push(true, took);
        // Records up to `last_cursor` are verified; the reported run starts right after them.
        let verified_through = page.progress.last_cursor.0 as usize;
        let missing = page.missing as usize;
        assert!(
            verified_through > next || missing > 0,
            "manifest page made no progress at {next}"
        );
        next = verified_through;
        let run_end = next + missing;
        for run_start in (next..run_end).step_by(MAX_SNAPSHOT_PAGE) {
            let run_stop = (run_start + MAX_SNAPSHOT_PAGE).min(run_end);
            let (records, encode) = timed(|| {
                log[run_start..run_stop]
                    .iter()
                    .enumerate()
                    .map(|(offset, envelope)| RawSnapshotRecord {
                        cursor: cursor_of(run_start + offset),
                        envelope_json: serde_json::to_vec(envelope).unwrap(),
                    })
                    .collect::<Vec<_>>()
            });
            stage.encode += encode;
            let (result, took) = timed(|| device.append_snapshot_raw_page(generation, &records));
            result.unwrap();
            stage.push(false, took);
        }
        next = run_end;
    }
    stage
}

fn projection_pending(device: &Client) -> bool {
    device
        .snapshot_projection_status()
        .unwrap()
        .is_some_and(|s| {
            matches!(
                s.state,
                SnapshotProjectionState::Draining | SnapshotProjectionState::Staging
            )
        })
}

fn file_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

fn count(config: &ClientConfig, table: &str) -> i64 {
    raw(config)
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// Opens a fresh device, runs every import step, prints the phase lines and checks the outcome.
fn run_import(mode: Mode, dir: &TempDir, vault: &Vault, log: &[Envelope], batch: usize) {
    let records = log.len();
    let high = records as u64;
    let cfg = config(dir, &format!("bench-device-{}", mode.name()), vault);

    let (device, took) = timed(|| unlocked(&cfg, vault));
    report("open", mode, records, took, "");

    let (progress, took) = timed(|| match mode {
        Mode::Full => device.begin_snapshot_with_compaction(
            Cursor(high),
            high,
            SnapshotPurpose::Resync,
            Some(COMPACTION_GENERATION),
        ),
        Mode::Manifest => {
            device.begin_snapshot_with_manifest(Cursor(high), high, Some(COMPACTION_GENERATION))
        }
    });
    let progress = progress.unwrap();
    report("begin", mode, records, took, "");

    let stage = match mode {
        Mode::Full => stage_full(&device, log, progress.generation),
        Mode::Manifest => stage_manifest(&device, log, progress.generation),
    };
    report(
        "stage",
        mode,
        records,
        stage.calls.total(),
        &stage.summary(),
    );

    let (report_result, took) = timed(|| device.finish_snapshot(progress.generation));
    report_result.unwrap();
    report("finish", mode, records, took, "");

    // Drain: each call moves up to `batch` retained records into the journal and applies up to
    // `batch` journal rows, as the Android host does while a snapshot remains.
    let mut drain = Calls::default();
    loop {
        let (step, took) = timed(|| device.apply_pending(batch));
        let step = step.unwrap();
        drain.0.push(took);
        if step.snapshot_remaining == 0 {
            break;
        }
        assert!(step.drained > 0, "drain made no progress");
    }
    report(
        "drain",
        mode,
        records,
        drain.total(),
        &format!("batch={batch} {} {}", drain.summary(), drain.edges()),
    );

    // Promotion: the projection leaves draining/staging inside `apply_pending` calls.
    let mut promote = Calls::default();
    let mut idle = 0;
    while projection_pending(&device) {
        let (step, took) = timed(|| device.apply_pending(batch));
        let step = step.unwrap();
        promote.0.push(took);
        if step.applied + step.drained + step.quarantined + step.superseded == 0 {
            idle += 1;
            assert!(
                idle < 3,
                "projection stalled: {:?}",
                device.snapshot_projection_status().unwrap()
            );
        } else {
            idle = 0;
        }
    }
    let projection = device.snapshot_projection_status().unwrap();
    report(
        "promote",
        mode,
        records,
        promote.total(),
        &format!(
            "batch={batch} {} state={:?}",
            promote.summary(),
            projection.as_ref().map(|p| p.state)
        ),
    );

    // Apply: the remaining journal rows, until a call makes no progress.
    let mut apply = Calls::default();
    loop {
        let (step, took) = timed(|| device.apply_pending(batch));
        let step = step.unwrap();
        apply.0.push(took);
        if step.applied + step.drained + step.quarantined + step.superseded == 0 {
            break;
        }
    }
    report(
        "apply",
        mode,
        records,
        apply.total(),
        &format!("batch={batch} {} {}", apply.summary(), apply.edges()),
    );

    let total = stage.calls.total() + drain.total() + promote.total() + apply.total();
    report("import_total", mode, records, total, "");

    let db_bytes = file_bytes(&cfg.database_path);
    let mut wal = cfg.database_path.as_os_str().to_owned();
    wal.push("-wal");
    let wal_bytes = file_bytes(Path::new(&wal));
    report(
        "size",
        mode,
        records,
        Duration::ZERO,
        &format!("db_bytes={db_bytes} wal_bytes={wal_bytes}"),
    );
    report(
        "counts",
        mode,
        records,
        Duration::ZERO,
        &format!(
            "journal={} messages={} snapshot_records={}",
            count(&cfg, "journal"),
            count(&cfg, "messages"),
            count(&cfg, "snapshot_records")
        ),
    );

    assert_eq!(
        count(&cfg, "journal"),
        records as i64,
        "every record journaled"
    );
    assert_eq!(count(&cfg, "snapshot_records"), 0, "nothing left staged");
    assert_eq!(device.receive_cursor().unwrap(), Cursor(high));
    assert!(device.snapshot_progress().unwrap().is_none());
    assert!(
        !matches!(&projection, Some(p) if p.state == SnapshotProjectionState::Failed),
        "projection failed: {projection:?}"
    );
}

fn run_benchmark(mode: Mode) {
    let records = env_count("PEPPY_BENCH_RECORDS", DEFAULT_RECORDS);
    let batch = env_count("PEPPY_BENCH_APPLY_BATCH", DEFAULT_APPLY_BATCH);
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (log, took) = timed(|| generate(&dir, &vault, records));
    report(
        "generate",
        mode,
        records,
        took,
        &format!("apply_batch={batch}"),
    );
    run_import(mode, &dir, &vault, &log, batch);
}

#[test]
// The 100k-record import takes minutes; run in release before changing snapshot import.
#[ignore = "benchmark: run in release with --ignored --nocapture"]
fn snapshot_benchmark_full_import() {
    run_benchmark(Mode::Full);
}

#[test]
// The 100k-record import takes minutes; run in release before changing iOS manifest resync.
#[ignore = "benchmark: run in release with --ignored --nocapture"]
fn snapshot_benchmark_manifest_import() {
    run_benchmark(Mode::Manifest);
}
