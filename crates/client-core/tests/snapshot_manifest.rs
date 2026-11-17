//! Verified resync (`begin_snapshot_with_manifest`): a manifest generation stages the same bytes
//! as a full import, verifies entries against the local journal, reports runs that must be
//! downloaded, and keeps the caps, failure and restart rules.
use crate::common::*;
use crate::snapshot_projection::{
    BOOK, book, capture, compact, contact, settle, state, tombstone, visible,
};
use rusqlite::params;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const OVERSIZE_JSON: usize = 2 * 1024 * 1024 + 1;

fn raw(config: &ClientConfig) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "07".repeat(32)))
        .unwrap();
    conn
}

fn entry(cursor: u64, envelope: &Envelope) -> SnapshotManifestEntry {
    SnapshotManifestEntry {
        cursor: Cursor(cursor),
        digest: envelope.wire_digest().unwrap(),
        has_compaction: envelope.compaction.is_some(),
        has_origin_tag: envelope.origin_tag.is_some(),
    }
}

fn served_json(_: u64, envelope: &Envelope) -> Vec<u8> {
    serde_json::to_vec(envelope).unwrap()
}
fn untampered(_: u64, _: &mut SnapshotManifestEntry) {}

/// What `raw_wire` stores for an over-limit record: the marker and the digest of the bytes.
fn oversize_marker(json: &[u8]) -> Vec<u8> {
    let mut marker = format!("oversize:{}:", json.len()).into_bytes();
    marker.extend_from_slice(&Sha256::digest(json));
    marker
}

fn ingest_all(device: &Client, log: &[Envelope]) {
    for (index, envelope) in log.iter().enumerate() {
        device.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
    }
}

/// The host's full import: every retained record is downloaded and staged.
fn stage_full(
    client: &Client,
    retained: &[SnapshotRecord],
    high: u64,
    generation: Option<u64>,
) -> Result<u64, Error> {
    let session = client.begin_snapshot_with_compaction(
        Cursor(high),
        retained.len() as u64,
        SnapshotPurpose::Resync,
        generation,
    )?;
    let records: Vec<RawSnapshotRecord> = retained
        .iter()
        .map(|r| RawSnapshotRecord {
            cursor: r.cursor,
            envelope_json: served_json(r.cursor.0, &r.envelope),
        })
        .collect();
    for page in records.chunks(500) {
        client.append_snapshot_raw_page(session.generation, page)?;
    }
    Ok(session.generation)
}

/// The host's manifest loop. `tamper` edits entries before they are sent, `served` gives the bytes
/// the records page returns for a downloaded record. Returns the generation and the `missing`
/// value of every manifest page.
fn stage_manifest(
    client: &Client,
    retained: &[SnapshotRecord],
    high: u64,
    generation: Option<u64>,
    tamper: &dyn Fn(u64, &mut SnapshotManifestEntry),
    served: &dyn Fn(u64, &Envelope) -> Vec<u8>,
) -> Result<(u64, Vec<u32>), Error> {
    let session =
        client.begin_snapshot_with_manifest(Cursor(high), retained.len() as u64, generation)?;
    let mut runs = Vec::new();
    let mut next = 0;
    while next < retained.len() {
        let end = (next + MAX_SNAPSHOT_MANIFEST_PAGE).min(retained.len());
        let entries: Vec<SnapshotManifestEntry> = retained[next..end]
            .iter()
            .map(|r| {
                let mut e = entry(r.cursor.0, &r.envelope);
                tamper(r.cursor.0, &mut e);
                e
            })
            .collect();
        let page = client.append_snapshot_manifest(session.generation, &entries)?;
        let verified_through = page.progress.last_cursor.0;
        next += retained[next..]
            .iter()
            .take_while(|r| r.cursor.0 <= verified_through)
            .count();
        let missing = page.missing as usize;
        runs.push(page.missing);
        if missing > 0 {
            let run: Vec<RawSnapshotRecord> = retained[next..next + missing]
                .iter()
                .map(|r| RawSnapshotRecord {
                    cursor: r.cursor,
                    envelope_json: served(r.cursor.0, &r.envelope),
                })
                .collect();
            client.append_snapshot_raw_page(session.generation, &run)?;
            next += missing;
        }
    }
    Ok((session.generation, runs))
}

