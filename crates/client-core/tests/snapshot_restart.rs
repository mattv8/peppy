//! Restart behaviour of staged snapshot rows (L5): verified reuse of the previous staging
//! generation, bounded `snapshot_records` growth across restarts, exemption rules for the reuse
//! source, query plans of the garbage-collection and reuse statements, and the restore path.
use crate::common::*;
use crate::snapshot_manifest::{
    all_entries, entry, plain_log, published, raw, raw_record, served_json, stage_manifest, staged,
    staged_count, untampered, verification_log,
};
use crate::snapshot_projection::{BOOK, settle, visible};
use rusqlite::{Connection, params};
use tempfile::TempDir;

/// Mirrors `SNAPSHOT_GC_BATCH` in the client: the rows one collection step may remove.
const GC_BATCH_ROWS: i64 = 2_000;
/// Restarts run by each bound test before its final, complete generation.
const RESTARTS: usize = 4;
const FLAGGED_TAG: [u8; 32] = [1; 32];
const LARGE_WIRE: usize = 2_048;
const SMALL_WIRE: usize = 16;
/// Ciphertext length of the fabricated envelopes in the bound tests.
const FABRICATED_CIPHERTEXT: usize = 64;

fn begin_manifest(client: &Client, records: usize) -> SnapshotProgress {
    client
        .begin_snapshot_with_manifest(Cursor(records as u64), records as u64, Some(2))
        .unwrap()
}

fn begin_cut(
    client: &Client,
    high: usize,
    count: usize,
    generation: Option<u64>,
) -> SnapshotProgress {
    client
        .begin_snapshot_with_manifest(Cursor(high as u64), count as u64, generation)
        .unwrap()
}

fn stage_prefix(client: &Client, generation: u64, log: &[Envelope], through: usize) {
    stage_raw(
        client,
        generation,
        log,
        &(1..=through as u64).collect::<Vec<_>>(),
    );
}

fn stage_raw(client: &Client, generation: u64, log: &[Envelope], cursors: &[u64]) {
    let records: Vec<RawSnapshotRecord> = cursors.iter().map(|&c| raw_record(c, log)).collect();
    client
        .append_snapshot_raw_page(generation, &records)
        .unwrap();
}

/// The records of `log` at `cursors`, as a retained set for `stage_manifest`.
fn cut(log: &[Envelope], cursors: &[u64]) -> Vec<SnapshotRecord> {
    cursors
        .iter()
        .map(|&c| SnapshotRecord {
            cursor: Cursor(c),
            envelope: log[c as usize - 1].clone(),
        })
        .collect()
}

/// Indexes of the envelopes whose headers the digest does not cover.
fn flagged_indexes(log: &[Envelope]) -> Vec<usize> {
    log.iter()
        .enumerate()
        .filter(|(_, e)| e.compaction.is_some() || e.origin_tag.is_some())
        .map(|(index, _)| index)
        .collect()
}

/// `count` distinct envelopes shaped like `template`, each with a `ciphertext_len`-byte body.
/// Digests are real (`wire_digest` covers AAD and ciphertext), so manifest entries verify.
fn fabricated_log(template: &Envelope, count: usize, ciphertext_len: usize) -> Vec<Envelope> {
    (0..count)
        .map(|index| {
            let mut envelope = template.clone();
            let mut ciphertext = vec![0; ciphertext_len.max(8)];
            ciphertext[..8].copy_from_slice(&(index as u64).to_be_bytes());
            envelope.ciphertext = ciphertext;
            envelope
        })
        .collect()
}

