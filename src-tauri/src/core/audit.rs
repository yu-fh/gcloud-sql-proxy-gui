//! The audit log: an append-only record of everything the app did and why.
//!
//! The proxy's own stdout answers "what did `cloud-sql-proxy` print". It does
//! not answer "who started prd at 14:02", "what did preflight object to", or
//! "which gcloud account was this run under" -- and those are the questions
//! anyone debugging a Cloud SQL session after the fact actually has. So this
//! module records four kinds of thing, all into one stream so their ordering
//! relative to each other survives:
//!
//! 1. **User actions** -- started/stopped a profile, saved and what changed,
//!    added/renamed/deleted, confirmed or cancelled a production start, opened
//!    the settings window.
//! 2. **System events** -- preflight outcomes, the exact spawn argv, child
//!    exits, status transitions, menu rebuilds.
//! 3. **System info** -- recorded once at startup: versions, resolved binary
//!    paths, the gcloud account, the config path.
//! 4. **Proxy output** -- every line the child wrote, which is what the log
//!    used to be and only.
//!
//! # No redaction
//!
//! The account email, the GCP project ids, and the full connection names are
//! written in the clear. That is deliberate and the user's explicit choice: the
//! file is local, and a log that elides the connection name cannot answer the
//! question it exists for. The one hard line is that no *credential* is ever
//! recorded -- see [`Logger::system_info`], which runs `gcloud config
//! get-value account` and never reads the ADC token next to it.
//!
//! # No Tauri, and no new dependency
//!
//! `core::` must stay Tauri-free, and this module also adds no crate. The
//! timestamp is formatted from [`SystemTime`] by [`format_utc`] -- about thirty
//! lines of civil-date arithmetic -- in preference to pulling `time` or
//! `chrono` in:
//!
//! * `time` is already in the build graph (via `cookie` -> `tauri`) but only at
//!   0.3.55, which needs rustc 1.88; declaring it as a direct dependency under
//!   this crate's `rust-version = "1.77"` resolves it to 0.3.41 instead and so
//!   puts *two* copies of `time` in the tree.
//! * The feature that would be wanted, `local-offset`, is documented as
//!   returning `Err` on Unix unless the process is single-threaded. This app is
//!   multi-threaded before the first record is written, so it would never
//!   succeed anyway.
//!
//! Records therefore carry UTC with an explicit `Z`. The webview knows the
//! user's timezone for free and renders local time from it, which is both
//! correct and the only place a timezone is actually needed.
//!
//! # Locking
//!
//! The shared state's documented order is `config` before `manager`
//! (see `app_state::Shared`). This logger deliberately does **not** join that
//! order as a third async mutex: it uses a `std::sync::Mutex` held for the
//! duration of a `Vec::push` and a `Vec::truncate` and nothing else. No
//! `.await` happens inside it and no other lock is taken while it is held, so
//! it cannot participate in a cycle with either of those two -- a task holding
//! it always releases it before it can block on anything.
//!
//! # The file write is not on the caller's path
//!
//! [`Logger::log`] appends to the in-memory ring and pushes the formatted line
//! onto a queue. A single writer thread owns the file handle and drains that
//! queue. So a caller never touches the filesystem, never blocks the async
//! runtime on a `write`, and a slow or wedged disk shows up as a growing queue
//! rather than as a stalled UI. Writes are flushed per batch, so a crash loses
//! at most whatever the writer had not yet picked up.

use std::collections::VecDeque;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many records the in-memory view keeps. Matches the cap the proxy log
/// buffer used to enforce on its own: a proxy left running all day would
/// otherwise grow this without bound, and the Logs view only ever renders the
/// tail of it anyway. The file keeps everything (up to rotation).
pub const DEFAULT_MEMORY_CAP: usize = 2000;

/// Rotate once the active file passes this size, and keep this many older
/// generations beside it.
///
/// 2 MiB x (1 active + 3 rotated) is a hard ceiling of 8 MiB on disk. The sizing
/// is from what the app actually emits: a record is ~120 bytes, and the proxy is
/// the only high-volume source, so 2 MiB is on the order of 17,000 records --
/// days of ordinary use, and still small enough to open in Console.app or mail
/// to a colleague without thinking about it. Three generations means a session
/// that spews (a proxy in a reconnect loop) cannot bury the startup system-info
/// block from the session before it.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_GENERATIONS: usize = 3;

/// The macOS convention for an app's own logs, and where Console.app looks:
/// `~/Library/Logs/<bundle id>/`.
const LOG_DIR_NAME: &str = "ai.firsthand.fh-cloud-sql-proxy-gui";
const LOG_FILE_NAME: &str = "audit.log";

/// How much the writer thread will let the queue grow before it starts
/// discarding.
///
/// A bound is what keeps a wedged disk from turning into unbounded memory
/// growth. It is generous on purpose -- 50,000 records is far more than any
/// burst this app produces -- so in practice nothing is ever dropped; if
/// something is, the writer says so in the file rather than dropping silently.
const QUEUE_LIMIT: usize = 50_000;

/// How urgent a record is. The Logs view renders these distinctly, so an error
/// is findable without reading every line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    /// The wire and on-disk spelling. Lowercase because it is also a CSS class
    /// suffix and a filter value in the webview.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which of the four streams a record belongs to, plus the finer grain within