#[derive(Debug, PartialEq)]
struct Staged {
    rows: Vec<(i64, Vec<u8>)>,
    received: i64,
    received_bytes: i64,
    overlap: i64,
    last_cursor: i64,
}
fn staged(config: &ClientConfig, generation: u64) -> Staged {
    let conn = raw(config);
    let mut stmt = conn
        .prepare("SELECT cursor,wire FROM snapshot_records WHERE generation=? ORDER BY cursor")
        .unwrap();
    let rows = stmt
        .query_map(params![generation as i64], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let (received, received_bytes, overlap, last_cursor) = conn
        .query_row(
            "SELECT received,received_bytes,overlap,last_cursor FROM snapshot_generations WHERE generation=?",
            params![generation as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    Staged {
        rows,
        received,
        received_bytes,
        overlap,
        last_cursor,
    }
}
fn staged_count(config: &ClientConfig, generation: u64) -> i64 {
    raw(config)
        .query_row(
            "SELECT COUNT(*) FROM snapshot_records WHERE generation=?",
            params![generation as i64],
            |r| r.get(0),
        )
        .unwrap()
}

#[derive(Debug, PartialEq)]
struct Published {
    journal: Vec<(i64, String, Option<String>, Vec<u8>)>,
    receive_cursor: Cursor,
    projection: Option<(SnapshotProjectionState, Option<String>)>,
    visible: Vec<String>,
    repair_required: bool,
    projection_generations: Vec<(i64, i64, String, Option<String>)>,
    projection_retained: Vec<(i64, i64)>,
    restores: Vec<(String, i64, String)>,
    restore_records: Vec<(i64, i64)>,
}
fn published(config: &ClientConfig, client: &Client) -> Published {
    let conn = raw(config);
    let journal = {
        let mut stmt = conn
            .prepare("SELECT cursor,status,reason,wire FROM journal ORDER BY cursor")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let projection_generations = {
        let mut stmt = conn
            .prepare("SELECT generation,high_water,state,reason FROM projection_generations ORDER BY generation")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let projection_retained = {
        let mut stmt = conn
            .prepare("SELECT generation,cursor FROM projection_retained ORDER BY generation,cursor")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let restores = {
        let mut stmt = conn
            .prepare(
                "SELECT use_id,generation,state FROM attachment_retention_restores ORDER BY use_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let restore_records = {
        let mut stmt = conn
            .prepare("SELECT generation,cursor FROM attachment_retention_restore_records ORDER BY generation,cursor")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    Published {
        journal,
        receive_cursor: client.receive_cursor().unwrap(),
        projection: state(client),
        visible: visible(client, BOOK),
        repair_required: client.contact_repair_required().unwrap(),
        projection_generations,
        projection_retained,
        restores,
        restore_records,
    }
}

fn set_journal_wire(config: &ClientConfig, cursor: u64, wire: &[u8]) {
    raw(config)
        .execute(
            "UPDATE journal SET wire=? WHERE cursor=?",
            params![wire, cursor as i64],
        )
        .unwrap();
}

/// Scenario fixture: an owner writes contacts; two devices hold the same journal prefix.
#[derive(Clone, Copy, Debug)]
enum Scenario {
    /// Nothing is compacted and nothing is added after the devices synced.
    Unchanged,
    /// The superseded upsert is compacted away; every record the devices hold is at or below
    /// their cursor, so the manifest and the full snapshot both omit it.
    CompactedBelowCursor,
    /// After the devices synced, a tombstone compacts their upsert and a new contact is added.
    AddedAboveCursor,
}

struct Prepared {
    _dir: TempDir,
    a_cfg: ClientConfig,
    b_cfg: ClientConfig,
    a: Client,
    b: Client,
    retained: Vec<SnapshotRecord>,
    high: u64,
}

fn prepare(scenario: Scenario, settle_history: bool) -> Prepared {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_cfg = config(&dir, "owner", &vault);
    let owner = unlocked(&owner_cfg, &vault);
    owner.set_server_compaction_supported(true).unwrap();
    let mut server = Server::default();
    capture(
        &owner,
        &book(&owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    server.upload(&owner);
    if let Scenario::CompactedBelowCursor = scenario {
        tombstone(&owner, BOOK, "scan1", &["c2"]);
        server.upload(&owner);
    }
    let a_cfg = config(&dir, "device-a", &vault);
    let b_cfg = config(&dir, "device-b", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    ingest_all(&a, &server.log);
    ingest_all(&b, &server.log);
    if settle_history {
        // Materialize the histories so the import must remove state that already exists locally.
        settle(&a);
        settle(&b);
    }
    if let Scenario::AddedAboveCursor = scenario {
        tombstone(&owner, BOOK, "scan1", &["c2"]);
        capture(
            &owner,
            &book(&owner_cfg, BOOK),
            vec![contact(BOOK, "c3", "Cy")],
        );
        server.upload(&owner);
    }
    let log_len = server.log.len();
    let retained = compact(&server.log, false);
    match scenario {
        Scenario::Unchanged => assert_eq!(retained.len(), log_len),
        Scenario::CompactedBelowCursor => assert!(retained.len() < log_len),
        Scenario::AddedAboveCursor => assert!(retained.len() < log_len),
    }
    Prepared {
        _dir: dir,
        a_cfg,
        b_cfg,
        a,
        b,
        retained,
        high: server.log.len() as u64,
    }
}

fn assert_equivalent_import(
    scenario: Scenario,
    settle_history: bool,
    initial_contacts: &[&str],
    expected_contacts: &[&str],
) {
    let p = prepare(scenario, settle_history);
    assert_eq!(
        published(&p.a_cfg, &p.a).journal,
        published(&p.b_cfg, &p.b).journal,
        "both devices must start from the same journal"
    );
    assert_eq!(
        visible(&p.a, BOOK),
        initial_contacts,
        "initial state of device A"
    );
    assert_eq!(
        visible(&p.b, BOOK),
        initial_contacts,
        "initial state of device B"
    );

    let generation_a = stage_full(&p.a, &p.retained, p.high, Some(7)).unwrap();
    let (generation_b, _) = stage_manifest(
        &p.b,
        &p.retained,
        p.high,
        Some(7),
        &untampered,
        &served_json,
    )
    .unwrap();
    assert_eq!(generation_a, generation_b);
    assert_eq!(
        staged(&p.a_cfg, generation_a),
        staged(&p.b_cfg, generation_b),
        "staging before publish must be identical"
    );

    p.a.finish_snapshot(generation_a).unwrap();
    p.b.finish_snapshot(generation_b).unwrap();
    settle(&p.a);
    settle(&p.b);
    let full = published(&p.a_cfg, &p.a);
    let via_manifest = published(&p.b_cfg, &p.b);
    assert_eq!(full, via_manifest, "published state must be identical");
    assert_eq!(visible(&p.a, BOOK), expected_contacts);
    assert_eq!(p.a.receive_cursor().unwrap(), Cursor(p.high));
}

#[test]
fn equivalence_nothing_compacted_matches_full_import() {
    assert_equivalent_import(Scenario::Unchanged, true, &["c1", "c2"], &["c1", "c2"]);
}

#[test]
fn equivalence_records_compacted_below_cursor_match_full_import() {
    // The tombstone is part of the device's applied history, so the compacted upsert is already gone.
    assert_equivalent_import(Scenario::CompactedBelowCursor, true, &["c2"], &["c2"]);
}

#[test]
fn equivalence_records_added_above_cursor_match_full_import() {
    // `c1` exists locally on both devices; the compacted import must remove it.
    assert_equivalent_import(
        Scenario::AddedAboveCursor,
        true,
        &["c1", "c2"],
        &["c2", "c3"],
    );
}

#[test]
fn equivalence_pending_history_added_above_cursor_matches_full_import() {
    assert_equivalent_import(Scenario::AddedAboveCursor, false, &[], &["c2", "c3"]);
}

/// Owner-side log with one origin-tagged SMS, one untagged SMS and one compacted contact upsert.
fn verification_log(dir: &TempDir, vault: &Vault) -> Vec<Envelope> {
    let cfg = config(dir, "verify-source", vault);
    let source = unlocked(&cfg, vault);
    source.set_server_compaction_supported(false).unwrap();
    source.set_server_origin_tags_supported(true).unwrap();
    source.capture_incoming(incoming("tagged", "v-1")).unwrap();
    source.set_server_origin_tags_supported(false).unwrap();
    source.capture_incoming(incoming("plain", "v-2")).unwrap();
    source.set_server_compaction_supported(true).unwrap();
    capture(&source, &book(&cfg, BOOK), vec![contact(BOOK, "c1", "Ada")]);
    let mut server = Server::default();
    server.upload(&source);
    // Message origin tags need provider evidence; the server record itself carries the tag here.
    // The digest does not cover it, so the entry and journal copies still verify on the digest.
    let mut log = server.log;
    log[0].origin_tag = Some([1; 32]);
    log
}

/// Records whose headers the digest does not cover; the manifest always reports these as missing.
fn flagged_count(log: &[Envelope]) -> u32 {
    log.iter()
        .filter(|e| e.compaction.is_some() || e.origin_tag.is_some())
        .count() as u32
}

fn index_where(log: &[Envelope], predicate: impl Fn(&Envelope) -> bool) -> usize {
    log.iter().position(predicate).expect("record kind present")
}

#[test]
fn manifest_verification_fixture_has_each_record_kind() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    index_where(&log, |e| e.origin_tag.is_some());
    index_where(&log, |e| e.origin_tag.is_none() && e.compaction.is_none());
    index_where(&log, |e| e.compaction.is_some());
}

#[test]
fn manifest_wrong_digest_missing_row_and_oversize_marker_are_downloaded_and_finish() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let retained: Vec<SnapshotRecord> = log
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect();
    let high = log.len() as u64;
    let plain = index_where(&log, |e| e.origin_tag.is_none() && e.compaction.is_none()) as u64 + 1;
    let contact_cursor = index_where(&log, |e| e.compaction.is_some()) as u64 + 1;

    // Wrong digest on an unflagged record: the journal row is intact, the entry digest is not.
    // The flagged records are missing regardless of their digest, so only this plain entry
    // exercises the digest comparison.
    let (_, wrong) = raw_device(&dir, &vault, "wrong-digest", &log);
    let (generation, runs) = stage_manifest(
        &wrong,
        &retained,
        high,
        None,
        &|cursor, e| {
            if cursor == plain {
                e.digest[0] ^= 1;
            }
        },
        &served_json,
    )
    .unwrap();
    assert_eq!(
        runs.iter().sum::<u32>(),
        flagged_count(&log) + 1,
        "the mismatched plain entry is missing, and so is every flagged entry"
    );
    wrong.finish_snapshot(generation).unwrap();

    // Missing journal row: the device never journaled the record at this cursor.
    let (missing_cfg, missing) = raw_device(&dir, &vault, "missing-row", &log);
    raw(&missing_cfg)
        .execute("DELETE FROM journal WHERE cursor=?", params![plain as i64])
        .unwrap();
    let (generation, runs) =
        stage_manifest(&missing, &retained, high, None, &untampered, &served_json).unwrap();
    assert_eq!(runs.iter().sum::<u32>(), flagged_count(&log) + 1);
    missing.finish_snapshot(generation).unwrap();

    // Oversize marker: the journal holds the marker of a record over the raw envelope limit;
    // the records page serves the same over-limit bytes, so the download matches the marker.
    let oversize_cfg = config(&dir, "oversize", &vault);
    let oversize = unlocked(&oversize_cfg, &vault);
    ingest_all(&oversize, &log);
    let big = vec![b' '; OVERSIZE_JSON];
    set_journal_wire(&oversize_cfg, contact_cursor, &oversize_marker(&big));
    let (generation, runs) = stage_manifest(
        &oversize,
        &retained,
        high,
        None,
        &untampered,
        &|cursor, e| {
            if cursor == contact_cursor {
                big.clone()
            } else {
                served_json(cursor, e)
            }
        },
    )
    .unwrap();
    assert_eq!(runs.iter().sum::<u32>(), flagged_count(&log));
    oversize.finish_snapshot(generation).unwrap();
}

/// A device whose journal holds every record of `log`, with its config (for raw journal edits).
fn raw_device(
    dir: &TempDir,
    vault: &Vault,
    name: &str,
    log: &[Envelope],
) -> (ClientConfig, Client) {
    let cfg = config(dir, name, vault);
    let device = unlocked(&cfg, vault);
    ingest_all(&device, log);
    (cfg, device)
}

#[test]
fn manifest_unparseable_journal_wire_is_missing_and_the_served_copy_is_accepted() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let retained: Vec<SnapshotRecord> = log
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect();
    let cfg = config(&dir, "unparseable", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let plain = index_where(&log, |e| e.origin_tag.is_none() && e.compaction.is_none()) as u64 + 1;
    set_journal_wire(&cfg, plain, b"not an envelope");
    // The journal copy is unparseable, so no digest verifies at that cursor; the served bytes equal
    // the journal copy, so the download is an overlap and the generation completes.
    let (generation, runs) = stage_manifest(
        &device,
        &retained,
        log.len() as u64,
        None,
        &|cursor, e| {
            if cursor == plain {
                e.digest = [0xAB; 32];
            }
        },
        &|cursor, e| {
            if cursor == plain {
                b"not an envelope".to_vec()
            } else {
                served_json(cursor, e)
            }
        },
    )
    .unwrap();
    assert_eq!(runs.iter().sum::<u32>(), flagged_count(&log) + 1);
    device.finish_snapshot(generation).unwrap();
}

/// Explicit one-entry flow: the entry is `missing`, nothing is staged for it, and the records page
/// cannot complete it because the journal copy differs from the server record.
fn flag_mismatch_fails_at_records_page(
    cfg: &ClientConfig,
    device: &Client,
    cursor: u64,
    envelope: &Envelope,
    flags: (bool, bool),
) {
    let mut e = entry(cursor, envelope);
    e.has_compaction = flags.0;
    e.has_origin_tag = flags.1;
    let session = device
        .begin_snapshot_with_manifest(Cursor(cursor), 1, None)
        .unwrap();
    let page = device
        .append_snapshot_manifest(session.generation, &[e])
        .unwrap();
    assert_eq!(page.missing, 1, "a flag mismatch is missing, not a failure");
    assert_eq!(
        staged_count(cfg, session.generation),
        0,
        "nothing staged for the entry"
    );
    assert_eq!(
        device.append_snapshot_raw_page(
            session.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(cursor),
                envelope_json: serde_json::to_vec(envelope).unwrap(),
            }],
        ),
        Err(Error::SnapshotMismatch)
    );
    assert!(device.snapshot_progress().unwrap().is_none());
}

#[test]
fn manifest_compaction_flag_mismatch_in_both_directions_is_missing_then_fails_at_records_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let compacted = index_where(&log, |e| e.compaction.is_some());
    let plain = index_where(&log, |e| e.origin_tag.is_none() && e.compaction.is_none());
    let (cc, pc) = (compacted as u64 + 1, plain as u64 + 1);

    // Journal copy stored without `compaction` while the server record has it.
    let stripped_cfg = config(&dir, "stripped-compaction", &vault);
    let stripped = unlocked(&stripped_cfg, &vault);
    ingest_all(&stripped, &log);
    let mut without = log[compacted].clone();
    without.compaction = None;
    assert_eq!(
        without.wire_digest().unwrap(),
        log[compacted].wire_digest().unwrap(),
        "compaction is not covered by the digest"
    );
    set_journal_wire(&stripped_cfg, cc, &serde_json::to_vec(&without).unwrap());
    flag_mismatch_fails_at_records_page(
        &stripped_cfg,
        &stripped,
        cc,
        &log[compacted],
        (true, false),
    );

    // Journal copy stored with `compaction` while the server record has none.
    let added_cfg = config(&dir, "added-compaction", &vault);
    let added = unlocked(&added_cfg, &vault);
    ingest_all(&added, &log);
    let mut with = log[plain].clone();
    with.compaction = log[compacted].compaction.clone();
    assert_eq!(
        with.wire_digest().unwrap(),
        log[plain].wire_digest().unwrap(),
        "compaction is not covered by the digest"
    );
    set_journal_wire(&added_cfg, pc, &serde_json::to_vec(&with).unwrap());
    flag_mismatch_fails_at_records_page(&added_cfg, &added, pc, &log[plain], (false, false));
}

#[test]
fn manifest_origin_tag_flag_mismatch_in_both_directions_is_missing_then_fails_at_records_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let tagged = index_where(&log, |e| e.origin_tag.is_some());
    let plain = index_where(&log, |e| e.origin_tag.is_none() && e.compaction.is_none());
    let (tc, pc) = (tagged as u64 + 1, plain as u64 + 1);

    // Journal copy stored without `origin_tag` while the server record has it.
    let stripped_cfg = config(&dir, "stripped-origin", &vault);
    let stripped = unlocked(&stripped_cfg, &vault);
    ingest_all(&stripped, &log);
    let mut without = log[tagged].clone();
    without.origin_tag = None;
    assert_eq!(
        without.wire_digest().unwrap(),
        log[tagged].wire_digest().unwrap(),
        "origin_tag is not covered by the digest"
    );
    set_journal_wire(&stripped_cfg, tc, &serde_json::to_vec(&without).unwrap());
    flag_mismatch_fails_at_records_page(&stripped_cfg, &stripped, tc, &log[tagged], (false, true));

    // Journal copy stored with `origin_tag` while the server record has none.
    let added_cfg = config(&dir, "added-origin", &vault);
    let added = unlocked(&added_cfg, &vault);
    ingest_all(&added, &log);
    let mut with = log[plain].clone();
    with.origin_tag = Some([9; 32]);
    assert_eq!(
        with.wire_digest().unwrap(),
        log[plain].wire_digest().unwrap(),
        "origin_tag is not covered by the digest"
    );
    set_journal_wire(&added_cfg, pc, &serde_json::to_vec(&with).unwrap());
    flag_mismatch_fails_at_records_page(&added_cfg, &added, pc, &log[plain], (false, false));
}

/// Owner-side log of `n` plain (untagged, uncompacted) SMS records.
fn plain_log(dir: &TempDir, vault: &Vault, n: usize) -> Vec<Envelope> {
    let cfg = config(dir, "plain-source", vault);
    let source = unlocked(&cfg, vault);
    source.set_server_origin_tags_supported(false).unwrap();
    for i in 0..n {
        source
            .capture_incoming(incoming(&format!("m{i}"), &format!("p-{i}")))
            .unwrap();
    }
    let mut server = Server::default();
    server.upload(&source);
    assert_eq!(server.log.len(), n);
    server.log
}

fn raw_record(cursor: u64, log: &[Envelope]) -> RawSnapshotRecord {
    RawSnapshotRecord {
        cursor: Cursor(cursor),
        envelope_json: serde_json::to_vec(&log[cursor as usize - 1]).unwrap(),
    }
}

fn all_entries(log: &[Envelope]) -> Vec<SnapshotManifestEntry> {
    log.iter()
        .enumerate()
        .map(|(i, e)| entry(i as u64 + 1, e))
        .collect()
}

#[test]
fn manifest_missing_run_is_consumed_as_a_prefix_and_resumes_from_last_cursor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 6);
    let cfg = config(&dir, "runs", &vault);
    let device = unlocked(&cfg, &vault);
    for cursor in [1u64, 5, 6] {
        device
            .ingest(&log[cursor as usize - 1], Cursor(cursor))
            .unwrap();
    }
    let session = device
        .begin_snapshot_with_manifest(Cursor(6), 6, None)
        .unwrap();
    let entries = all_entries(&log);

    // One page: cursor 1 verifies, cursors 2 to 4 are a run of three missing, 5 and 6 are ignored.
    let page = device
        .append_snapshot_manifest(session.generation, &entries)
        .unwrap();
    assert_eq!(page.missing, 3);
    assert_eq!(page.progress.received_records, 1);
    assert_eq!(page.progress.last_cursor, Cursor(1));
    assert_eq!(staged_count(&cfg, session.generation), 1);

    device
        .append_snapshot_raw_page(
            session.generation,
            &[
                raw_record(2, &log),
                raw_record(3, &log),
                raw_record(4, &log),
            ],
        )
        .unwrap();

    // Second call from the new last cursor: 5 and 6 verify, nothing is missing.
    let page = device
        .append_snapshot_manifest(session.generation, &entries[4..])
        .unwrap();
    assert_eq!(page.missing, 0);
    assert_eq!(page.progress.received_records, 6);
    assert_eq!(page.progress.last_cursor, Cursor(6));
    device.finish_snapshot(session.generation).unwrap();
    assert_eq!(device.receive_cursor().unwrap(), Cursor(6));
}

#[test]
fn manifest_rejects_empty_and_oversize_pages_without_failing_the_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "pages", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let session = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    assert_eq!(
        device.append_snapshot_manifest(session.generation, &[]),
        Err(Error::InvalidRequest("snapshot manifest page"))
    );
    let too_many: Vec<SnapshotManifestEntry> = (0..=MAX_SNAPSHOT_MANIFEST_PAGE as u64)
        .map(|i| SnapshotManifestEntry {
            cursor: Cursor(i + 1),
            digest: [0; 32],
            has_compaction: false,
            has_origin_tag: false,
        })
        .collect();
    assert_eq!(
        device.append_snapshot_manifest(session.generation, &too_many),
        Err(Error::InvalidRequest("snapshot manifest page"))
    );
    assert!(
        device.snapshot_progress().unwrap().is_some(),
        "generation still staging"
    );
    device
        .append_snapshot_manifest(session.generation, &all_entries(&log))
        .unwrap();
    device.finish_snapshot(session.generation).unwrap();
}

#[test]
fn manifest_cursor_order_and_high_water_violations_fail_the_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let e = all_entries(&log);
    let cases: Vec<Vec<SnapshotManifestEntry>> = vec![
        vec![e[1].clone(), e[0].clone()],
        vec![e[0].clone(), e[0].clone()],
        vec![SnapshotManifestEntry {
            cursor: Cursor(3),
            ..e[0].clone()
        }],
    ];
    for (case, entries) in cases.into_iter().enumerate() {
        let cfg = config(&dir, &format!("order-{case}"), &vault);
        let device = unlocked(&cfg, &vault);
        ingest_all(&device, &log);
        device
            .begin_snapshot_with_manifest(Cursor(2), 2, None)
            .unwrap();
        let generation = device.snapshot_progress().unwrap().unwrap().generation;
        assert_eq!(
            device.append_snapshot_manifest(generation, &entries),
            Err(Error::SnapshotMismatch)
        );
        assert!(
            device.snapshot_progress().unwrap().is_none(),
            "generation failed"
        );
    }
}

#[test]
fn manifest_verified_entries_beyond_expected_fail_the_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "expected", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let session = device
        .begin_snapshot_with_manifest(Cursor(2), 1, None)
        .unwrap();
    assert_eq!(
        device.append_snapshot_manifest(session.generation, &all_entries(&log)),
        Err(Error::SnapshotMismatch)
    );
    assert!(device.snapshot_progress().unwrap().is_none());
}