fn insert_journal(config: &ClientConfig, log: &[Envelope]) {
    let mut conn = raw(config);
    let tx = conn.transaction().unwrap();
    for (index, envelope) in log.iter().enumerate() {
        tx.execute(
            "INSERT INTO journal(cursor,wire,status) VALUES(?,?,'applied')",
            params![index as i64 + 1, serde_json::to_vec(envelope).unwrap()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn historical_rows(config: &ClientConfig) -> i64 {
    raw(config)
        .query_row("SELECT COUNT(*) FROM journal WHERE historical=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn assert_reuse_key_absent(config: &ClientConfig) {
    let count: i64 = raw(config)
        .query_row(
            "SELECT COUNT(*) FROM metadata WHERE k='snapshot_reuse_generation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no stale reuse key may survive");
}

fn assert_no_dead_generations(config: &ClientConfig) {
    let conn = raw(config);
    let generations: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM snapshot_generations WHERE state IN ('failed','obsolete')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM snapshot_records r JOIN snapshot_generations g ON g.generation=r.generation WHERE g.state IN ('failed','obsolete')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        (generations, rows),
        (0, 0),
        "dead generations must be collected"
    );
}

/// What a host serves for one cut: the manifest entry and the record bytes at each cursor.
struct Source {
    entries: Vec<SnapshotManifestEntry>,
    wires: Vec<Vec<u8>>,
}

impl Source {
    fn envelopes(log: &[Envelope]) -> Self {
        Self {
            entries: all_entries(log),
            wires: log
                .iter()
                .map(|envelope| serde_json::to_vec(envelope).unwrap())
                .collect(),
        }
    }

    /// Unparseable records of `len` bytes: they never verify, so every entry is downloaded.
    fn placeholders(count: usize, len: usize) -> Self {
        Self {
            entries: (1..=count as u64)
                .map(|cursor| SnapshotManifestEntry {
                    cursor: Cursor(cursor),
                    digest: [0; 32],
                    has_compaction: false,
                    has_origin_tag: false,
                })
                .collect(),
            wires: vec![vec![b'x'; len]; count],
        }
    }
}

/// Row and byte caps on `snapshot_records`, checked after every append. `generations` lists the
/// staging generations the scenario can hold at once: the largest generation and its reuse source
/// (pass the same cut twice). `slack_rows` is the garbage slack, with its widest wire's bytes.
struct Bound {
    conn: Connection,
    rows: i64,
    bytes: i64,
}

impl Bound {
    fn new(config: &ClientConfig, generations: &[&Source], slack_rows: i64) -> Self {
        let rows: i64 = generations.iter().map(|s| s.wires.len() as i64).sum();
        let bytes: i64 = generations
            .iter()
            .map(|s| s.wires.iter().map(Vec::len).sum::<usize>() as i64)
            .sum();
        let widest = generations
            .iter()
            .flat_map(|s| s.wires.iter().map(Vec::len))
            .max()
            .unwrap_or(0) as i64;
        Self {
            conn: raw(config),
            rows: rows + slack_rows,
            bytes: bytes + slack_rows * widest,
        }
    }

    fn check(&self, after: &str) {
        let (rows, bytes): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COUNT(*),COALESCE(SUM(LENGTH(wire)),0) FROM snapshot_records",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            rows <= self.rows,
            "after {after}: {rows} staged rows exceed the bound of {}",
            self.rows
        );
        assert!(
            bytes <= self.bytes,
            "after {after}: {bytes} staged bytes exceed the bound of {}",
            self.bytes
        );
    }
}

/// Drives one generation the way a host does: a manifest page, then raw downloads of each missing
/// run, stopping when its append budget runs out. Every append is checked against the bound.
struct Stager<'a> {
    client: &'a Client,
    generation: u64,
    source: &'a Source,
    bound: &'a Bound,
    appends_left: usize,
    downloaded: usize,
}

impl<'a> Stager<'a> {
    fn new(
        client: &'a Client,
        generation: u64,
        source: &'a Source,
        bound: &'a Bound,
        appends: usize,
    ) -> Self {
        Self {
            client,
            generation,
            source,
            bound,
            appends_left: appends,
            downloaded: 0,
        }
    }

    /// Returns the index of the first record the budget did not reach.
    fn stage_manifest(&mut self) -> usize {
        let total = self.source.entries.len();
        let mut next = 0;
        while self.appends_left > 0 && next < total {
            let end = (next + MAX_SNAPSHOT_MANIFEST_PAGE).min(total);
            let page = self
                .client
                .append_snapshot_manifest(self.generation, &self.source.entries[next..end])
                .unwrap();
            self.spend("manifest page");
            next = page.progress.last_cursor.0 as usize;
            let missing_end = (next + page.missing as usize).min(total);
            next = self.download(next, missing_end);
        }
        next
    }

    /// Downloads every record in raw pages, as a full (non-manifest) import does.
    fn download_all(&mut self) -> usize {
        self.download(0, self.source.wires.len())
    }

    fn download(&mut self, mut next: usize, end: usize) -> usize {
        while next < end && self.appends_left > 0 {
            let stop = (next + MAX_SNAPSHOT_PAGE).min(end);
            let records: Vec<RawSnapshotRecord> = (next..stop)
                .map(|index| RawSnapshotRecord {
                    cursor: Cursor(index as u64 + 1),
                    envelope_json: self.source.wires[index].clone(),
                })
                .collect();
            self.client
                .append_snapshot_raw_page(self.generation, &records)
                .unwrap();
            self.downloaded += stop - next;
            self.spend("raw page");
            next = stop;
        }
        next
    }

    fn spend(&mut self, after: &str) {
        self.appends_left -= 1;
        self.bound.check(after);
    }
}

#[test]
fn manifest_restart_reuses_matching_rows_and_downloads_only_changed_entries() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "reuse", &vault);
    let client = unlocked(&config, &vault);
    let log = plain_log(&dir, &vault, 4);
    let first = begin_manifest(&client, log.len());
    stage_prefix(&client, first.generation, &log, 3);

    let second = begin_manifest(&client, log.len());
    let page = client
        .append_snapshot_manifest(second.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(page.missing, 1);
    assert_eq!(page.progress.received_records, 3);
    assert_eq!(staged_count(&config, first.generation), 0);
    client
        .append_snapshot_raw_page(second.generation, &[raw_record(4, &log)])
        .unwrap();
    client.finish_snapshot(second.generation).unwrap();
    while client
        .apply_pending(MAX_APPLY_BATCH)
        .unwrap()
        .snapshot_remaining
        > 0
    {}
    assert_eq!(
        raw(&config)
            .query_row("SELECT COUNT(*) FROM journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn tampered_or_flagged_reuse_rows_are_downloaded() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "tampered", &vault);
    let client = unlocked(&config, &vault);
    let log = plain_log(&dir, &vault, 2);
    let first = begin_manifest(&client, 2);
    stage_prefix(&client, first.generation, &log, 2);
    raw(&config)
        .execute(
            "UPDATE snapshot_records SET wire=x'00' WHERE generation=? AND cursor=1",
            params![first.generation as i64],
        )
        .unwrap();
    let second = begin_manifest(&client, 2);
    let mut entries = all_entries(&log);
    entries[1].has_origin_tag = true;
    assert_eq!(
        client
            .append_snapshot_manifest(second.generation, &entries)
            .unwrap()
            .missing,
        2
    );
}

#[test]
fn reuse_source_is_collected_after_successor_fails_or_full_begin() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "cleanup", &vault);
    let client = unlocked(&config, &vault);
    let log = plain_log(&dir, &vault, 2);
    let first = begin_manifest(&client, 2);
    stage_prefix(&client, first.generation, &log, 2);
    let second = begin_manifest(&client, 2);
    assert_eq!(
        client.append_snapshot_manifest(
            second.generation,
            &[SnapshotManifestEntry {
                cursor: Cursor(3),
                digest: [0; 32],
                has_compaction: false,
                has_origin_tag: false,
            }]
        ),
        Err(Error::SnapshotMismatch)
    );
    let full = client
        .begin_snapshot(Cursor(2), 2, SnapshotPurpose::Resync)
        .unwrap();
    client
        .append_snapshot_raw_page(full.generation, &[raw_record(1, &log), raw_record(2, &log)])
        .unwrap();
    assert_eq!(
        raw(&config)
            .query_row(
                "SELECT COUNT(*) FROM snapshot_records WHERE generation<>?",
                params![full.generation as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_reuse_key_absent(&config);
}

#[test]
fn restart_with_newer_cut_reuses_verified_rows_and_matches_a_clean_manifest_import() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let mut log = plain_log(&dir, &vault, 9);
    // Cursor 4 carries an origin tag, which the digest does not cover: it is never verified.
    log[3].origin_tag = Some(FLAGGED_TAG);
    let clean_cfg = config(&dir, "clean", &vault);
    let restarted_cfg = config(&dir, "restarted", &vault);
    let clean = unlocked(&clean_cfg, &vault);
    let restarted = unlocked(&restarted_cfg, &vault);

    // The older cut R1 (high 6): the device stages 1, 2, 3 and 5, then restarts mid-staging.
    let older = begin_cut(&restarted, 6, 6, Some(1));
    stage_raw(&restarted, older.generation, &log, &[1, 2, 3, 5]);
    // The newer cut R2 (high 9, compaction generation 2): record 2 was compacted away and 6..9
    // were added after R1. Records 1, 3 and 5 are byte-identical to the stored rows.
    let newer = cut(&log, &[1, 3, 4, 5, 6, 7, 8, 9]);
    let (restarted_generation, restarted_runs) =
        stage_manifest(&restarted, &newer, 9, Some(2), &untampered, &served_json).unwrap();
    // An abandoned older generation gives the clean device the same generation number (2) as the
    // restarted device, so the published projection records compare equal.
    begin_cut(&clean, 6, 6, Some(1));
    let (clean_generation, clean_runs) =
        stage_manifest(&clean, &newer, 9, Some(2), &untampered, &served_json).unwrap();

    // Record 4 is flagged, so the first missing run is [4]. Record 5 verifies by its reuse row and
    // ends that run. Then 6..9 are unstaged and downloaded as one run. A clean device downloads all
    // eight records in one run.
    assert_eq!(restarted_runs, vec![1, 4]);
    assert_eq!(clean_runs, vec![8]);
    assert_eq!(
        restarted_runs.iter().sum::<u32>(),
        5,
        "record 4 and records 6..9"
    );
    assert_eq!(staged_count(&restarted_cfg, older.generation), 0);
    assert_eq!(restarted_generation, clean_generation);
    assert_eq!(
        staged(&restarted_cfg, restarted_generation),
        staged(&clean_cfg, clean_generation),
        "staging before publish must be identical"
    );

    let restarted_report = restarted.finish_snapshot(restarted_generation).unwrap();
    let clean_report = clean.finish_snapshot(clean_generation).unwrap();
    // Reused rows are not overlap: they are journaled on drain like downloaded rows.
    assert_eq!(
        (restarted_report.journaled, restarted_report.duplicate),
        (8, 0)
    );
    assert_eq!((clean_report.journaled, clean_report.duplicate), (8, 0));
    settle(&restarted);
    settle(&clean);
    assert_eq!(
        published(&restarted_cfg, &restarted),
        published(&clean_cfg, &clean),
        "published state must match the clean import"
    );
    assert_eq!(historical_rows(&restarted_cfg), 8);
    assert_eq!(historical_rows(&clean_cfg), 8);
    assert_eq!(visible(&restarted, BOOK), visible(&clean, BOOK));
    assert_eq!(restarted.receive_cursor().unwrap(), Cursor(9));
}

#[test]
fn tampered_reuse_row_is_downloaded_and_the_stale_row_is_removed_by_the_append() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "tampered-digest", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 2, 2, Some(1));
    stage_prefix(&client, older.generation, &log, 2);
    // Still parseable, but the digest of its ciphertext no longer matches the entry.
    let mut altered = log[0].clone();
    altered.ciphertext[0] ^= 1;
    raw(&cfg)
        .execute(
            "UPDATE snapshot_records SET wire=? WHERE generation=? AND cursor=1",
            params![
                serde_json::to_vec(&altered).unwrap(),
                older.generation as i64
            ],
        )
        .unwrap();

    let newer = begin_cut(&client, 2, 2, Some(2));
    let page = client
        .append_snapshot_manifest(newer.generation, &all_entries(&log))
        .unwrap();
    assert_eq!((page.missing, page.progress.received_records), (1, 0));
    assert_eq!(
        raw(&cfg)
            .query_row(
                "SELECT COUNT(*) FROM snapshot_records WHERE generation=? AND cursor=1",
                params![older.generation as i64],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "the stale row stays until its cursor is passed"
    );

    stage_raw(&client, newer.generation, &log, &[1]);
    assert_eq!(
        raw(&cfg)
            .query_row(
                "SELECT COUNT(*) FROM snapshot_records WHERE generation=? AND cursor=1",
                params![older.generation as i64],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "the append removes the stale row at or below the new last cursor"
    );
    let page = client
        .append_snapshot_manifest(newer.generation, &all_entries(&log)[1..])
        .unwrap();
    assert_eq!(page.missing, 0);
    assert_eq!(
        staged(&cfg, newer.generation).rows,
        vec![(1, served_json(1, &log[0])), (2, served_json(2, &log[1]))]
    );
    client.finish_snapshot(newer.generation).unwrap();
}

#[test]
fn flagged_entries_are_never_reused_even_when_the_stored_row_is_identical() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let cfg = config(&dir, "flagged-entries", &vault);
    let client = unlocked(&cfg, &vault);
    let flagged = flagged_indexes(&log);
    assert!(
        flagged.len() >= 2,
        "fixture has an origin-tagged and a compacted record"
    );
    for index in flagged {
        let cursor = index as u64 + 1;
        let older = begin_cut(&client, log.len(), 1, Some(1));
        stage_raw(&client, older.generation, &log, &[cursor]);
        let newer = begin_cut(&client, log.len(), 1, Some(2));
        let page = client
            .append_snapshot_manifest(newer.generation, &[entry(cursor, &log[index])])
            .unwrap();
        assert_eq!((page.missing, page.progress.received_records), (1, 0));
        assert_eq!(staged_count(&cfg, newer.generation), 0);
    }
}

#[test]
fn stored_rows_carrying_a_flag_are_never_reused_for_an_unflagged_entry() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let cfg = config(&dir, "flagged-rows", &vault);
    let client = unlocked(&cfg, &vault);
    for index in flagged_indexes(&log) {
        let cursor = index as u64 + 1;
        let older = begin_cut(&client, log.len(), 1, Some(1));
        stage_raw(&client, older.generation, &log, &[cursor]);
        let newer = begin_cut(&client, log.len(), 1, Some(2));
        // The digest does not cover the flag, so this entry would verify if the flag were ignored.
        let unflagged = SnapshotManifestEntry {
            cursor: Cursor(cursor),
            digest: log[index].wire_digest().unwrap(),
            has_compaction: false,
            has_origin_tag: false,
        };
        let page = client
            .append_snapshot_manifest(newer.generation, &[unflagged])
            .unwrap();
        assert_eq!((page.missing, page.progress.received_records), (1, 0));
        assert_eq!(staged_count(&cfg, newer.generation), 0);
    }
}

#[test]
fn manifest_entry_above_the_new_high_water_fails_without_moving_reuse_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "above-high", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 3, 3, Some(1));
    stage_prefix(&client, older.generation, &log, 3);
    // The new cut ends at 2, so the cursor-3 entry is one it no longer contains.
    let newer = begin_cut(&client, 2, 2, Some(2));
    assert_eq!(
        client
            .append_snapshot_manifest(newer.generation, &all_entries(&log))
            .map(|_| ()),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(
        staged_count(&cfg, older.generation),
        3,
        "a rejected page rolls back its moves"
    );
    assert_eq!(staged_count(&cfg, newer.generation), 0);
    assert_reuse_key_absent(&cfg);
}