/// them that makes the file skimmable.
///
/// A category rather than a free-text tag so the on-disk spelling cannot drift
/// between call sites, and so the view can group without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Recorded once at startup: versions, paths, the gcloud account.
    System,
    /// Something the user did: a click, a save, a confirmation.
    Action,
    /// Preflight, spawn, exit, status transitions, menu rebuilds.
    Event,
    /// A line the `cloud-sql-proxy` child wrote.
    Proxy,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::System => "system",
            Category::Action => "action",
            Category::Event => "event",
            Category::Proxy => "proxy",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of the audit trail.
///
/// `profile_id` is optional because three of the four categories have records
/// that belong to no particular profile (a system-info line, the settings
/// window opening, a menu rebuild). Keeping it as `Option` rather than an empty
/// string is what lets the view's profile filter distinguish "not about a
/// profile" from "about the profile whose id is empty", which cannot exist but
/// would be indistinguishable otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Milliseconds since the Unix epoch. The wire carries this as well as the
    /// formatted string, so the view can sort and re-format without parsing.
    pub at_ms: u64,
    pub severity: Severity,
    pub category: Category,
    pub profile_id: Option<String>,
    pub message: String,
}

impl Record {
    /// The on-disk line: fixed-width-ish leading columns so the file is
    /// readable in a pager, then the message verbatim.
    ///
    /// Newlines inside `message` are escaped rather than written through. A
    /// proxy line with an embedded newline would otherwise produce a second
    /// physical line with no timestamp, which is exactly the thing that makes a
    /// log file un-greppable.
    pub fn to_line(&self) -> String {
        let profile = self.profile_id.as_deref().unwrap_or("-");
        format!(
            "{} {:<5} {:<6} {:<12} {}",
            format_utc(self.at_ms),
            self.severity,
            self.category,
            profile,
            escape(&self.message)
        )
    }
}

/// Replace the characters that would break the one-record-per-line contract.
fn escape(message: &str) -> String {
    if !message.contains(['\n', '\r']) {
        return message.to_string();
    }
    message.replace('\r', "\\r").replace('\n', "\\n")
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
///
/// Hand-rolled rather than pulled from a crate; see the module docs on why.
/// The civil-from-days conversion is Howard Hinnant's, which is the same
/// algorithm the date libraries use, and it is exercised against known epochs
/// and leap-year boundaries in the tests below.
pub fn format_utc(at_ms: u64) -> String {
    let secs = (at_ms / 1000) as i64;
    let millis = at_ms % 1000;

    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since 1970-01-01 to a civil (proleptic Gregorian) date.
///
/// Hinnant's `civil_from_days`: shift the epoch to 0000-03-01 so leap days fall
/// at the end of the era, then divide into 400-year eras where the day count is
/// exactly 146097.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + i64::from(m <= 2), m as u32, d as u32)
}

/// Milliseconds since the epoch, now.
///
/// A clock set before 1970 yields 0 rather than an error: the timestamp is
/// context on a log line, and refusing to record the line because the clock is
/// absurd would lose the thing the user actually wanted.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where the audit log lives: `~/Library/Logs/<bundle id>/audit.log` on macOS.
///
/// `dirs::home_dir` is the only thing that can fail here, and only on a system
/// with no home directory, in which case there is nowhere sensible to write and
/// the logger runs memory-only.
pub fn default_log_path() -> Option<PathBuf> {
    // `~/Library/Logs` is the macOS convention and what Console.app shows.
    // `dirs` has no accessor for it (it is not an XDG concept), so it is built
    // from the home directory. On a non-macOS build this still produces a
    // stable per-user location, which is all the tests need.
    #[cfg(target_os = "macos")]
    let base = dirs::home_dir().map(|home| home.join("Library").join("Logs"));
    #[cfg(not(target_os = "macos"))]
    let base = dirs::data_local_dir().or_else(dirs::home_dir);

    base.map(|dir| dir.join(LOG_DIR_NAME).join(LOG_FILE_NAME))
}

/// The queue the writer thread drains, plus the counters that let the logger
/// report on the writer without blocking on it.
struct Sink {
    /// Formatted lines awaiting a write. `VecDeque` because the writer takes
    /// from the front in batches.
    queue: Mutex<VecDeque<String>>,
    /// Woken when a line is queued or when a shutdown is requested.
    ready: Condvar,
    shutdown: AtomicBool,
    /// Lines discarded because the queue hit [`QUEUE_LIMIT`]. Reported into the
    /// file by the writer rather than dropped silently.
    dropped: AtomicU64,
    /// Write failures the writer has seen. Read by [`Logger::write_failures`]
    /// so a test (and, if it is ever wanted, the UI) can tell that persistence
    /// is degraded without the logger itself ever touching the disk.
    failures: AtomicU64,
}

/// The append-only logger. Cheap to clone -- it is an `Arc` inside -- because
/// the tray poll loop, every command, and `ProxyManager` all hold one.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    /// The in-memory view, newest last, capped at `memory_cap`.
    records: Mutex<Vec<Record>>,
    memory_cap: usize,
    /// `None` when there is nowhere to write (no home directory, or an
    /// explicitly memory-only logger). Everything else still works.
    sink: Option<Arc<Sink>>,
    path: Option<PathBuf>,
}

impl Logger {
    /// A logger that persists to `path`, rotating at [`DEFAULT_MAX_BYTES`].
    ///
    /// Never fails. If the directory cannot be created or the file cannot be
    /// opened, the returned logger keeps its in-memory view and its writer
    /// thread reports failures through [`Logger::write_failures`] -- a menu bar
    /// app must not die because `~/Library/Logs` is read-only.
    pub fn to_file(path: PathBuf) -> Self {
        Self::with_policy(path, DEFAULT_MEMORY_CAP, DEFAULT_MAX_BYTES, DEFAULT_GENERATIONS)
    }