#[test]
fn manifest_begin_counts_the_download_lower_bound_against_max_snapshot_records() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 1);
    let cfg = config(&dir, "caps-begin", &vault);
    let device = unlocked(&cfg, &vault);
    assert_eq!(
        device
            .begin_snapshot_with_manifest(Cursor(200_000), 100_001, None)
            .map(|_| ()),
        Err(Error::InvalidRequest("snapshot record count"))
    );
    assert!(
        device.snapshot_progress().unwrap().is_none(),
        "refusal changes nothing"
    );

    // One journaled record lowers the download bound to exactly MAX_SNAPSHOT_RECORDS.
    device.ingest(&log[0], Cursor(1)).unwrap();
    assert!(
        device
            .begin_snapshot_with_manifest(Cursor(200_000), 100_001, None)
            .unwrap()
            .manifest
    );
    assert_eq!(
        device
            .begin_snapshot(Cursor(200_000), 100_001, SnapshotPurpose::Resync)
            .map(|_| ()),
        Err(Error::InvalidRequest("snapshot record count"))
    );
    assert!(
        device.snapshot_progress().unwrap().unwrap().manifest,
        "a refused full begin keeps the manifest generation"
    );
}

fn insert_journal_rows(config: &ClientConfig, count: u64) {
    raw(config)
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{count}) \
             INSERT INTO journal(cursor,wire,status) SELECT i, x'00', 'applied' FROM n;"
        ))
        .unwrap();
}

