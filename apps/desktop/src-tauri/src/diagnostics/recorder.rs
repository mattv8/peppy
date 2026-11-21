//! Bounded, opt-in recorder behind `diagnostics`. Records are queued by `accept` and written by one
//! background thread into `current.log` and `previous.log`, mirroring Android's `DiagnosticsLog`.
//! Callers never touch files; only the writer (or `process_pending` in tests) does.
//!
//! Lock order: `storage` before `queue`. Anything that changes the generation or the enabled flag,
//! and anything that touches staged exports, takes `storage` first, so a batch being appended is
//! never interleaved with a disable, a clear, or an export.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::{EVENT_DIAGNOSTIC_STATE, FIELD_DROPPED, FIELD_ENABLED};

pub const QUEUE_CAPACITY: usize = 256;
pub const BATCH_SIZE: usize = 64;
/// Includes the newline, as on Android.
pub const MAX_RECORD_BYTES: usize = 512;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
pub const CURRENT_FILE: &str = "current.log";
pub const PREVIOUS_FILE: &str = "previous.log";
/// Staged export snapshots are `export-<uuid>.tmp` inside the log directory.
pub const STAGING_PREFIX: &str = "export-";
pub const STAGING_SUFFIX: &str = ".tmp";
/// Bounds the name part of temporary files, as `fsutil` does, so names stay within 255 bytes.
const SHORT_NAME_BYTES: usize = 180;
const NEWLINE: u8 = b'\n';

/// What the native commands report. `storage_error` is set by the writer on a failed append,
/// rotation or read, and cleared by the next successful write, clear or enable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub retained_bytes: u64,
    pub dropped: u64,
    pub storage_error: bool,
}

/// Why a storage operation failed. `Destination` is the user's export target; `Storage` is the
/// log directory, its files, or a staged snapshot that no longer exists (for example after a clear).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Storage,
    Destination,
}

/// One staged export snapshot, owned by the export that created it. Native only: not `Clone` and
/// not serializable. Persisting or discarding consumes it. A handle dropped unconsumed (a cancelled
/// command future, a panic) asks the writer to discard exactly its own snapshot.
pub struct StagedExport {
    path: Option<PathBuf>,
    shared: Arc<Shared>,
}

impl Drop for StagedExport {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            self.shared.request_discard(path);
        }
    }
}

struct Entry {
    generation: u64,
    line: String,
}

enum Item {
    Record(Entry),
    Control(Control),
}

enum Control {
    Stage {
        reply: Sender<Result<PathBuf, Failure>>,
    },
    Clear {
        reply: Sender<Result<(), Failure>>,
    },
    /// Discards exactly one snapshot, for a handle dropped unconsumed.
    Discard {
        path: PathBuf,
    },
}

enum Work {
    Records(Vec<Entry>),
    Control(Control),
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    records: usize,
    generation: u64,
    storage_error: bool,
    /// Staged snapshots that have not been persisted or discarded, by path.
    staged: Vec<PathBuf>,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    enabled: AtomicBool,
    dropped: AtomicU64,
    retained: AtomicU64,
    worker: AtomicBool,
    header: String,
    storage: Mutex<Storage>,
}

pub struct DiagnosticsLog {
    directory: PathBuf,
    shared: Arc<Shared>,
}