    /// A logger at the platform's conventional location, or memory-only if that
    /// cannot be resolved.
    pub fn at_default_path() -> Self {
        match default_log_path() {
            Some(path) => Self::to_file(path),
            None => Self::memory_only(),
        }
    }

    /// A logger that keeps records in memory and writes nothing. Used by tests
    /// and by any build with no writable home directory.
    pub fn memory_only() -> Self {
        Self {
            inner: Arc::new(Inner {
                records: Mutex::new(Vec::new()),
                memory_cap: DEFAULT_MEMORY_CAP,
                sink: None,
                path: None,
            }),
        }
    }

    /// Full control over the caps, for tests that need rotation to happen in
    /// bytes they can afford to write.
    pub fn with_policy(
        path: PathBuf,
        memory_cap: usize,
        max_bytes: u64,
        generations: usize,
    ) -> Self {
        let sink = Arc::new(Sink {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            shutdown: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        });

        spawn_writer(Arc::clone(&sink), path.clone(), max_bytes, generations);

        Self {
            inner: Arc::new(Inner {
                records: Mutex::new(Vec::new()),
                memory_cap,
                sink: Some(sink),
                path: Some(path),
            }),
        }
    }

    /// Where records are being written, if anywhere. The Logs view shows this
    /// so the user can find the file.
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    /// Record one line.
    ///
    /// Synchronous and non-blocking on I/O: it appends to the in-memory ring
    /// and hands the formatted line to the writer thread. Safe to call from an
    /// async task, from a menu click handler on the main thread, and from the
    /// proxy reader tasks.
    pub fn log(
        &self,
        severity: Severity,
        category: Category,
        profile_id: Option<&str>,
        message: impl Into<String>,
    ) {
        let record = Record {
            at_ms: now_ms(),
            severity,
            category,
            profile_id: profile_id.map(str::to_string),
            message: message.into(),
        };

        if let Some(sink) = &self.inner.sink {
            let line = record.to_line();
            // Held across a push and at most a `pop_front`, never across an
            // `.await` or another lock. See the module docs on locking.
            if let Ok(mut queue) = sink.queue.lock() {
                if queue.len() >= QUEUE_LIMIT {
                    // Drop the oldest rather than the newest: the newest line
                    // is the one someone is currently trying to explain.
                    queue.pop_front();
                    sink.dropped.fetch_add(1, Ordering::Relaxed);
                }
                queue.push_back(line);
                sink.ready.notify_one();
            }
        }

        if let Ok(mut records) = self.inner.records.lock() {
            // A zero cap means "keep nothing", which is a legitimate
            // configuration and must not underflow the drain below.
            if self.inner.memory_cap == 0 {
                records.clear();
            } else {
                records.push(record);
                if records.len() > self.inner.memory_cap {
                    let excess = records.len() - self.inner.memory_cap;
                    records.drain(..excess);
                }
            }
        }
    }

    pub fn info(&self, category: Category, profile_id: Option<&str>, message: impl Into<String>) {
        self.log(Severity::Info, category, profile_id, message);
    }

    pub fn warn(&self, category: Category, profile_id: Option<&str>, message: impl Into<String>) {
        self.log(Severity::Warn, category, profile_id, message);
    }

    pub fn error(&self, category: Category, profile_id: Option<&str>, message: impl Into<String>) {
        self.log(Severity::Error, category, profile_id, message);
    }