#[test]
fn manifest_accepts_a_150000_record_vault_when_the_device_holds_enough() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "large-vault", &vault);
    let device = unlocked(&cfg, &vault);
    insert_journal_rows(&cfg, 50_000);
    assert!(
        device
            .begin_snapshot_with_manifest(Cursor(150_000), 150_000, None)
            .unwrap()
            .manifest
    );
    assert_eq!(
        device
            .begin_snapshot(Cursor(150_000), 150_000, SnapshotPurpose::Resync)
            .map(|_| ()),
        Err(Error::InvalidRequest("snapshot record count"))
    );
}

#[test]
fn manifest_record_count_is_bounded_by_max_snapshot_manifest_records() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "manifest-max", &vault);
    let device = unlocked(&cfg, &vault);
    assert_eq!(
        device
            .begin_snapshot_with_manifest(Cursor(1_000_001), 1_000_001, None)
            .map(|_| ()),
        Err(Error::InvalidRequest("snapshot record count"))
    );
    insert_journal_rows(&cfg, 900_000);
    assert!(
        device
            .begin_snapshot_with_manifest(Cursor(1_000_000), 1_000_000, None)
            .unwrap()
            .manifest
    );
}

#[test]
fn manifest_download_cap_fails_the_generation_at_100001_downloads() {
    // Every record below is a download: the journal copy at cursor 1 cannot verify, so the
    // generation downloads all 100,001 records. The download cap is what must fail it.
    // Placeholder bytes are unparseable, so `raw_wire` stores them unchanged; the cap counts
    // records, not their content, so this exercises the real accounting without 100,001 envelopes.
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 1);
    let cfg = config(&dir, "download-cap", &vault);
    let device = unlocked(&cfg, &vault);
    device.ingest(&log[0], Cursor(1)).unwrap();
    let served_first = serde_json::to_vec(&log[0]).unwrap();
    let session = device
        .begin_snapshot_with_manifest(Cursor(100_001), 100_001, None)
        .unwrap();
    let mut first = 1u64;
    while first <= 100_000 {
        let page: Vec<RawSnapshotRecord> = (first..first + 500)
            .map(|cursor| RawSnapshotRecord {
                cursor: Cursor(cursor),
                envelope_json: if cursor == 1 {
                    served_first.clone()
                } else {
                    b"x".to_vec()
                },
            })
            .collect();
        device
            .append_snapshot_raw_page(session.generation, &page)
            .unwrap();
        first += 500;
    }
    assert_eq!(
        device.append_snapshot_raw_page(
            session.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(100_001),
                envelope_json: b"x".to_vec(),
            }],
        ),
        Err(Error::SnapshotMismatch)
    );
    assert!(device.snapshot_progress().unwrap().is_none());
}