#[test]
fn manifest_entries_repeating_a_cursor_fail_without_moving_twice() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "duplicate-cursor", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 2, 2, Some(1));
    stage_prefix(&client, older.generation, &log, 1);
    let newer = begin_cut(&client, 2, 2, Some(2));
    assert_eq!(
        client
            .append_snapshot_manifest(newer.generation, &[entry(1, &log[0]), entry(1, &log[0])])
            .map(|_| ()),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(staged_count(&cfg, older.generation), 1);
    assert_eq!(staged_count(&cfg, newer.generation), 0);
}

#[test]
fn journal_verified_manifest_pages_keep_staged_rows_bounded_across_restarts() {
    const RECORDS: usize = 10_000;
    const PAGES_PER_RESTART: usize = 1;
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let template = plain_log(&dir, &vault, 1).remove(0);
    let cfg = config(&dir, "bounded-journal", &vault);
    let device = unlocked(&cfg, &vault);
    let log = fabricated_log(&template, RECORDS, FABRICATED_CIPHERTEXT);
    insert_journal(&cfg, &log);
    let source = Source::envelopes(&log);
    let bound = Bound::new(&cfg, &[&source, &source], PER_APPEND_ROW_CAP);

    for restart in 0..RESTARTS {
        let session = begin_cut(&device, RECORDS, RECORDS, Some(restart as u64 + 1));
        let mut stager = Stager::new(
            &device,
            session.generation,
            &source,
            &bound,
            PAGES_PER_RESTART,
        );
        stager.stage_manifest();
        assert_eq!(stager.downloaded, 0, "the journal verifies every page");
    }
    let session = begin_cut(&device, RECORDS, RECORDS, Some(RESTARTS as u64 + 1));
    let mut stager = Stager::new(&device, session.generation, &source, &bound, usize::MAX);
    assert_eq!(stager.stage_manifest(), RECORDS);
    assert_eq!(staged_count(&cfg, session.generation), RECORDS as i64);
    device.finish_snapshot(session.generation).unwrap();
}