impl DiagnosticsLog {
    /// Creates no files and starts no thread. Call `spawn_worker` once to drain in the background.
    pub fn new(directory: PathBuf, header: String) -> Self {
        let shared = Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            enabled: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            retained: AtomicU64::new(0),
            worker: AtomicBool::new(false),
            header,
            storage: Mutex::new(Storage::new(directory.clone())),
        };
        Self {
            directory,
            shared: Arc::new(shared),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Starts the single writer thread. Repeated calls are no-ops.
    pub fn spawn_worker(&self) {
        if self.shared.worker.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name("peppy-diagnostics-writer".to_owned())
            .spawn(move || run_worker(&shared));
        if spawned.is_err() {
            // Without a writer, control requests are processed on the caller's thread instead.
            self.shared.worker.store(false, Ordering::Release);
        }
    }

    pub fn enabled(&self) -> bool {
        self.shared.enabled.load(Ordering::Acquire)
    }

    /// Changing the switch invalidates every record accepted before it, and clears a storage error.
    /// Waits for a batch that is being appended, so nothing is written after this returns.
    pub fn set_enabled(&self, enabled: bool) {
        let _storage = lock(&self.shared.storage);
        let mut queue = lock(&self.shared.queue);
        queue.generation = queue.generation.wrapping_add(1);
        queue.storage_error = false;
        self.shared.enabled.store(enabled, Ordering::Release);
    }

    /// Never blocks on file IO. Invalid or oversize records, records refused by a full queue, and
    /// records whose recording was switched off while in flight are counted as dropped.
    pub fn accept(&self, line: String) {
        if !self.enabled() {
            return;
        }
        if !fits(line.len()) || !is_valid_record(&line) {
            self.shared.count_dropped();
            return;
        }
        let mut queue = lock(&self.shared.queue);
        if !self.enabled() || queue.records >= QUEUE_CAPACITY {
            drop(queue);
            self.shared.count_dropped();
            return;
        }
        let generation = queue.generation;
        queue
            .items
            .push_back(Item::Record(Entry { generation, line }));
        queue.records += 1;
        drop(queue);
        self.shared.wake.notify_one();
    }

    pub fn status(&self) -> Status {
        let storage_error = lock(&self.shared.queue).storage_error;
        Status {
            enabled: self.enabled(),
            retained_bytes: self.shared.retained.load(Ordering::Acquire),
            dropped: self.shared.dropped.load(Ordering::Acquire),
            storage_error,
        }
    }

    /// Writes a new snapshot after every record accepted before this call and returns the handle
    /// that owns it.
    pub fn stage_export(&self) -> Result<StagedExport, Failure> {
        let (reply, receiver) = mpsc::channel();
        self.submit(Control::Stage { reply }, false);
        self.finish(receiver).map(|path| StagedExport {
            path: Some(path),
            shared: Arc::clone(&self.shared),
        })
    }

    /// Moves this export's snapshot to the user's destination. Fails with `Storage` when the
    /// snapshot no longer exists, for example because a clear ran during the save panel. Other
    /// staged snapshots are never touched.
    pub fn persist_export(
        &self,
        mut staged: StagedExport,
        destination: &Path,
    ) -> Result<(), Failure> {
        let _storage = lock(&self.shared.storage);
        let Some(path) = staged.path.take() else {
            return Err(Failure::Storage);
        };
        if !take_staged(&self.shared.queue, &path) {
            return Err(Failure::Storage);
        }
        let result = persist(&path, destination).map_err(|_| Failure::Destination);
        let _ = fs::remove_file(&path);
        result
    }

    /// Removes this export's snapshot when the user cancels. Other staged snapshots are untouched.
    pub fn discard_staged(&self, mut staged: StagedExport) {
        let _storage = lock(&self.shared.storage);
        if let Some(path) = staged.path.take() {
            if take_staged(&self.shared.queue, &path) {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// Discards records accepted before the call, then deletes the retained logs and every staged
    /// snapshot. Recording stays on. Replies only after the cached size is published.
    pub fn clear(&self) -> Result<(), Failure> {
        let (reply, receiver) = mpsc::channel();
        {
            let _storage = lock(&self.shared.storage);
            self.submit(Control::Clear { reply }, true);
        }
        self.finish(receiver)
    }

    /// Writes queued work on the calling thread, for tests that drain deterministically. Without a
    /// worker, `finish` does the same on the caller's thread.
    #[cfg(test)]
    pub fn process_pending(&self) {
        self.shared.process_pending();
    }

    #[cfg(test)]
    pub fn queued_records(&self) -> usize {
        lock(&self.shared.queue).records
    }

    #[cfg(test)]
    pub fn queued_items(&self) -> usize {
        lock(&self.shared.queue).items.len()
    }

    /// Stages a snapshot whose requester has already gone, so its reply cannot be delivered.
    /// Test-only seam: it drops the receiver before the writer replies.
    #[cfg(test)]
    pub fn stage_with_lost_requester(&self) {
        let (reply, receiver) = mpsc::channel();
        drop(receiver);
        self.submit(Control::Stage { reply }, false);
        self.process_pending();
    }

    /// Dequeues one batch, runs `hook` (the barrier), then appends the batch. Test-only.
    #[cfg(test)]
    pub fn process_one_with_hook(&self, hook: impl FnOnce(&Self)) {
        let Some(work) = self.shared.try_work() else {
            return;
        };
        hook(self);
        self.shared.apply(work);
    }

    #[cfg(test)]
    pub fn export_to(&self, destination: &Path) -> Result<(), Failure> {
        let staged = self.stage_export()?;
        self.persist_export(staged, destination)
    }

    fn submit(&self, control: Control, new_generation: bool) {
        let mut queue = lock(&self.shared.queue);
        if new_generation {
            queue.generation = queue.generation.wrapping_add(1);
        }
        queue.items.push_back(Item::Control(control));
        drop(queue);
        self.shared.wake.notify_one();
    }

    fn finish<T>(&self, receiver: Receiver<Result<T, Failure>>) -> Result<T, Failure> {
        if !self.shared.worker.load(Ordering::Acquire) {
            self.shared.process_pending();
        }
        receiver.recv().unwrap_or(Err(Failure::Storage))
    }
}

/// Removes `path` from the live staged snapshots. Returns `false` when it is no longer staged.
fn take_staged(queue: &Mutex<Queue>, path: &Path) -> bool {
    let mut queue = lock(queue);
    match queue.staged.iter().position(|staged| staged == path) {
        Some(index) => {
            queue.staged.remove(index);
            true
        }
        None => false,
    }
}

fn run_worker(shared: &Shared) {
    // First use: repair retained files before the first write.
    shared.apply(Work::Records(Vec::new()));
    loop {
        let work = shared.wait_work();
        shared.apply(work);
    }
}

impl Shared {
    fn count_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }

    /// Queues a discard for a snapshot whose handle was dropped unconsumed. The caller's thread
    /// does no filesystem work.
    fn request_discard(&self, path: PathBuf) {
        lock(&self.queue)
            .items
            .push_back(Item::Control(Control::Discard { path }));
        self.wake.notify_one();
    }

    fn set_storage_error(&self, value: bool) {
        lock(&self.queue).storage_error = value;
    }

    fn wait_work(&self) -> Work {
        let mut queue = lock(&self.queue);
        loop {
            if let Some(work) = self.take_locked(&mut queue) {
                return work;
            }
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn try_work(&self) -> Option<Work> {
        let mut queue = lock(&self.queue);
        self.take_locked(&mut queue)
    }

    /// Takes a run of records (at most one batch) or one control item, in arrival order. The
    /// generation is checked later, under the storage lock, when the batch is appended.
    fn take_locked(&self, queue: &mut Queue) -> Option<Work> {
        let mut batch = Vec::new();
        let mut took = false;
        while let Some(front) = queue.items.front() {
            if matches!(front, Item::Control(_)) {
                if !batch.is_empty() {
                    break;
                }
                return match queue.items.pop_front() {
                    Some(Item::Control(control)) => Some(Work::Control(control)),
                    _ => None,
                };
            }
            if batch.len() == BATCH_SIZE {
                break;
            }
            let Some(Item::Record(entry)) = queue.items.pop_front() else {
                break;
            };
            took = true;
            queue.records -= 1;
            batch.push(entry);
        }
        took.then_some(Work::Records(batch))
    }

    fn process_pending(&self) {
        self.apply(Work::Records(Vec::new()));
        while let Some(work) = self.try_work() {
            self.apply(work);
        }
    }

    /// Keeps the entries that are still current and enabled, and counts the rest as dropped. Runs
    /// under the storage lock, so no disable or clear can interleave with the check and the append.
    fn admit(&self, entries: Vec<Entry>) -> Vec<String> {
        let generation = lock(&self.queue).generation;
        let enabled = self.enabled.load(Ordering::Acquire);
        entries
            .into_iter()
            .filter_map(|entry| {
                if entry.generation == generation && enabled {
                    Some(entry.line)
                } else {
                    self.count_dropped();
                    None
                }
            })
            .collect()
    }

    /// Publishes the cached retained size. Called before any reply, so a caller that waits for a
    /// reply reads the size of the state it caused.
    fn publish(&self, storage: &Storage) {
        self.retained
            .store(storage.retained_bytes(), Ordering::Release);
    }

    /// Only the writer thread (or `process_pending`) runs this, so storage work stays ordered with
    /// the queue. Storage is locked before the queue, never the reverse.
    fn apply(&self, work: Work) {
        let mut storage = lock(&self.storage);
        match work {
            Work::Records(entries) => {
                let lines = self.admit(entries);
                match storage.append_records(&lines) {
                    Ok(()) if lines.is_empty() => {}
                    Ok(()) => self.set_storage_error(false),
                    Err(_) => {
                        storage.recover();
                        self.set_storage_error(true);
                    }
                }
                self.publish(&storage);
            }
            Work::Control(Control::Stage { reply }) => {
                let state = state_record(
                    self.dropped.load(Ordering::Acquire),
                    self.enabled.load(Ordering::Acquire),
                );
                let result = match storage.stage_snapshot(&self.header, &state) {
                    Ok(staged) => {
                        lock(&self.queue).staged.push(staged.clone());
                        Ok(staged)
                    }
                    Err(_) => {
                        storage.recover();
                        self.set_storage_error(true);
                        Err(Failure::Storage)
                    }
                };
                self.publish(&storage);
                // A requester that has gone cannot persist or discard its snapshot: remove it now.
                if let Err(mpsc::SendError(Ok(path))) = reply.send(result) {
                    if take_staged(&self.queue, &path) {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
            Work::Control(Control::Discard { path }) => {
                if take_staged(&self.queue, &path) {
                    let _ = fs::remove_file(&path);
                }
            }
            Work::Control(Control::Clear { reply }) => {
                let result = storage.clear().map_err(|_| Failure::Storage);
                // Every staged snapshot is gone from the queue, even when a removal failed.
                lock(&self.queue).staged.clear();
                match result {
                    Ok(()) => self.set_storage_error(false),
                    Err(_) => {
                        storage.recover();
                        self.set_storage_error(true);
                    }
                }
                self.publish(&storage);
                let _ = reply.send(result);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A record fits when its length plus the newline is at most `MAX_RECORD_BYTES`.
fn fits(length: usize) -> bool {
    length < MAX_RECORD_BYTES
}

fn line_len(text: &str) -> u64 {
    text.len() as u64 + 1
}

fn state_record(dropped: u64, enabled: bool) -> String {
    format!(
        "event={EVENT_DIAGNOSTIC_STATE} session=0 timestamp_ms=0 trace=0 {FIELD_DROPPED}={dropped} {FIELD_ENABLED}={}",
        u8::from(enabled)
    )
}

/// Matches `event=\d+ session=\d+ timestamp_ms=\d+ trace=\d+(?: \d+=-?\d+)*`, the Android pattern.
pub fn is_valid_record(line: &str) -> bool {
    let mut fields = line.split(' ');
    let fixed = ["event", "session", "timestamp_ms", "trace"];
    let fixed_ok = fixed.iter().all(|key| {
        fields
            .next()
            .and_then(|field| field.split_once('='))
            .is_some_and(|(name, value)| name == *key && is_digits(value))
    });
    fixed_ok && fields.all(is_field)
}

fn is_field(field: &str) -> bool {
    field.split_once('=').is_some_and(|(code, value)| {
        is_digits(code) && (is_digits(value) || value.strip_prefix('-').is_some_and(is_digits))
    })
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_valid_bytes(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(is_valid_record)
}

fn is_staged_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(STAGING_PREFIX) && name.ends_with(STAGING_SUFFIX))
}

struct Storage {
    directory: PathBuf,
    current_len: u64,
    previous_len: u64,
    repaired: bool,
    /// Orphaned snapshots from an earlier run are removed once, before this run stages any.
    swept: bool,
}

impl Storage {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            current_len: 0,
            previous_len: 0,
            repaired: false,
            swept: false,
        }
    }

    fn retained_bytes(&self) -> u64 {
        self.current_len + self.previous_len
    }

    fn current_path(&self) -> PathBuf {
        self.directory.join(CURRENT_FILE)
    }

    fn previous_path(&self) -> PathBuf {
        self.directory.join(PREVIOUS_FILE)
    }

    /// Re-reads sizes from disk after a failure, so cached sizes never trust a partial write.
    fn refresh_lengths(&mut self) {
        self.previous_len = file_len(&self.previous_path());
        self.current_len = file_len(&self.current_path());
    }

    fn recover(&mut self) {
        self.repaired = false;
        self.refresh_lengths();
    }

    /// Runs once per process, on the first storage call (the writer's startup pass, even with
    /// recording off). Removes orphaned snapshots, then repairs retained files. Never creates the
    /// directory.
    fn ensure_repaired(&mut self) -> io::Result<()> {
        if !self.swept {
            self.swept = true;
            // Best effort: an orphan that survives here is still removed by the next clear.
            let _ = self.remove_orphans();
        }
        if self.repaired {
            return Ok(());
        }
        if self.directory.is_dir() {
            self.previous_len = repair_file(&self.previous_path())?;
            self.current_len = repair_file(&self.current_path())?;
        }
        self.repaired = true;
        Ok(())
    }

    fn append_records(&mut self, lines: &[String]) -> io::Result<()> {
        self.ensure_repaired()?;
        if lines.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.directory)?;
        let mut output: Option<BufWriter<File>> = None;
        for line in lines {
            let size = line_len(line);
            if self.current_len + size > MAX_FILE_BYTES {
                if let Some(mut writer) = output.take() {
                    writer.flush()?;
                }
                self.rotate()?;
            }
            if output.is_none() {
                output = Some(BufWriter::new(open_append(&self.current_path())?));
            }
            if let Some(writer) = output.as_mut() {
                writer.write_all(line.as_bytes())?;
                writer.write_all(b"\n")?;
            }
            self.current_len += size;
        }
        if let Some(mut writer) = output {
            writer.flush()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        remove_if_present(&self.previous_path())?;
        if self.current_path().exists() {
            fs::rename(self.current_path(), self.previous_path())?;
        }
        self.previous_len = self.current_len;
        self.current_len = 0;
        Ok(())
    }

    /// Writes the snapshot (header, state record, newest retained records within the 4 MiB cap)
    /// to a new private staging file and returns its path.
    fn stage_snapshot(&mut self, header: &str, state: &str) -> io::Result<PathBuf> {
        self.ensure_repaired()?;
        fs::create_dir_all(&self.directory)?;
        let budget = MAX_TOTAL_BYTES.saturating_sub(line_len(header) + line_len(state));
        let records = self.newest_records(budget)?;
        let staged = self.directory.join(format!(
            "{STAGING_PREFIX}{}{STAGING_SUFFIX}",
            uuid::Uuid::new_v4().simple()
        ));
        match write_snapshot(&staged, header, state, &records) {
            Ok(()) => Ok(staged),
            Err(error) => {
                let _ = fs::remove_file(&staged);
                Err(error)
            }
        }
    }

    fn newest_records(&self, budget: u64) -> io::Result<Vec<String>> {
        let mut lines = read_valid_lines(&self.previous_path())?;
        lines.extend(read_valid_lines(&self.current_path())?);
        let mut total: u64 = lines.iter().map(|line| line_len(line)).sum();
        let mut first = 0;
        while total > budget && first < lines.len() {
            total -= line_len(&lines[first]);
            first += 1;
        }
        Ok(lines.split_off(first))
    }

    /// Staged snapshots are found by name on disk too, so a file orphaned by an earlier failure
    /// is removed as well.
    fn staged_files(&self) -> io::Result<Vec<PathBuf>> {
        if !self.directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if is_staged_path(&path) {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn remove_orphans(&self) -> io::Result<()> {
        for path in self.staged_files()? {
            remove_if_present(&path)?;
        }
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        let mut paths = vec![self.previous_path(), self.current_path()];
        let mut failure = None;
        match self.staged_files() {
            Ok(files) => paths.extend(files),
            Err(error) => failure = Some(error),
        }
        for path in paths {
            if let Err(error) = remove_if_present(&path) {
                failure = failure.or(Some(error));
            }
        }
        self.repaired = true;
        self.refresh_lengths();
        failure.map_or(Ok(()), Err)
    }
}

fn file_len(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => 0,
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Creates a new file readable and writable by the owner only, as `fsutil` does for its files.
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Valid, newline-terminated records of one file, oldest first.
fn read_valid_lines(path: &Path) -> io::Result<Vec<String>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    Ok(bytes
        .split_inclusive(|byte| *byte == NEWLINE)
        .filter_map(|segment| {
            let line = match segment.split_last() {
                Some((&NEWLINE, line)) => line,
                _ => return None,
            };
            (fits(line.len()) && is_valid_bytes(line))
                .then(|| String::from_utf8_lossy(line).into_owned())
        })
        .collect())
}

/// Drops invalid, oversized and unterminated lines and trims to the cap, keeping the newest.
/// Rewrites the file only when something changed. Returns the repaired size.
fn repair_file(path: &Path) -> io::Result<u64> {
    if !path.is_file() {
        return Ok(0);
    }
    let bytes = fs::read(path)?;
    let mut kept: VecDeque<&[u8]> = VecDeque::new();
    let mut kept_bytes = 0u64;
    let mut repair = false;
    for segment in bytes.split_inclusive(|byte| *byte == NEWLINE) {
        let line = match segment.split_last() {
            Some((&NEWLINE, line)) => line,
            _ => {
                // Unterminated tail: discarded.
                repair = true;
                break;
            }
        };
        if !fits(line.len()) || !is_valid_bytes(line) {
            repair = true;
            continue;
        }
        kept_bytes += segment.len() as u64;
        kept.push_back(segment);
    }
    while kept_bytes > MAX_FILE_BYTES {
        let Some(oldest) = kept.pop_front() else {
            break;
        };
        kept_bytes -= oldest.len() as u64;
        repair = true;
    }
    if repair {
        let mut content = Vec::with_capacity(kept_bytes as usize);
        for segment in &kept {
            content.extend_from_slice(segment);
        }
        crate::fsutil::write_private_atomic(path, &content)?;
    }
    Ok(kept_bytes)
}

fn write_snapshot(path: &Path, header: &str, state: &str, records: &[String]) -> io::Result<()> {
    let mut output = BufWriter::new(create_private(path)?);
    let lines = [header, state]
        .into_iter()
        .chain(records.iter().map(String::as_str));
    for line in lines {
        output.write_all(line.as_bytes())?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    output.get_ref().sync_all()
}

/// Moves the staged snapshot into place. When a rename cannot cross volumes, it copies beside the
/// destination first, so a failed export never leaves a partial file at the destination.
fn persist(staged: &Path, destination: &Path) -> io::Result<()> {
    if fs::rename(staged, destination).is_ok() {
        return Ok(());
    }
    let temporary = beside(destination);
    let result =
        copy_private(staged, &temporary).and_then(|()| fs::rename(&temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn copy_private(source: &Path, destination: &Path) -> io::Result<()> {
    let mut output = create_private(destination)?;
    io::copy(&mut File::open(source)?, &mut output)?;
    output.sync_all()
}

/// Temporary path next to `destination`, with its name part bounded to `SHORT_NAME_BYTES` so the
/// file name stays within 255 bytes however long the user's name is.
pub(super) fn beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "diagnostics".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(
        ".{}.{}.tmp",
        bounded_prefix(&name, SHORT_NAME_BYTES),
        uuid::Uuid::new_v4().simple()
    ))
}

fn bounded_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