#[test]
fn manifest_verified_entries_are_counted_in_received_bytes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "bytes", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let session = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    device
        .append_snapshot_manifest(session.generation, &all_entries(&log))
        .unwrap();
    let journal_bytes: i64 = raw(&cfg)
        .query_row("SELECT SUM(LENGTH(wire)) FROM journal", [], |r| r.get(0))
        .unwrap();
    let staged = staged(&cfg, session.generation);
    assert_eq!(staged.received, 2);
    assert_eq!(staged.received_bytes, journal_bytes);
    assert_eq!(staged.overlap, 2);
}

fn manifest_metadata(config: &ClientConfig, key: &str) -> Option<String> {
    raw(config)
        .query_row("SELECT v FROM metadata WHERE k=?", params![key], |r| {
            r.get(0)
        })
        .ok()
}

#[test]
fn plain_begin_obsoletes_a_manifest_generation_and_clears_the_marker() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "obsolete", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let manifest = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    assert!(device.snapshot_progress().unwrap().unwrap().manifest);
    let plain = device
        .begin_snapshot(Cursor(2), 2, SnapshotPurpose::Resync)
        .unwrap();
    assert!(!plain.manifest);
    assert!(!device.snapshot_progress().unwrap().unwrap().manifest);
    assert_eq!(
        manifest_metadata(&cfg, "snapshot_manifest_generation"),
        None
    );
    assert_eq!(manifest_metadata(&cfg, "snapshot_manifest_verified"), None);
    assert_eq!(
        device.append_snapshot_manifest(manifest.generation, &all_entries(&log)),
        Err(Error::InvalidRequest("snapshot manifest generation"))
    );
}