#[test]
fn downloaded_manifest_pages_with_reuse_keep_staged_rows_bounded_across_restarts() {
    const RECORDS: usize = 6_000;
    // Each restart spends a few appends, so every generation stops part-way and the next reuses it.
    const BUDGETS: [usize; RESTARTS] = [7, 4, 5, 3];
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let template = plain_log(&dir, &vault, 1).remove(0);
    let cfg = config(&dir, "bounded-download", &vault);
    let device = unlocked(&cfg, &vault);
    let source = Source::envelopes(&fabricated_log(&template, RECORDS, FABRICATED_CIPHERTEXT));
    let bound = Bound::new(&cfg, &[&source, &source], PER_APPEND_ROW_CAP);

    for (restart, budget) in BUDGETS.into_iter().enumerate() {
        let session = begin_cut(&device, RECORDS, RECORDS, Some(restart as u64 + 1));
        let mut stager = Stager::new(&device, session.generation, &source, &bound, budget);
        stager.stage_manifest();
        if restart > 0 {
            assert!(
                stager.downloaded < RECORDS,
                "restart {restart} reuses the previous generation instead of downloading all"
            );
        }
    }
    let session = begin_cut(&device, RECORDS, RECORDS, Some(RESTARTS as u64 + 1));
    let mut stager = Stager::new(&device, session.generation, &source, &bound, usize::MAX);
    assert_eq!(stager.stage_manifest(), RECORDS);
    assert_eq!(staged_count(&cfg, session.generation), RECORDS as i64);
    device.finish_snapshot(session.generation).unwrap();
}