    /// Every retained record, oldest first.
    pub fn records(&self) -> Vec<Record> {
        self.inner
            .records
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Retained records filtered by minimum severity and/or profile.
    ///
    /// `min_severity` is inclusive, so `Warn` yields warnings and errors. A
    /// `profile_id` of `Some(id)` keeps only records tagged with that profile;
    /// records belonging to no profile are excluded, because a user filtering to
    /// "dev" is asking about dev and not about the startup banner.
    pub fn filtered(&self, min_severity: Option<Severity>, profile_id: Option<&str>) -> Vec<Record> {
        self.records()
            .into_iter()
            .filter(|record| match min_severity {
                Some(min) => record.severity >= min,
                None => true,
            })
            .filter(|record| match profile_id {
                Some(want) => record.profile_id.as_deref() == Some(want),
                None => true,
            })
            .collect()
    }

    /// How many lines the writer thread failed to persist. Zero on a healthy
    /// logger; non-zero means the in-memory view is still complete but the file
    /// is not.
    pub fn write_failures(&self) -> u64 {
        self.inner
            .sink
            .as_ref()
            .map(|sink| sink.failures.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Block until the writer has drained everything queued so far.
    ///
    /// Only for tests and for the exit path: ordinary logging never waits on the
    /// disk. Gives up after `timeout` rather than hanging if the writer is
    /// wedged, because an app that will not quit is worse than a log missing its
    /// last line.
    pub fn flush_blocking(&self, timeout: Duration) {
        let Some(sink) = &self.inner.sink else {
            return;
        };
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let empty = sink.queue.lock().map(|q| q.is_empty()).unwrap_or(true);
            if empty {
                return;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Ask the writer thread to finish and stop. Records already queued are still
/// written; the thread exits once the queue is empty.
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(sink) = &self.sink {
            sink.shutdown.store(true, Ordering::Relaxed);
            sink.ready.notify_all();
        }
    }
}

/// The one thread that owns the file handle.
///
/// A dedicated thread rather than a tokio task on purpose: the work is blocking
/// file I/O, and putting it on the async runtime would be exactly the stall this
/// design exists to avoid. It is a plain OS thread, so it is also alive before
/// the runtime starts and after it shuts down -- which matters, because startup
/// and exit are both things worth logging.
fn spawn_writer(sink: Arc<Sink>, path: PathBuf, max_bytes: u64, generations: usize) {
    std::thread::Builder::new()
        .name("audit-writer".to_string())
        .spawn(move || writer_loop(sink, path, max_bytes, generations))
        // A machine that cannot spawn a thread has larger problems; the logger
        // degrades to memory-only rather than panicking.
        .map(|_| ())
        .unwrap_or(());
}

fn writer_loop(sink: Arc<Sink>, path: PathBuf, max_bytes: u64, generations: usize) {
    let mut state = WriterState::open(&path);

    loop {
        let batch = {
            let Ok(mut queue) = sink.queue.lock() else {
                return;
            };
            while queue.is_empty() {
                if sink.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                // A timeout rather than a plain wait so a lost notification
                // (or a shutdown flag set between the check and the wait)
                // cannot park this thread forever.
                let Ok((guard, _)) = sink.ready.wait_timeout(queue, Duration::from_millis(250))
                else {
                    return;
                };
                queue = guard;
            }
            // Drain everything pending: one `write` for a burst rather than one
            // per line, which is what keeps a spewing proxy cheap.
            queue.drain(..).collect::<Vec<String>>()
        };

        let dropped = sink.dropped.swap(0, Ordering::Relaxed);
        let mut buffer = String::with_capacity(batch.len() * 96);
        if dropped > 0 {
            // Say so in the file. A silently short log is worse than one that
            // admits it lost lines.
            buffer.push_str(&Record {
                at_ms: now_ms(),
                severity: Severity::Warn,
                category: Category::System,
                profile_id: None,
                message: format!("audit queue overflowed; {dropped} record(s) not written"),
            }
            .to_line());
            buffer.push('\n');
        }
        for line in batch {
            buffer.push_str(&line);
            buffer.push('\n');
        }

        if !state.write(&path, buffer.as_bytes(), max_bytes, generations) {
            // Count the batch as failed and carry on. Retrying in a tight loop
            // against a read-only directory would spin a core for nothing.
            sink.failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The writer's handle on the active file, and how many bytes it holds.
///
/// Tracking the size rather than calling `metadata` per write keeps rotation to
/// arithmetic in the common case. It is seeded from the file on open, so an
/// existing log from a previous run is rotated on the same threshold rather than
/// being allowed another full generation's worth.
struct WriterState {
    file: Option<File>,
    written: u64,
}

impl WriterState {
    fn open(path: &Path) -> Self {
        match open_append(path) {
            Ok((file, size)) => Self {
                file: Some(file),
                written: size,
            },
            // Nothing to write to. `write` retries the open on each batch, so a
            // directory that becomes writable later starts working without a
            // restart.
            Err(_) => Self {
                file: None,
                written: 0,
            },
        }
    }

    /// Append `bytes`, rotating first if that would take the file past
    /// `max_bytes`. Returns false if the write did not happen.
    fn write(&mut self, path: &Path, bytes: &[u8], max_bytes: u64, generations: usize) -> bool {
        if self.file.is_none() {
            // A previous open failed. Try again: the common cause is a
            // directory that did not exist yet.
            *self = Self::open(path);
        }

        // Rotate *before* writing, and only when the file already holds
        // something. Rotating an empty file would leave a zero-byte generation
        // and, worse, a batch larger than `max_bytes` would rotate on every
        // single write and churn through every generation at once.
        if self.written > 0 && self.written + bytes.len() as u64 > max_bytes {
            // Drop the handle first: on macOS the rename would otherwise leave
            // this file object pointing at the rotated inode, and every
            // subsequent append would go to `audit.log.1`.
            self.file = None;
            rotate(path, generations);
            *self = Self::open(path);
        }

        let Some(file) = self.file.as_mut() else {
            return false;
        };

        if file.write_all(bytes).is_err() {
            // A failed write can leave the handle in an unknown state, so throw
            // it away and let the next batch reopen.
            self.file = None;
            return false;
        }
        // Flush per batch. The point of the file is to survive a crash, and an
        // unflushed buffer is exactly what a crash loses.
        if file.flush().is_err() {
            self.file = None;
            return false;
        }
        self.written += bytes.len() as u64;
        true
    }
}

/// Open `path` for appending, creating the directory if needed. Returns the
/// handle and the size already on disk.
fn open_append(path: &Path) -> std::io::Result<(File, u64)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Ok((file, size))
}

/// Shift `audit.log` to `audit.log.1`, `.1` to `.2`, and so on, dropping
/// whatever falls off the end.
///
/// Renaming from the oldest generation downwards is what makes this safe to
/// interrupt: each step either has happened or has not, and the active file is
/// the *last* thing moved, so a failure part-way leaves the current log where it
/// is rather than losing it. Every error is ignored deliberately -- a rotation
/// that cannot happen must not stop the app from logging.
fn rotate(path: &Path, generations: usize) {
    if generations == 0 {
        // No generations kept: truncate in place. Still not a delete, so an
        // open handle elsewhere (a `tail -f`) keeps working.
        let _ = OpenOptions::new().write(true).truncate(true).open(path);
        return;
    }

    let numbered = |n: usize| -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{n}"));
        path.with_file_name(name)
    };

    // The oldest generation is overwritten by the one below it, so it does not
    // need removing first -- `rename` replaces the destination.
    for n in (1..generations).rev() {
        let _ = std::fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = std::fs::rename(path, numbered(1));
}

// ---------------------------------------------------------------------------
// System info
// ---------------------------------------------------------------------------

/// One fact about the machine, gathered at startup.
pub struct SystemFact {
    pub label: &'static str,
    pub value: String,
}

/// What [`Logger::system_info`] needs to look up, injected so the gathering can
/// be tested without running the real subprocesses.
pub struct SystemInfoInputs {
    pub app_version: String,
    pub proxy_binary: PathBuf,
    pub config_path: PathBuf,
}

impl Logger {
    /// Gather and record the startup facts: app version, macOS version, the
    /// resolved `cloud-sql-proxy` path and its `--version`, the gcloud account,
    /// and the config path.
    ///
    /// **Run this off the startup path.** It shells out to `sw_vers`,
    /// `cloud-sql-proxy --version` and `gcloud config get-value account`, which
    /// together take on the order of a second -- and a menu bar app whose icon
    /// takes a second to appear looks broken. `main` spawns it.
    ///
    /// Deliberately not recorded: anything from
    /// `application_default_credentials.json`. The account *email* is what the
    /// user asked for and is not a credential; the token beside it is, and is
    /// never read.
    pub fn system_info(&self, inputs: &SystemInfoInputs) {
        for fact in gather_system_facts(inputs) {
            self.info(
                Category::System,
                None,
                format!("{}: {}", fact.label, fact.value),
            );
        }
    }
}

/// The startup facts, in the order they are logged.
///
/// Separated from the logging so it can be tested for shape without asserting
/// on whatever `gcloud` happens to be configured as on the machine running the
/// tests.
pub fn gather_system_facts(inputs: &SystemInfoInputs) -> Vec<SystemFact> {
    let proxy_display = inputs.proxy_binary.display().to_string();

    vec![
        SystemFact {
            label: "app version",
            value: inputs.app_version.clone(),
        },
        SystemFact {
            label: "macOS version",
            value: macos_version(),
        },
        SystemFact {
            label: "cloud-sql-proxy path",
            value: proxy_display.clone(),
        },
        SystemFact {
            label: "cloud-sql-proxy version",
            value: command_output(&proxy_display, &["--version"]),
        },
        SystemFact {
            label: "gcloud account",
            value: gcloud_account(),
        },
        SystemFact {
            label: "config path",
            value: inputs.config_path.display().to_string(),
        },
        SystemFact {
            label: "audit log path",
            value: match default_log_path() {
                Some(path) => path.display().to_string(),
                None => "(none; memory only)".to_string(),
            },
        },
    ]
}

/// `sw_vers` rather than a `uname` release number: the user thinks in "15.3",
/// not in "Darwin 24.3.0", and a version string nobody recognises is not worth
/// recording.
fn macos_version() -> String {
    #[cfg(target_os = "macos")]
    {
        let product = command_output("/usr/bin/sw_vers", &["-productVersion"]);
        let build = command_output("/usr/bin/sw_vers", &["-buildVersion"]);
        format!("{product} ({build})")
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::consts::OS.to_string()
    }
}

/// The account `gcloud` is configured for.
///
/// Resolved by absolute path candidates rather than by `PATH`: a `.app`
/// launched from Finder inherits a minimal environment that typically has
/// neither `/opt/homebrew/bin` nor the Cloud SDK's own bin directory, so a bare
/// `gcloud` would simply not be found in the case that matters.
fn gcloud_account() -> String {
    const CANDIDATES: [&str; 4] = [
        "/opt/homebrew/bin/gcloud",
        "/usr/local/bin/gcloud",
        "/usr/bin/gcloud",
        "gcloud",
    ];

    for candidate in CANDIDATES {
        if candidate != "gcloud" && !Path::new(candidate).exists() {
            continue;
        }
        let output = command_output(candidate, &["config", "get-value", "account"]);
        if !output.starts_with('(') {
            return output;
        }
    }
    "(not available)".to_string()
}

/// Run `binary args…` and return its trimmed first line of output.
///
/// Every failure mode -- binary missing, non-zero exit, no output -- collapses
/// to a parenthesised note rather than an error, because a missing fact is a
/// fact worth recording and is never a reason to fail startup. The parentheses
/// are also what [`gcloud_account`] uses to recognise a failed candidate.
fn command_output(binary: &str, args: &[&str]) -> String {
    let output = std::process::Command::new(binary)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let first = text.lines().next().unwrap_or("").trim().to_string();
            if !first.is_empty() {
                return first;
            }
            // Some tools (older gcloud among them) print to stderr.
            let err = String::from_utf8_lossy(&out.stderr);
            let first_err = err.lines().next().unwrap_or("").trim().to_string();
            if !first_err.is_empty() {
                return first_err;
            }
            "(no output)".to_string()
        }
        Err(error) => format!("(unavailable: {error})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        (dir, path)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    // --- record shape ------------------------------------------------------

    #[test]
    fn a_line_carries_timestamp_severity_category_profile_and_message() {
        let record = Record {
            at_ms: 0,
            severity: Severity::Warn,
            category: Category::Event,
            profile_id: Some("dev".to_string()),
            message: "preflight blocked".to_string(),
        };
        let line = record.to_line();
        assert!(line.starts_with("1970-01-01T00:00:00.000Z"), "{line}");
        assert!(line.contains("warn"), "{line}");
        assert!(line.contains("event"), "{line}");
        assert!(line.contains("dev"), "{line}");
        assert!(line.ends_with("preflight blocked"), "{line}");
    }

    #[test]
    fn a_record_with_no_profile_renders_a_placeholder_not_an_empty_column() {
        let record = Record {
            at_ms: 0,
            severity: Severity::Info,
            category: Category::System,
            profile_id: None,
            message: "app version: 0.1.0".to_string(),
        };
        // A blank column would make the message look like the profile id to
        // anything splitting on whitespace.
        assert!(record.to_line().contains(" -  "), "{}", record.to_line());
    }

    #[test]
    fn embedded_newlines_are_escaped_so_one_record_stays_one_line() {
        let record = Record {
            at_ms: 0,
            severity: Severity::Info,
            category: Category::Proxy,
            profile_id: Some("dev".to_string()),
            message: "first\nsecond\r\nthird".to_string(),
        };
        let line = record.to_line();
        assert!(!line.contains('\n'), "{line}");
        assert!(!line.contains('\r'), "{line}");
        assert!(line.contains("first\\nsecond"), "{line}");
    }

    // --- timestamps --------------------------------------------------------

    #[test]
    fn format_utc_matches_known_epochs() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(1_000), "1970-01-01T00:00:01.000Z");
        // 2001-09-09T01:46:40Z, the classic billion-second mark.
        assert_eq!(format_utc(1_000_000_000_000), "2001-09-09T01:46:40.000Z");
        // A millisecond component that is not zero, so the `.mmm` field is
        // actually exercised rather than always formatting as `.000`.
        assert_eq!(format_utc(1_787_142_896_789), "2026-08-19T12:34:56.789Z");
    }

    #[test]
    fn format_utc_handles_leap_days_and_year_boundaries() {
        // 2024-02-29T00:00:00Z -- a leap day in a leap year divisible by 4.
        assert_eq!(format_utc(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
        // 2000-02-29T00:00:00Z -- the century that *is* a leap year.
        assert_eq!(format_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        // 1900 was not a leap year; 1900-03-01 is the day after 1900-02-28.
        // Expressed forward from the epoch instead: 2100-03-01 (also not a
        // leap year) must not be 2100-02-29.
        assert_eq!(format_utc(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
        // Last millisecond of a year.
        assert_eq!(format_utc(1_767_225_599_999), "2025-12-31T23:59:59.999Z");
    }

    #[test]
    fn civil_from_days_round_trips_the_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(365), (1971, 1, 1));
    }

    // --- memory cap --------------------------------------------------------

    #[test]
    fn the_memory_cap_drops_the_oldest_records() {
        let (_dir, path) = temp_log();
        let logger = Logger::with_policy(path, 3, DEFAULT_MAX_BYTES, 1);
        for i in 0..6 {
            logger.info(Category::Proxy, Some("dev"), format!("line {i}"));
        }
        let messages: Vec<String> = logger
            .records()
            .into_iter()
            .map(|r| r.message)
            .collect();
        assert_eq!(messages, vec!["line 3", "line 4", "line 5"]);
    }

    #[test]
    fn a_zero_memory_cap_retains_nothing_and_does_not_underflow() {
        let logger = Logger::with_policy(temp_log().1, 0, DEFAULT_MAX_BYTES, 1);
        logger.info(Category::Proxy, Some("dev"), "a");
        logger.info(Category::Proxy, Some("dev"), "b");
        assert!(logger.records().is_empty());
    }

    #[test]
    fn the_memory_cap_is_independent_of_the_file() {
        // The view is capped; the file is not (until rotation). A short memory
        // cap must not truncate what lands on disk.
        let (_dir, path) = temp_log();
        let logger = Logger::with_policy(path.clone(), 2, DEFAULT_MAX_BYTES, 1);
        for i in 0..8 {
            logger.info(Category::Event, None, format!("event {i}"));
        }
        logger.flush_blocking(Duration::from_secs(5));

        assert_eq!(logger.records().len(), 2);
        let contents = read(&path);
        for i in 0..8 {
            assert!(contents.contains(&format!("event {i}")), "missing event {i}");
        }
    }

    // --- severity filtering ------------------------------------------------

    #[test]
    fn severity_filtering_is_inclusive_of_more_severe_records() {
        let logger = Logger::memory_only();
        logger.info(Category::Event, None, "an info");
        logger.warn(Category::Event, None, "a warning");
        logger.error(Category::Event, None, "an error");

        let all = logger.filtered(None, None);
        assert_eq!(all.len(), 3);

        let warn_and_up = logger.filtered(Some(Severity::Warn), None);
        assert_eq!(warn_and_up.len(), 2);
        assert!(warn_and_up.iter().all(|r| r.severity != Severity::Info));

        let errors = logger.filtered(Some(Severity::Error), None);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "an error");
    }

    #[test]
    fn profile_filtering_excludes_records_belonging_to_no_profile() {
        let logger = Logger::memory_only();
        logger.info(Category::System, None, "app version: 0.1.0");
        logger.info(Category::Proxy, Some("dev"), "dev line");
        logger.info(Category::Proxy, Some("prd"), "prd line");

        let dev = logger.filtered(None, Some("dev"));
        assert_eq!(dev.len(), 1);
        assert_eq!(dev[0].message, "dev line");
    }

    #[test]
    fn severity_and_profile_filters_compose() {
        let logger = Logger::memory_only();
        logger.info(Category::Proxy, Some("dev"), "dev info");
        logger.error(Category::Proxy, Some("dev"), "dev error");
        logger.error(Category::Proxy, Some("prd"), "prd error");

        let got = logger.filtered(Some(Severity::Error), Some("dev"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message, "dev error");
    }

    #[test]
    fn severity_orders_info_below_warn_below_error() {
        // `filtered` relies on the derived Ord, so pin it.
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Error);
    }

    // --- persistence -------------------------------------------------------

    #[test]
    fn records_reach_the_file() {
        let (_dir, path) = temp_log();
        let logger = Logger::with_policy(path.clone(), 100, DEFAULT_MAX_BYTES, 1);
        logger.info(Category::Action, Some("dev"), "started dev");
        logger.flush_blocking(Duration::from_secs(5));

        let contents = read(&path);
        assert!(contents.contains("started dev"), "{contents}");
        assert!(contents.contains("action"), "{contents}");
        assert!(contents.ends_with('\n'), "each record is one line");
        assert_eq!(logger.write_failures(), 0);
    }

    #[test]
    fn the_file_is_appended_to_across_loggers_not_truncated() {
        // A restart must not erase the previous session's trail.
        let (_dir, path) = temp_log();
        {
            let first = Logger::with_policy(path.clone(), 100, DEFAULT_MAX_BYTES, 1);
            first.info(Category::System, None, "session one");
            first.flush_blocking(Duration::from_secs(5));
        }
        {
            let second = Logger::with_policy(path.clone(), 100, DEFAULT_MAX_BYTES, 1);
            second.info(Category::System, None, "session two");
            second.flush_blocking(Duration::from_secs(5));
        }
        let contents = read(&path);
        assert!(contents.contains("session one"), "{contents}");
        assert!(contents.contains("session two"), "{contents}");
    }

    #[test]
    fn the_log_directory_is_created_if_it_does_not_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two levels that do not exist yet, as ~/Library/Logs/<bundle id> would
        // be on a fresh machine.
        let path = dir.path().join("Logs").join("bundle.id").join("audit.log");
        let logger = Logger::with_policy(path.clone(), 100, DEFAULT_MAX_BYTES, 1);
        logger.info(Category::System, None, "hello");
        logger.flush_blocking(Duration::from_secs(5));

        assert!(path.exists(), "writer should have created the directory");
        assert!(read(&path).contains("hello"));
    }

    // --- rotation ----------------------------------------------------------

    #[test]
    fn the_file_rotates_once_it_passes_the_threshold() {
        let (_dir, path) = temp_log();
        // ~120 bytes a record, so 400 bytes is a handful of records per
        // generation -- small enough to drive rotation without writing a
        // megabyte in a unit test.
        let logger = Logger::with_policy(path.clone(), 5000, 400, 3);
        for i in 0..40 {
            logger.info(Category::Proxy, Some("dev"), format!("rotation line {i:03}"));
            // Flush per record so the writer sees them as separate batches; a
            // single 40-record batch would be one oversized write rather than
            // an accumulation across the threshold.
            logger.flush_blocking(Duration::from_secs(5));
        }

        let rotated = path.with_file_name("audit.log.1");
        assert!(rotated.exists(), "expected audit.log.1 to exist after rotation");
        assert!(path.exists(), "the active file must exist after rotation");
        assert!(
            std::fs::metadata(&path).expect("active file").len() <= 400 + 200,
            "the active file should be near the threshold, not unbounded"
        );
    }

    #[test]
    fn rotation_never_loses_the_current_file() {
        let (_dir, path) = temp_log();
        let logger = Logger::with_policy(path.clone(), 5000, 300, 2);
        for i in 0..30 {
            logger.info(Category::Proxy, Some("dev"), format!("keep {i:03}"));
            logger.flush_blocking(Duration::from_secs(5));
        }
        // The most recent record must be findable in the active file: a
        // rotation that renamed the file out from under the open handle would
        // send later appends to `audit.log.1` and leave `audit.log` empty.
        let active = read(&path);
        assert!(active.contains("keep 029"), "active file: {active}");
    }

    #[test]
    fn rotation_keeps_at_most_the_configured_generations() {
        let (dir, path) = temp_log();
        let logger = Logger::with_policy(path.clone(), 5000, 200, 2);
        for i in 0..80 {
            logger.info(Category::Proxy, Some("dev"), format!("gen {i:03}"));
            logger.flush_blocking(Duration::from_secs(5));
        }

        let count = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("audit.log")
            })
            .count();
        // active + 2 generations, and never a third.
        assert!(count <= 3, "expected at most 3 files, found {count}");
        assert!(!path.with_file_name("audit.log.3").exists());
    }

    #[test]
    fn zero_generations_truncates_in_place_rather_than_growing() {
        let (_dir, path) = temp_log();
        let logger = Logger::with_policy(path.clone(), 5000, 250, 0);
        for i in 0..40 {
            logger.info(Category::Proxy, Some("dev"), format!("trunc {i:03}"));
            logger.flush_blocking(Duration::from_secs(5));
        }
        assert!(!path.with_file_name("audit.log.1").exists());
        let size = std::fs::metadata(&path).expect("active file").len();
        assert!(size <= 250 + 200, "file grew past the cap: {size} bytes");
    }

    // --- degradation -------------------------------------------------------

    #[test]
    fn an_unwritable_directory_degrades_instead_of_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("create dir");
        // Read-only and non-traversable: create_dir_all inside it fails, and so
        // does opening a file in it.
        let mut perms = std::fs::metadata(&locked).expect("metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o500);
        }
        std::fs::set_permissions(&locked, perms).expect("set_permissions");

        let path = locked.join("nested").join("audit.log");
        let logger = Logger::with_policy(path.clone(), 100, DEFAULT_MAX_BYTES, 1);

        // The whole point: logging still works, and the process is still alive.
        logger.info(Category::System, None, "still logging");
        logger.error(Category::Event, Some("dev"), "and still recording errors");
        logger.flush_blocking(Duration::from_secs(2));

        assert_eq!(logger.records().len(), 2, "memory view must be unaffected");
        assert!(!path.exists(), "nothing should have been written");
        assert!(
            logger.write_failures() > 0,
            "the failure should be counted, not swallowed"
        );

        // Restore permissions so the TempDir can clean itself up.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
        }
    }

    #[test]
    fn a_memory_only_logger_works_with_no_path_at_all() {
        let logger = Logger::memory_only();
        logger.info(Category::System, None, "no file anywhere");
        assert_eq!(logger.records().len(), 1);
        assert!(logger.path().is_none());
        assert_eq!(logger.write_failures(), 0);
        // Flushing a logger with no sink must return rather than spin.
        logger.flush_blocking(Duration::from_millis(50));
    }

    #[test]
    fn a_logger_is_cheap_to_clone_and_clones_share_one_buffer() {
        // The tray loop, the commands, and ProxyManager all hold one; they must
        // all be writing to the same trail.
        let logger = Logger::memory_only();
        let clone = logger.clone();
        logger.info(Category::Action, Some("dev"), "from the original");
        clone.info(Category::Action, Some("dev"), "from the clone");
        assert_eq!(logger.records().len(), 2);
        assert_eq!(clone.records().len(), 2);
    }

    // --- system info -------------------------------------------------------

    #[test]
    fn system_facts_cover_every_category_the_user_asked_for() {
        let facts = gather_system_facts(&SystemInfoInputs {
            app_version: "9.9.9".to_string(),
            proxy_binary: PathBuf::from("/opt/homebrew/bin/cloud-sql-proxy"),
            config_path: PathBuf::from("/tmp/profiles.json"),
        });
        let labels: Vec<&str> = facts.iter().map(|f| f.label).collect();
        for expected in [
            "app version",
            "macOS version",
            "cloud-sql-proxy path",
            "cloud-sql-proxy version",
            "gcloud account",
            "config path",
        ] {
            assert!(labels.contains(&expected), "missing {expected} in {labels:?}");
        }

        // Injected values are reported verbatim.
        let by_label = |label: &str| {
            facts
                .iter()
                .find(|f| f.label == label)
                .map(|f| f.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(by_label("app version"), "9.9.9");
        assert_eq!(by_label("config path"), "/tmp/profiles.json");
        assert_eq!(
            by_label("cloud-sql-proxy path"),
            "/opt/homebrew/bin/cloud-sql-proxy"
        );
    }

    #[test]
    fn a_missing_binary_becomes_a_note_not_a_failure() {
        // Startup must not depend on cloud-sql-proxy or gcloud being installed.
        let facts = gather_system_facts(&SystemInfoInputs {
            app_version: "0.1.0".to_string(),
            proxy_binary: PathBuf::from("/nonexistent/cloud-sql-proxy"),
            config_path: PathBuf::from("/tmp/profiles.json"),
        });
        let version = facts
            .iter()
            .find(|f| f.label == "cloud-sql-proxy version")
            .expect("version fact");
        assert!(
            version.value.starts_with('('),
            "expected a parenthesised note, got {}",
            version.value
        );
    }

    #[test]
    fn system_info_writes_one_record_per_fact() {
        let logger = Logger::memory_only();
        logger.system_info(&SystemInfoInputs {
            app_version: "0.1.0".to_string(),
            proxy_binary: PathBuf::from("/nonexistent/cloud-sql-proxy"),
            config_path: PathBuf::from("/tmp/profiles.json"),
        });
        let records = logger.records();
        assert!(records.len() >= 6, "got {} records", records.len());
        assert!(records.iter().all(|r| r.category == Category::System));
        assert!(records.iter().all(|r| r.profile_id.is_none()));
        assert!(records
            .iter()
            .any(|r| r.message == "app version: 0.1.0"));
    }

    #[test]
    fn no_credential_material_is_ever_gathered() {
        // The account email is explicitly wanted; the ADC token is not, and
        // nothing here may read the file it lives in.
        let facts = gather_system_facts(&SystemInfoInputs {
            app_version: "0.1.0".to_string(),
            proxy_binary: PathBuf::from("/nonexistent/cloud-sql-proxy"),
            config_path: PathBuf::from("/tmp/profiles.json"),
        });
        for fact in &facts {
            let lower = fact.value.to_lowercase();
            for forbidden in [
                "application_default_credentials",
                "refresh_token",
                "access_token",
                "client_secret",
                "private_key",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "{} leaked {forbidden}: {}",
                    fact.label,
                    fact.value
                );
            }
        }
    }

    #[test]
    fn the_default_path_is_under_library_logs_on_macos() {
        let path = default_log_path().expect("a home directory in the test env");
        assert!(path.ends_with("audit.log"), "{}", path.display());
        #[cfg(target_os = "macos")]
        {
            let text = path.display().to_string();
            assert!(text.contains("/Library/Logs/"), "{text}");
            assert!(text.contains(LOG_DIR_NAME), "{text}");
        }
    }
}