#[test]
fn manifest_begin_resets_the_verified_counter() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "reset", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let first = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    device
        .append_snapshot_manifest(first.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(
        manifest_metadata(&cfg, "snapshot_manifest_verified").as_deref(),
        Some("2")
    );
    device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    assert_eq!(
        manifest_metadata(&cfg, "snapshot_manifest_verified").as_deref(),
        Some("0")
    );
}

#[test]
fn manifest_resumes_after_reopen_in_the_manifest_phase() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 6);
    let cfg = config(&dir, "resume-manifest", &vault);
    let device = unlocked(&cfg, &vault);
    for cursor in 1u64..=3 {
        device
            .ingest(&log[cursor as usize - 1], Cursor(cursor))
            .unwrap();
    }
    let session = device
        .begin_snapshot_with_manifest(Cursor(6), 6, None)
        .unwrap();
    let entries = all_entries(&log);
    device
        .append_snapshot_manifest(session.generation, &entries[..2])
        .unwrap();
    drop(device);

    let device = unlocked(&cfg, &vault);
    let progress = device.snapshot_progress().unwrap().unwrap();
    assert!(progress.manifest);
    assert_eq!(
        (progress.received_records, progress.last_cursor),
        (2, Cursor(2))
    );
    let page = device
        .append_snapshot_manifest(progress.generation, &entries[2..])
        .unwrap();
    assert_eq!(page.missing, 3);
    device
        .append_snapshot_raw_page(
            progress.generation,
            &[
                raw_record(4, &log),
                raw_record(5, &log),
                raw_record(6, &log),
            ],
        )
        .unwrap();
    device.finish_snapshot(progress.generation).unwrap();
    assert_eq!(device.receive_cursor().unwrap(), Cursor(6));
}

#[test]
fn manifest_resumes_after_reopen_in_the_download_phase() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 6);
    let cfg = config(&dir, "resume-download", &vault);
    let device = unlocked(&cfg, &vault);
    for cursor in 1u64..=3 {
        device
            .ingest(&log[cursor as usize - 1], Cursor(cursor))
            .unwrap();
    }
    let session = device
        .begin_snapshot_with_manifest(Cursor(6), 6, None)
        .unwrap();
    let page = device
        .append_snapshot_manifest(session.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(page.missing, 3);
    // Download only the first record of the run, then stop.
    device
        .append_snapshot_raw_page(session.generation, &[raw_record(4, &log)])
        .unwrap();
    drop(device);

    let device = unlocked(&cfg, &vault);
    let progress = device.snapshot_progress().unwrap().unwrap();
    assert!(progress.manifest);
    assert_eq!(
        (progress.received_records, progress.last_cursor),
        (4, Cursor(4))
    );
    device
        .append_snapshot_raw_page(
            progress.generation,
            &[raw_record(5, &log), raw_record(6, &log)],
        )
        .unwrap();
    device.finish_snapshot(progress.generation).unwrap();
    assert_eq!(device.receive_cursor().unwrap(), Cursor(6));
}