#[test]
fn mixed_wire_sizes_alternate_across_restarts_within_the_bound() {
    const RECORDS: usize = 4_000;
    const BUDGETS: [usize; RESTARTS] = [9, 5, 9, 5];
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "mixed-sizes", &vault);
    let device = unlocked(&cfg, &vault);
    let large = Source::placeholders(RECORDS, LARGE_WIRE);
    let small = Source::placeholders(RECORDS, SMALL_WIRE);
    // Either size can be the reuse source of the other, so the largest generation is held twice.
    let bound = Bound::new(&cfg, &[&large, &large], PER_APPEND_ROW_CAP);

    // Large then small, and small then large: the old generation's bytes exceed the new one's
    // and the reverse, both while the reuse source is still exempt.
    for (restart, budget) in BUDGETS.into_iter().enumerate() {
        let source = if restart % 2 == 0 { &large } else { &small };
        let session = begin_cut(&device, RECORDS, RECORDS, Some(restart as u64 + 1));
        let mut stager = Stager::new(&device, session.generation, source, &bound, budget);
        stager.stage_manifest();
    }
    let session = begin_cut(&device, RECORDS, RECORDS, Some(RESTARTS as u64 + 1));
    let mut stager = Stager::new(&device, session.generation, &small, &bound, usize::MAX);
    assert_eq!(stager.stage_manifest(), RECORDS);
    device.finish_snapshot(session.generation).unwrap();
}

#[test]
fn full_restarts_keep_staged_rows_bounded_and_never_reuse() {
    const RECORDS: usize = 4_000;
    const APPENDS_PER_RESTART: usize = 5;
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let template = plain_log(&dir, &vault, 1).remove(0);
    let cfg = config(&dir, "bounded-full", &vault);
    let device = unlocked(&cfg, &vault);
    let source = Source::envelopes(&fabricated_log(&template, RECORDS, FABRICATED_CIPHERTEXT));
    let bound = Bound::new(&cfg, &[&source, &source], PER_APPEND_ROW_CAP);

    for _ in 0..RESTARTS {
        let session = device
            .begin_snapshot(
                Cursor(RECORDS as u64),
                RECORDS as u64,
                SnapshotPurpose::Resync,
            )
            .unwrap();
        assert_reuse_key_absent(&cfg);
        assert_eq!(staged_count(&cfg, session.generation), 0);
        let mut stager = Stager::new(
            &device,
            session.generation,
            &source,
            &bound,
            APPENDS_PER_RESTART,
        );
        stager.download_all();
        assert_eq!(
            staged_count(&cfg, session.generation),
            stager.downloaded as i64,
            "a full generation stages only what it downloads"
        );
    }
    let session = device
        .begin_snapshot(
            Cursor(RECORDS as u64),
            RECORDS as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    let mut stager = Stager::new(&device, session.generation, &source, &bound, usize::MAX);
    assert_eq!(stager.download_all(), RECORDS);
    assert_eq!(staged_count(&cfg, session.generation), RECORDS as i64);
    device.finish_snapshot(session.generation).unwrap();
    assert_reuse_key_absent(&cfg);
}

#[derive(Clone, Copy, Debug)]
enum Successor {
    Published,
    FailedByBadPage,
    SupersededByManifest,
    SupersededByFull,
}

/// Leaves a reuse source with a tail row, then ends its successor as `outcome` describes. Every
/// dead generation and the reuse key must be gone once the client settles, reopened or not.
fn successor_outcome_releases_the_reuse_source(outcome: Successor, reopen: bool) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 4);
    let cfg = config(&dir, "successor", &vault);
    let mut client = unlocked(&cfg, &vault);
    // Generation 1 holds 1, 2 and 4; the newer cut (high 3) no longer contains 4.
    let older = begin_cut(&client, 4, 4, Some(1));
    stage_raw(&client, older.generation, &log, &[1, 2, 4]);
    let newer = begin_cut(&client, 3, 3, Some(2));
    let page = client
        .append_snapshot_manifest(newer.generation, &all_entries(&log)[..3])
        .unwrap();
    assert_eq!(page.missing, 1);
    stage_raw(&client, newer.generation, &log, &[3]);

    match outcome {
        Successor::Published => {
            client.finish_snapshot(newer.generation).unwrap();
        }
        Successor::FailedByBadPage => {
            assert_eq!(
                client
                    .append_snapshot_manifest(newer.generation, &[entry(4, &log[3])])
                    .map(|_| ()),
                Err(Error::SnapshotMismatch)
            );
        }
        Successor::SupersededByManifest => {
            let third = begin_cut(&client, 3, 3, Some(3));
            // Generation 1 is dead now; generation 2 is the reuse source while generation 3 stages.
            settle(&client);
            assert_eq!(staged_count(&cfg, older.generation), 0);
            assert_eq!(staged_count(&cfg, newer.generation), 3);
            let page = client
                .append_snapshot_manifest(third.generation, &all_entries(&log)[..3])
                .unwrap();
            assert_eq!(page.missing, 0);
            client.finish_snapshot(third.generation).unwrap();
        }
        Successor::SupersededByFull => {
            let full = client
                .begin_snapshot(Cursor(3), 3, SnapshotPurpose::Resync)
                .unwrap();
            stage_raw(&client, full.generation, &log, &[1, 2, 3]);
            client.finish_snapshot(full.generation).unwrap();
        }
    }
    if reopen {
        drop(client);
        client = unlocked(&cfg, &vault);
    }
    settle(&client);
    assert_no_dead_generations(&cfg);
    assert_reuse_key_absent(&cfg);
}