#[test]
fn manifest_append_is_refused_on_a_full_import_and_finish_checks_the_count() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 2);
    let cfg = config(&dir, "refusals", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let full = device
        .begin_snapshot(Cursor(2), 2, SnapshotPurpose::Resync)
        .unwrap();
    assert_eq!(
        device.append_snapshot_manifest(full.generation, &all_entries(&log)),
        Err(Error::InvalidRequest("snapshot manifest generation"))
    );
    let manifest = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    device
        .append_snapshot_manifest(manifest.generation, &all_entries(&log)[..1])
        .unwrap();
    assert_eq!(
        device.finish_snapshot(manifest.generation),
        Err(Error::SnapshotMismatch)
    );
}

#[test]
fn live_ingest_above_the_manifest_high_water_is_unaffected() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "live-above", &vault);
    let device = unlocked(&cfg, &vault);
    device.ingest(&log[0], Cursor(1)).unwrap();
    device.ingest(&log[1], Cursor(2)).unwrap();
    let session = device
        .begin_snapshot_with_manifest(Cursor(2), 2, None)
        .unwrap();
    device.ingest(&log[2], Cursor(3)).unwrap();
    let page = device
        .append_snapshot_manifest(session.generation, &all_entries(&log)[..2])
        .unwrap();
    assert_eq!(page.missing, 0);
    device.finish_snapshot(session.generation).unwrap();
    assert_eq!(
        raw(&cfg)
            .query_row("SELECT wire FROM journal WHERE cursor=3", [], |r| r
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        serde_json::to_vec(&log[2]).unwrap(),
        "the live record above high-water stays journaled as ingested"
    );
    assert_eq!(staged_count(&cfg, session.generation), 2);
}

#[test]
fn live_ingest_at_a_cursor_the_manifest_lists_verifies() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "live-listed", &vault);
    let device = unlocked(&cfg, &vault);
    device.ingest(&log[0], Cursor(1)).unwrap();
    let session = device
        .begin_snapshot_with_manifest(Cursor(3), 3, None)
        .unwrap();
    // Arrives live after the generation began, at a cursor the manifest will list.
    device.ingest(&log[1], Cursor(2)).unwrap();
    let page = device
        .append_snapshot_manifest(session.generation, &all_entries(&log))
        .unwrap();
    assert_eq!(page.progress.received_records, 2);
    assert_eq!(
        page.missing, 1,
        "cursor 2 verified from the live journal row"
    );
    device
        .append_snapshot_raw_page(session.generation, &[raw_record(3, &log)])
        .unwrap();
    device.finish_snapshot(session.generation).unwrap();
}

/// Two devices with identical journals of `log`: A imports fully, B through the manifest. Asserts
/// the staging and the published state are equal and returns the `missing` runs of B.
fn assert_manifest_matches_full(
    dir: &TempDir,
    vault: &Vault,
    name: &str,
    log: &[Envelope],
) -> Vec<u32> {
    let retained: Vec<SnapshotRecord> = log
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect();
    let high = log.len() as u64;
    let a_cfg = config(dir, &format!("{name}-a"), vault);
    let a = unlocked(&a_cfg, vault);
    let b_cfg = config(dir, &format!("{name}-b"), vault);
    let b = unlocked(&b_cfg, vault);
    ingest_all(&a, log);
    ingest_all(&b, log);
    settle(&a);
    settle(&b);
    let generation_a = stage_full(&a, &retained, high, Some(7)).unwrap();
    let (generation_b, runs) =
        stage_manifest(&b, &retained, high, Some(7), &untampered, &served_json).unwrap();
    assert_eq!(
        staged(&a_cfg, generation_a),
        staged(&b_cfg, generation_b),
        "staging before publish must be identical"
    );
    a.finish_snapshot(generation_a).unwrap();
    b.finish_snapshot(generation_b).unwrap();
    settle(&a);
    settle(&b);
    assert_eq!(published(&a_cfg, &a), published(&b_cfg, &b));
    runs
}

#[test]
fn manifest_flagged_identical_copies_are_downloaded_and_match_the_full_import() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let flagged = log
        .iter()
        .filter(|e| e.compaction.is_some() || e.origin_tag.is_some())
        .count() as u32;
    assert!(flagged >= 2, "fixture has a compacted and a tagged record");
    let runs = assert_manifest_matches_full(&dir, &vault, "flagged-identical", &log);
    assert_eq!(
        runs.iter().sum::<u32>(),
        flagged,
        "every flagged entry is downloaded, even when its journal copy is identical"
    );
}

/// The journal copy differs from the server record only inside the headers the digest does not
/// cover. Both carry the same digest, so the digest cannot tell them apart. The entry is missing,
/// and the records page must fail the generation rather than publish the journal copy.
fn assert_divergent_copy_fails_at_records_page(
    dir: &TempDir,
    vault: &Vault,
    name: &str,
    cursor: u64,
    server: &Envelope,
    journal: &Envelope,
    flags: (bool, bool),
) {
    assert_eq!(
        journal.wire_digest().unwrap(),
        server.wire_digest().unwrap(),
        "the digest is blind to this difference"
    );
    assert_ne!(
        serde_json::to_vec(journal).unwrap(),
        serde_json::to_vec(server).unwrap()
    );
    let cfg = config(dir, name, vault);
    let device = unlocked(&cfg, vault);
    device.ingest(server, Cursor(cursor)).unwrap();
    set_journal_wire(&cfg, cursor, &serde_json::to_vec(journal).unwrap());
    let before = published(&cfg, &device);
    flag_mismatch_fails_at_records_page(&cfg, &device, cursor, server, flags);
    assert_eq!(
        published(&cfg, &device),
        before,
        "nothing is published or rewritten"
    );
}