#[test]
fn published_successor_releases_its_reuse_source() {
    successor_outcome_releases_the_reuse_source(Successor::Published, false);
    successor_outcome_releases_the_reuse_source(Successor::Published, true);
}

#[test]
fn failed_successor_releases_its_reuse_source() {
    successor_outcome_releases_the_reuse_source(Successor::FailedByBadPage, false);
    successor_outcome_releases_the_reuse_source(Successor::FailedByBadPage, true);
}

#[test]
fn successor_superseded_by_a_manifest_begin_hands_over_the_reuse_source() {
    successor_outcome_releases_the_reuse_source(Successor::SupersededByManifest, false);
    successor_outcome_releases_the_reuse_source(Successor::SupersededByManifest, true);
}

#[test]
fn successor_superseded_by_a_full_begin_releases_its_reuse_source() {
    successor_outcome_releases_the_reuse_source(Successor::SupersededByFull, false);
    successor_outcome_releases_the_reuse_source(Successor::SupersededByFull, true);
}

#[test]
fn reopened_client_resumes_the_restart_and_still_reuses_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "process-restart", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 3, 3, Some(1));
    stage_prefix(&client, older.generation, &log, 2);
    let newer = begin_cut(&client, 3, 3, Some(2));
    drop(client);

    let client = unlocked(&cfg, &vault);
    let progress = client.snapshot_progress().unwrap().unwrap();
    assert_eq!(
        (
            progress.generation,
            progress.manifest,
            progress.received_records
        ),
        (newer.generation, true, 0)
    );
    let page = client
        .append_snapshot_manifest(newer.generation, &all_entries(&log))
        .unwrap();
    assert_eq!((page.missing, page.progress.received_records), (1, 2));
    assert_eq!(staged_count(&cfg, older.generation), 0);
    stage_raw(&client, newer.generation, &log, &[3]);
    client.finish_snapshot(newer.generation).unwrap();
}

#[test]
fn restore_begin_after_manifest_staging_clears_the_reuse_key_and_never_reuses() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "restore-reuse", &vault);
    let client = unlocked(&cfg, &vault);
    let manifest = begin_cut(&client, 3, 3, Some(1));
    stage_prefix(&client, manifest.generation, &log, 2);

    let restore = client
        .begin_snapshot(Cursor(3), 3, SnapshotPurpose::Restore)
        .unwrap();
    assert_reuse_key_absent(&cfg);
    assert_eq!(
        client
            .append_snapshot_manifest(restore.generation, &all_entries(&log))
            .map(|_| ()),
        Err(Error::InvalidRequest("snapshot manifest generation"))
    );
    // The earlier generation is obsolete with no reuse source, so its rows are plain garbage.
    assert_eq!(
        staged_count(&cfg, restore.generation),
        0,
        "restore never reuses rows"
    );
    assert_eq!(staged_count(&cfg, manifest.generation), 0);
    assert_no_dead_generations(&cfg);
}

#[test]
fn manifest_append_without_garbage_or_reuse_only_inserts_its_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "no-garbage", &vault);
    let client = unlocked(&cfg, &vault);
    for (index, envelope) in log.iter().enumerate() {
        client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
    }
    let session = begin_cut(&client, 3, 3, Some(1));
    // Count every DELETE and UPDATE the client runs on `snapshot_records` during the append.
    raw(&cfg)
        .execute_batch(
            "CREATE TABLE statement_probe(kind TEXT NOT NULL);
             CREATE TRIGGER probe_delete AFTER DELETE ON snapshot_records BEGIN INSERT INTO statement_probe VALUES('delete'); END;
             CREATE TRIGGER probe_update AFTER UPDATE ON snapshot_records BEGIN INSERT INTO statement_probe VALUES('update'); END;",
        )
        .unwrap();
    let page = client
        .append_snapshot_manifest(session.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(page.missing, 0);
    let probes: i64 = raw(&cfg)
        .query_row("SELECT COUNT(*) FROM statement_probe", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        probes, 0,
        "no garbage and no reuse source: no DELETE or UPDATE runs"
    );
    let conn = raw(&cfg);
    let cursors: Vec<i64> = conn
        .prepare("SELECT cursor FROM snapshot_records ORDER BY cursor")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        cursors,
        vec![1, 2, 3],
        "the append adds exactly its three rows"
    );
    assert_no_dead_generations(&cfg);
    assert_reuse_key_absent(&cfg);
}

fn generation_state(config: &ClientConfig, generation: u64) -> String {
    raw(config)
        .query_row(
            "SELECT state FROM snapshot_generations WHERE generation=?",
            params![generation as i64],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn reuse_never_bypasses_a_live_journal_conflict_at_the_same_cursor() {
    // A is staged at cursor 1 by the older generation. Live ingest then journals a different
    // envelope B at cursor 1 before the successor's manifest arrives.
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "journal-conflict", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 2, 2, Some(1));
    stage_prefix(&client, older.generation, &log, 1);
    let newer = begin_cut(&client, 2, 2, Some(2));
    client.ingest(&log[1], Cursor(1)).unwrap();

    let page = client
        .append_snapshot_manifest(newer.generation, &[entry(1, &log[0])])
        .unwrap();
    assert_eq!(
        page.missing, 1,
        "a conflicting journal row is never a reuse target"
    );
    assert_eq!(page.progress.received_records, 0);
    assert_eq!(staged_count(&cfg, older.generation), 1, "nothing is moved");
    assert_eq!(staged_count(&cfg, newer.generation), 0);

    // Downloading A reaches the byte-conflict check of the records page, as it did before reuse.
    assert_eq!(
        client
            .append_snapshot_raw_page(newer.generation, &[raw_record(1, &log)])
            .map(|_| ()),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(generation_state(&cfg, newer.generation), "failed");
}

/// Mirrors `SNAPSHOT_GC_MAX_ROWS_PER_APPEND` (four manifest pages): the most rows one append may
/// delete as garbage or trim as reuse prefix.
const PER_APPEND_ROW_CAP: i64 = 4 * MAX_SNAPSHOT_MANIFEST_PAGE as i64;

/// Inserts `wire` at every cursor of `cursors` for `generation`, bypassing the staging path.
fn stage_rows(
    config: &ClientConfig,
    generation: u64,
    cursors: std::ops::RangeInclusive<u64>,
    wire: &[u8],
) {
    let mut conn = raw(config);
    let tx = conn.transaction().unwrap();
    {
        let mut insert = tx
            .prepare("INSERT INTO snapshot_records(generation,cursor,wire) VALUES(?,?,?)")
            .unwrap();
        for cursor in cursors {
            insert
                .execute(params![generation as i64, cursor as i64, wire])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn total_rows(config: &ClientConfig) -> i64 {
    raw(config)
        .query_row("SELECT COUNT(*) FROM snapshot_records", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn sparse_jump_over_a_large_reuse_prefix_trims_at_most_the_cap_per_append() {
    const PREFIX: u64 = 30_000;
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let template = plain_log(&dir, &vault, 1).remove(0);
    let wire = serde_json::to_vec(&template).unwrap();
    let cfg = config(&dir, "sparse-jump", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, PREFIX as usize, PREFIX as usize, Some(1));
    stage_rows(&cfg, older.generation, 1..=PREFIX, &wire);
    let newer = begin_cut(&client, 2 * PREFIX as usize, 2, Some(2));

    // The entry at PREFIX verifies by reuse; every reuse row below it is now a trimmable prefix.
    let before = total_rows(&cfg);
    let page = client
        .append_snapshot_manifest(
            newer.generation,
            &[SnapshotManifestEntry {
                cursor: Cursor(PREFIX),
                digest: template.wire_digest().unwrap(),
                has_compaction: false,
                has_origin_tag: false,
            }],
        )
        .unwrap();
    assert_eq!(page.missing, 0);
    // Moves keep the row total, so the whole drop is the prefix trim of this append.
    assert_eq!(before - total_rows(&cfg), PER_APPEND_ROW_CAP);
    assert_eq!(staged_count(&cfg, newer.generation), 1);

    // The next append trims the remainder and the successor finishes.
    client
        .append_snapshot_raw_page(
            newer.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(2 * PREFIX),
                envelope_json: wire,
            }],
        )
        .unwrap();
    assert_eq!(staged_count(&cfg, older.generation), 0);
    client.finish_snapshot(newer.generation).unwrap();
}

#[test]
fn large_page_against_tiny_dead_rows_deletes_at_most_the_cap_per_append() {
    const DEAD_ROWS: u64 = 30_000;
    const PAGE: usize = MAX_SNAPSHOT_MANIFEST_PAGE;
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let template = plain_log(&dir, &vault, 1).remove(0);
    let log = fabricated_log(&template, PAGE, FABRICATED_CIPHERTEXT);
    let cfg = config(&dir, "tiny-dead", &vault);
    let device = unlocked(&cfg, &vault);
    let dead = device
        .begin_snapshot(Cursor(DEAD_ROWS), DEAD_ROWS, SnapshotPurpose::Resync)
        .unwrap();
    stage_rows(&cfg, dead.generation, 1..=DEAD_ROWS, &[0u8]);
    // A full begin obsoletes the dead generation without leaving a reuse source behind.
    device
        .begin_snapshot(Cursor(PAGE as u64), PAGE as u64, SnapshotPurpose::Resync)
        .unwrap();
    insert_journal(&cfg, &log);
    let page_cut = device
        .begin_snapshot_with_manifest(Cursor(PAGE as u64), PAGE as u64, Some(3))
        .unwrap();

    // The 5,000 journal copies reach a byte target that the one-byte dead rows can never meet, so
    // the uncapped collection would drain all 30,000 of them within this one append.
    let before = total_rows(&cfg);
    let page = device
        .append_snapshot_manifest(page_cut.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(page.missing, 0);
    let removed = before + PAGE as i64 - total_rows(&cfg);
    assert!(
        (1..=PER_APPEND_ROW_CAP).contains(&removed),
        "one append removed {removed} rows"
    );

    // Later calls finish the job.
    for _ in 0..20 {
        if staged_count(&cfg, dead.generation) == 0 {
            break;
        }
        device.apply_pending(1).unwrap();
    }
    assert_eq!(staged_count(&cfg, dead.generation), 0);
}

#[test]
fn disjoint_cuts_stay_within_two_generations_plus_one_batch() {
    // The old generation holds cursors above the successor's high water, so no reuse row exists
    // and every row of both generations stays staged. Records are of comparable size per cut.
    const RECORDS: usize = 8_000;
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "disjoint", &vault);
    let device = unlocked(&cfg, &vault);
    let old = Source::placeholders(RECORDS, LARGE_WIRE);
    let new = Source::placeholders(RECORDS, SMALL_WIRE);
    let bound = Bound::new(&cfg, &[&old, &new], GC_BATCH_ROWS);

    let old_session = device
        .begin_snapshot_with_manifest(Cursor(2 * RECORDS as u64), RECORDS as u64, Some(1))
        .unwrap();
    stage_rows(
        &cfg,
        old_session.generation,
        RECORDS as u64 + 1..=2 * RECORDS as u64,
        &old.wires[0],
    );
    bound.check("the old generation");

    let session = begin_cut(&device, RECORDS, RECORDS, Some(2));
    let mut stager = Stager::new(&device, session.generation, &new, &bound, usize::MAX);
    assert_eq!(stager.stage_manifest(), RECORDS);
    assert_eq!(staged_count(&cfg, session.generation), RECORDS as i64);
    assert_eq!(staged_count(&cfg, old_session.generation), RECORDS as i64);
    bound.check("the successor's completion");
}

/// Live ingest at a cursor where the staging successor holds a different record fails that
/// successor. Its reuse source must become ordinary garbage in the same transaction.
fn run_live_conflict_scenario(reopen: bool) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "live-conflict", &vault);
    let mut client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 3, 3, Some(1));
    stage_prefix(&client, older.generation, &log, 2);
    let newer = begin_cut(&client, 3, 3, Some(2));
    // The successor stages A (record 1's bytes) at cursor 2; live ingest then journals B there.
    client
        .append_snapshot_raw_page(
            newer.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(2),
                envelope_json: serde_json::to_vec(&log[0]).unwrap(),
            }],
        )
        .unwrap();
    client.ingest(&log[1], Cursor(2)).unwrap();
    assert_eq!(generation_state(&cfg, newer.generation), "failed");
    assert_reuse_key_absent(&cfg);

    if reopen {
        drop(client);
        client = unlocked(&cfg, &vault);
    }
    settle(&client);
    assert_no_dead_generations(&cfg);
    assert_reuse_key_absent(&cfg);
}