#[test]
fn manifest_flagged_divergent_compaction_metadata_fails_at_records_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let compacted = index_where(&log, |e| e.compaction.is_some());
    let server = &log[compacted];
    let original = server.compaction.clone().unwrap();
    let mut checkpoint = server.clone();
    checkpoint.compaction.as_mut().unwrap().checkpoint = !original.checkpoint;
    let mut terminal = server.clone();
    terminal.compaction.as_mut().unwrap().terminal = !original.terminal;
    let mut superseded = server.clone();
    superseded
        .compaction
        .as_mut()
        .unwrap()
        .supersedes
        .push(CompactionReference {
            producer_device_id: DeviceId::new(),
            producer_sequence: SourceSequence(1),
        });
    for (index, journal) in [checkpoint, terminal, superseded].iter().enumerate() {
        assert_divergent_copy_fails_at_records_page(
            &dir,
            &vault,
            &format!("divergent-compaction-{index}"),
            compacted as u64 + 1,
            server,
            journal,
            (true, false),
        );
    }
}

#[test]
fn manifest_flagged_divergent_origin_tag_value_fails_at_records_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = verification_log(&dir, &vault);
    let tagged = index_where(&log, |e| e.origin_tag.is_some());
    let server = &log[tagged];
    let mut journal = server.clone();
    journal.origin_tag = Some([2; 32]);
    assert_divergent_copy_fails_at_records_page(
        &dir,
        &vault,
        "divergent-origin",
        tagged as u64 + 1,
        server,
        &journal,
        (false, true),
    );
}

#[test]
fn manifest_failed_page_stages_nothing_from_its_verified_prefix() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 3);
    let cfg = config(&dir, "failed-page", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let session = device
        .begin_snapshot_with_manifest(Cursor(3), 2, None)
        .unwrap();
    // Cursors 1 and 2 verify and would be staged before cursor 3 exceeds `expected`.
    assert_eq!(
        device.append_snapshot_manifest(session.generation, &all_entries(&log)),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(
        staged_count(&cfg, session.generation),
        0,
        "rolled back, not staged"
    );
    assert!(device.snapshot_progress().unwrap().is_none());
}

#[test]
fn manifest_cursor_beyond_i64_fails_the_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 1);
    let cfg = config(&dir, "i64-cursor", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let high = i64::MAX as u64;
    let session = device
        .begin_snapshot_with_manifest(Cursor(high), 1, None)
        .unwrap();
    // Before the fix this cursor was clamped to i64::MAX and passed the high-water check.
    let entry = SnapshotManifestEntry {
        cursor: Cursor(high + 1),
        digest: [0; 32],
        has_compaction: false,
        has_origin_tag: false,
    };
    assert_eq!(
        device.append_snapshot_manifest(session.generation, &[entry]),
        Err(Error::SnapshotMismatch)
    );
    assert!(device.snapshot_progress().unwrap().is_none());
}

/// Sets a staging generation's received byte count directly in the database. The byte limit is
/// 512 MiB, so the test starts a generation just below it instead of staging that much data.
fn set_received_bytes(config: &ClientConfig, generation: u64, bytes: i64) {
    raw(config)
        .execute(
            "UPDATE snapshot_generations SET received_bytes=? WHERE generation=?",
            params![bytes, generation as i64],
        )
        .unwrap();
}

fn journal_length(config: &ClientConfig, cursor: u64) -> i64 {
    raw(config)
        .query_row(
            "SELECT LENGTH(wire) FROM journal WHERE cursor=?",
            params![cursor as i64],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn manifest_byte_limit_is_enforced_per_verified_row_and_rolls_back_the_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let log = plain_log(&dir, &vault, 4);
    let cfg = config(&dir, "byte-limit", &vault);
    let device = unlocked(&cfg, &vault);
    ingest_all(&device, &log);
    let max = MAX_SNAPSHOT_BYTES as i64;
    let (l1, l2, l3) = (
        journal_length(&cfg, 1),
        journal_length(&cfg, 2),
        journal_length(&cfg, 3),
    );
    let entries = all_entries(&log);

    // A page that lands exactly on the limit is accepted.
    let exact = device
        .begin_snapshot_with_manifest(Cursor(4), 4, None)
        .unwrap();
    set_received_bytes(&cfg, exact.generation, max - (l1 + l2));
    device
        .append_snapshot_manifest(exact.generation, &entries[..2])
        .unwrap();
    assert_eq!(staged(&cfg, exact.generation).received_bytes, max);

    // One more verified row exceeds the limit: the generation fails and that row is not staged.
    assert_eq!(
        device.append_snapshot_manifest(exact.generation, &entries[2..3]),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(staged_count(&cfg, exact.generation), 2, "earlier rows stay");

    // A failing page keeps none of its own verified prefix: cursor 2 and 3 fit, cursor 4 does not.
    let prefix = device
        .begin_snapshot_with_manifest(Cursor(4), 4, None)
        .unwrap();
    set_received_bytes(&cfg, prefix.generation, max - (l1 + l2 + l3));
    device
        .append_snapshot_manifest(prefix.generation, &entries[..1])
        .unwrap();
    assert_eq!(
        device.append_snapshot_manifest(prefix.generation, &entries[1..4]),
        Err(Error::SnapshotMismatch)
    );
    assert_eq!(
        staged_count(&cfg, prefix.generation),
        1,
        "the failing page's verified prefix is rolled back; the earlier row stays"
    );

    // The failure is durable across a reopen: the generation is no longer staging, so a further
    // page is refused. (Failed generations are garbage-collected, so their rows and state may be
    // gone after reopen; a generation that were still staging would accept cursor 2 here.)
    drop(device);
    let device = unlocked(&cfg, &vault);
    assert!(device.snapshot_progress().unwrap().is_none());
    assert_eq!(
        device.append_snapshot_manifest(prefix.generation, &entries[1..2]),
        Err(Error::InvalidRequest("snapshot manifest generation"))
    );
}