#[test]
fn live_conflict_releases_the_reuse_source() {
    run_live_conflict_scenario(false);
}

#[test]
fn live_conflict_releases_the_reuse_source_after_reopen() {
    run_live_conflict_scenario(true);
}

fn wire_length(config: &ClientConfig, generation: u64, cursor: u64) -> i64 {
    raw(config)
        .query_row(
            "SELECT LENGTH(wire) FROM snapshot_records WHERE generation=? AND cursor=?",
            params![generation as i64, cursor as i64],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn byte_cap_after_successful_moves_rolls_the_page_back_and_fails_the_successor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "byte-cap-moves", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 2, 2, Some(1));
    stage_prefix(&client, older.generation, &log, 2);
    let newer = begin_cut(&client, 2, 2, Some(2));
    // Leave room for the first move only: the second verified entry crosses the byte cap.
    let room = MAX_SNAPSHOT_BYTES as i64 - wire_length(&cfg, older.generation, 1) - 1;
    raw(&cfg)
        .execute(
            "UPDATE snapshot_generations SET received_bytes=? WHERE generation=?",
            params![room, newer.generation as i64],
        )
        .unwrap();

    assert_eq!(
        client
            .append_snapshot_manifest(newer.generation, &all_entries(&log))
            .map(|_| ()),
        Err(Error::SnapshotTooLarge)
    );
    // The first move was made inside the failed transaction, so the rollback returns it to the
    // reuse source. The failed successor keeps nothing from the page.
    assert_eq!(generation_state(&cfg, newer.generation), "failed");
    assert_eq!(staged_count(&cfg, newer.generation), 0);
    assert_eq!(staged_count(&cfg, older.generation), 2);
    assert_reuse_key_absent(&cfg);
    settle(&client);
    assert_no_dead_generations(&cfg);
}

#[test]
fn replayed_partially_reused_manifest_page_fails_without_moving_rows_again() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "replay-partial", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 3, 3, Some(1));
    stage_prefix(&client, older.generation, &log, 3);
    let newer = begin_cut(&client, 3, 3, Some(2));
    let page = {
        let mut entries = all_entries(&log);
        entries[2].digest = [0; 32];
        entries
    };
    let first = client
        .append_snapshot_manifest(newer.generation, &page)
        .unwrap();
    assert_eq!(first.missing, 1);
    assert_eq!(staged_count(&cfg, newer.generation), 2);

    // The replay names cursors already at the progress mark: rejected before any row moves.
    assert_eq!(
        client
            .append_snapshot_manifest(newer.generation, &page)
            .map(|_| ()),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(generation_state(&cfg, newer.generation), "failed");
    assert_eq!(
        staged_count(&cfg, newer.generation),
        2,
        "replay adds and moves nothing"
    );
    settle(&client);
    assert_no_dead_generations(&cfg);
}

#[test]
fn reopen_after_partial_reuse_resumes_at_the_durable_cursor_with_moved_rows_kept() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 4);
    let cfg = config(&dir, "reopen-partial", &vault);
    let client = unlocked(&cfg, &vault);
    let older = begin_cut(&client, 4, 4, Some(1));
    stage_prefix(&client, older.generation, &log, 4);
    let newer = begin_cut(&client, 4, 4, Some(2));
    let mut entries = all_entries(&log);
    entries[2].digest = [0; 32];
    let page = client
        .append_snapshot_manifest(newer.generation, &entries)
        .unwrap();
    assert_eq!((page.missing, page.progress.last_cursor), (1, Cursor(2)));
    assert_eq!(staged_count(&cfg, newer.generation), 2);
    drop(client);

    let client = unlocked(&cfg, &vault);
    let progress = client.snapshot_progress().unwrap().unwrap();
    assert_eq!(
        (
            progress.generation,
            progress.last_cursor,
            progress.received_records
        ),
        (newer.generation, Cursor(2), 2)
    );
    assert_eq!(
        staged_count(&cfg, newer.generation),
        2,
        "moved rows survive the reopen"
    );
    stage_raw(&client, newer.generation, &log, &[3]);
    let last = client
        .append_snapshot_manifest(newer.generation, &all_entries(&log)[3..])
        .unwrap();
    assert_eq!(last.missing, 0);
    assert_eq!(staged_count(&cfg, older.generation), 0);
    client.finish_snapshot(newer.generation).unwrap();
    settle(&client);
    assert_eq!(client.receive_cursor().unwrap(), Cursor(4));
}
