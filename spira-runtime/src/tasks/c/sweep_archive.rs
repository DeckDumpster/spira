//! # sweep.archive — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-archive.md`;
//! its Acceptance section is the test file `tests/it/task_c_sweep_archive.rs`.
//!
//! ## Tick
//!
//! - Interval 20 min (`OnUnitActiveSec=20min`), first 5 min after boot (`OnBootSec=5min`);
//!   `Persistent=true` is inert on a monotonic timer.
//! - **Wakes (proposed):** none. The session-end hook (`archive.sh hook`, `:298-313`) remains a
//!   separate writer outside the scheduler (Claude Code runs it), so it is not an event source.
//! - **Single-flight:** yes; and the whole-run `flock` on `$SPIRA_ARCHIVE/.lock` (`:69-81`) stays,
//!   because the hook is a second writer the scheduler does not own.
//! - No `world guard`.
//!
//! ## Observe
//!
//! `sweep.archive.observe` (probe, host, deadline 60 s) returns:
//!
//! ```text
//! Snapshot { sources: Vec<{rel, size, mtime}>, index: BTreeMap<rel, {bytes, mtime, body_exists}>, projects_dir_exists: bool }
//! ```
//!
//! - `sources`: every `*.jsonl` under `$SPIRA_TOKEN_PROJECTS` (`:268-274`), stat only.
//! - `index`: `$SPIRA_ARCHIVE/index.jsonl` (`:227-239`) — the archive is the component that owns
//!   this; the probe reads its index, not re-derived from bodies.
//! - No lifecycle-store reads. Today the unit forks no Spira binary.
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<rel>`: the fast path at `:358-361` — a source is stored iff no index row,
//! or `bytes`/`mtime` differ, or the body is missing. Missing default projects dir → nothing
//! (`:327-329`); a configured one that is missing → refuse (`:330`). Rows whose source vanished are
//! **kept** (`:372-374`). No carry.
//! **Fixtures:** `the archive-no-sessions suite` and the archive test scripts' trees, ported.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.archive.store` (one per changed rel, one in flight at a time) | host | 300 s per transcript | `archive/<rel>/<size>/<mtime>` | a source outside `$SPIRA_TOKEN_PROJECTS`; an unreadable source (skipped); the lock held > 900 s by the hook |
//! | `sweep.archive.index` | host | 60 s | `archive/index/<sha of row set>` | — ; appends the pending rows; rewrites the index only when its lines exceed twice its distinct sources |
//!
//! `store` reads the source once, from its cursor to the size at open, in 8 MiB frames: each frame is
//! hashed, scanned and compressed (`zstd`, else `gzip`, as a child over stdin) as it is read, so no
//! frozen copy is made. A full copy builds `bodies/<rel>.zst|.gz.part` and renames it, removing the
//! other extension; growth appends frames to the body, and concatenated frames decompress as one
//! stream, so `archive.sh restore` and `verify` need no change. `cursors/<rel>.json`, written after
//! the pending row, holds the source inode and offset, a hash of the 4 KiB before that offset, the
//! SHA-256 state at the last 64-byte boundary, the line-scan state at the last newline, and the
//! body's inode, length and extension. A cursor that is missing, unparseable or mismatching, a
//! shorter source, or the other compressor makes the pass a full copy; so does a body the hook
//! rewrote or a torn append. The row's `sha256` covers the whole stored transcript.
//! The index is append-only between compactions: the last row per `source` wins on read, the
//! bridge id is inherited by session id, and `archive.sh` reads it the same way.
//! Nothing ever deletes a body (`:35-37`).
//!
//! ## Silence
//!
//! An unchanged sweep stores nothing and leaves the index untouched (`:244-246`), so it journals
//! nothing beyond the probe. Today it prints a summary line every 20 min to `run/archive.log`
//! (`:378-380`); that goes.
//!
//! ## Replaces
//!
//! - Unit `spira-archive-prod.service` / `.timer`; output `run/archive.log`.
//! - Children: `bash conf.sh`, `python3`, `flock`, `zstd`|`gzip`, `date`.
//! - Config keys pinned: `SPIRA_ARCHIVE` (default `$SPIRA_RUN/archive`,
//!   `spira/conf.d.defaults.generated.sh:58`), `SPIRA_TOKEN_PROJECTS` (default
//!   `$HOME/.claude/projects`, `:1158`).
//! - `archive.sh query|lineage|restore|verify|where|hook` stay as the operator CLI and hook.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` shadows `store`/`index`; the hook still archives
//! each session at its end, so a shadowed window loses only mid-session growth.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.archive";
pub const STREAM: &str = "task.sweep.archive";
pub const TICK: EventKindTag = "sweep.archive.tick";
pub const STATE: EventKindTag = "sweep.archive.state";

pub const OBSERVE: &str = "sweep.archive.observe";
pub const STORE: &str = "sweep.archive.store";
pub const INDEX: &str = "sweep.archive.index";

/// The most transcripts one pass stores. The carry names each in flight and is journaled as one
/// record, so an unbounded pass overflowed the store's string limit and crashed the scheduler
/// on every boot; the rest are stored by the next pass.
pub const PASS_MAX: usize = 200;
/// The most carry bytes one pass names, half the store's string limit: paths vary in length, so a
/// count alone does not keep the record under it.
pub const PASS_CARRY_BYTES: usize = 32 * 1024;
pub const EFFECTS: &[&str] = &[OBSERVE, STORE, INDEX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_ARCHIVE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_ARCHIVE", "SPIRA_TOKEN_PROJECTS"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 60_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub bytes: u64,
    pub mtime: u64,
    pub body_exists: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub sources: Vec<Source>,
    pub index: BTreeMap<String, Row>,
    pub projects_dir_exists: bool,
    /// A configured (not default) projects dir that is missing refuses.
    pub projects_dir_configured: bool,
}

/// Per rel and for the index (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unseen,
    Changed,
    Storing,
    Stored,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Index {
    #[default]
    Current,
    Stale,
}

/// `state × event → state` per rel. An `index` while a `store` of the same pass is in flight
/// is refused naming `storing`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Unseen, "store", Phase::Storing), (Phase::Changed, "store", Phase::Storing), (Phase::Storing, "store:ok", Phase::Stored), (Phase::Storing, "store:failed", Phase::Changed), (Phase::Stored, "changed", Phase::Changed)];
pub const INDEX_TRANSITIONS: &[(Index, &str, Index)] = &[(Index::Current, "stored", Index::Stale), (Index::Stale, "index:ok", Index::Current)];

/// No carry beyond the pass in flight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub rels: BTreeMap<String, Phase>,
    /// Stores of the pass not yet requested: one is issued per completed store, so the host pool never holds the pass.
    pub queue: Vec<Store>,
    pub index: Index,
}

pub struct SweepArchive {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// What [`STORE`] carries: one transcript, at the size and mtime the probe saw.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
}

fn phase_name(p: Phase) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The next queued store, marked storing; the queue is kept reversed so this pops the pass in order.
fn next_store(c: &mut Carry) -> Result<Option<Act>, String> {
    let Some(body) = c.queue.pop() else { return Ok(None) };
    c.rels.insert(body.rel.clone(), Phase::Storing);
    Ok(Some(Act { name: STORE.into(), key: format!("archive/{}/{}/{}", body.rel, body.size, body.mtime), input: serde_json::to_string(&body).map_err(|e| e.to_string())?, deadline_ms: 300_000, probe: false }))
}

/// The fast path: store a source with no index row, a size or mtime change, or no body.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let mut next = carry.clone();
    match input {
        Input::Fire => Ok((next, vec![])),
        Input::Observed(s) => {
            if !s.projects_dir_exists {
                if s.projects_dir_configured {
                    return Err(format!("{NAME}: the configured transcript directory (SPIRA_TOKEN_PROJECTS) does not exist"));
                }
                return Ok((Carry { index: carry.index, ..Carry::default() }, vec![]));
            }
            next.rels.clear();
            next.queue.clear();
            let mut named = 0;
            for src in s.sources.iter().filter(|src| !s.index.get(&src.rel).is_some_and(|r| r.bytes == src.size && r.mtime == src.mtime && r.body_exists)).take(PASS_MAX) {
                named += src.rel.len() + 80;
                if named > PASS_CARRY_BYTES && !next.queue.is_empty() {
                    break;
                }
                next.queue.push(Store { rel: src.rel.clone(), size: src.size, mtime: src.mtime });
            }
            next.queue.reverse();
            let acts = next_store(&mut next)?.into_iter().collect();
            Ok((next, acts))
        }
        Input::Outcome { effect, ok, body } if effect == STORE => {
            // A failed store names its rel first (`<rel>: why`), so it goes back to changed.
            let rel = if *ok { body.trim() } else { body.split(": ").next().unwrap_or_default() };
            match carry.rels.get(rel) {
                Some(Phase::Storing) => {}
                Some(p) => return Err(format!("{NAME}: {rel} is in state {}, not storing", phase_name(*p))),
                None if !*ok => {
                    let acts = next_store(&mut next)?.into_iter().collect();
                    return Ok((next, acts));
                }
                None => return Err(format!("{NAME}: {rel} has no store in flight")),
            }
            next.rels.insert(rel.to_string(), if *ok { Phase::Stored } else { Phase::Changed });
            if *ok {
                next.index = Index::Stale;
            }
            if let Some(act) = next_store(&mut next)? {
                return Ok((next, vec![act]));
            }
            let in_flight = next.rels.values().any(|p| *p == Phase::Storing);
            if !in_flight && next.index == Index::Stale {
                return Ok((next, vec![Act { name: INDEX.into(), key: "archive/index".into(), input: String::new(), deadline_ms: 60_000, probe: false }]));
            }
            Ok((next, vec![]))
        }
        Input::Outcome { effect, ok, .. } if effect == INDEX => {
            if let Some((rel, _)) = carry.rels.iter().find(|(_, p)| **p == Phase::Storing) {
                return Err(format!("{NAME}: an index outcome while {rel} is storing is refused"));
            }
            if *ok {
                next.index = Index::Current;
            }
            Ok((next, vec![]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Lost(lost) => lost.as_failure(decide, cfg, carry, now_ms),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// `archive.sh sweep`'s worker, in-process: the probe's stat walk, a body's incremental hash,
/// scan and compression, and the index append. The compressor (`zstd`, else `gzip`) stays a
/// child, run through `runtime::ports::Proc`.
pub mod archive {
    use std::collections::BTreeMap;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use runtime::ports::{Proc, ProcSpec};
    use serde::{Deserialize, Serialize};
    use serde_json::{Map, Value};
    use sha2::digest::generic_array::typenum::U64;
    use sha2::digest::generic_array::GenericArray;

    use super::{Row, Snapshot, Source, Store};

    /// The paths the sweep reads and writes, from `SPIRA_ARCHIVE` and `SPIRA_TOKEN_PROJECTS`.
    pub struct Paths {
        pub archive: PathBuf,
        pub projects: PathBuf,
        /// `~/.claude/projects`: missing before the box's first session, which is not a refusal.
        pub default_projects: PathBuf,
    }

    impl Paths {
        pub fn index(&self) -> PathBuf {
            self.archive.join("index.jsonl")
        }
        /// Rows a store wrote and the index has not merged yet.
        pub fn pending(&self) -> PathBuf {
            self.archive.join("pending")
        }
    }

    /// The flock on `<archive>/.lock` the session-end hook also takes, waited for up to `wait`.
    pub struct Lock(std::fs::File);

    impl Lock {
        pub fn take(archive: &Path, wait: Duration) -> Result<Lock, String> {
            use std::os::fd::AsRawFd;
            std::fs::create_dir_all(archive).map_err(|e| format!("cannot create {}: {e}", archive.display()))?;
            let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(archive.join(".lock")).map_err(|e| format!("archive lock: {e}"))?;
            let start = std::time::Instant::now();
            loop {
                // SAFETY: flock on a descriptor this process owns; released when `f` drops.
                if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    return Ok(Lock(f));
                }
                if start.elapsed() >= wait {
                    return Err(format!("another sweep has held the archive lock for {}s — not starting a second", wait.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            use std::os::fd::AsRawFd;
            // SAFETY: as in `take`.
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    fn mtime_of(m: &std::fs::Metadata) -> u64 {
        m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
    }

    /// Every `*.jsonl` under `dir`, deepest included, sorted (`os.walk`, which does not descend
    /// a symlinked directory).
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&p, out),
                Ok(_) if p.extension().is_some_and(|x| x == "jsonl") => out.push(p),
                _ => {}
            }
        }
    }

    /// The index as a reader sees it: the last row per `source` wins, the bridge id is inherited
    /// by session id, sorted by `(first_ts, source)`. Unparseable lines are skipped.
    pub fn load_index(path: &Path) -> Vec<Map<String, Value>> {
        settle(read_index(path).by_source.into_values().collect())
    }

    struct IndexFile {
        /// Non-blank lines, including ones that do not parse.
        lines: usize,
        torn: bool,
        by_source: BTreeMap<String, Map<String, Value>>,
    }

    fn read_index(path: &Path) -> IndexFile {
        let raw = std::fs::read(path).unwrap_or_default();
        let text = String::from_utf8_lossy(&raw);
        let mut file = IndexFile { lines: 0, torn: !raw.is_empty() && !raw.ends_with(b"\n"), by_source: BTreeMap::new() };
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            file.lines += 1;
            if let Some(row) = serde_json::from_str::<Value>(l).ok().and_then(|v| v.as_object().cloned()) {
                if let Some(src) = row.get("source").and_then(Value::as_str).map(str::to_string) {
                    file.by_source.insert(src, row);
                }
            }
        }
        file
    }

    /// The probe: the transcripts stat'd, the index as the fast path reads it.
    pub fn observe(p: &Paths) -> Snapshot {
        let exists = p.projects.is_dir();
        let configured = p.projects != p.default_projects;
        let mut files = vec![];
        if exists {
            walk(&p.projects, &mut files);
        }
        files.sort();
        let sources = files
            .iter()
            .filter_map(|f| {
                let m = std::fs::metadata(f).ok().filter(|m| m.is_file())?;
                let rel = f.strip_prefix(&p.projects).ok()?.to_string_lossy().into_owned();
                Some(Source { rel, size: m.len(), mtime: mtime_of(&m) })
            })
            .collect();
        let index = load_index(&p.index())
            .into_iter()
            .filter_map(|r| {
                let rel = r.get("source")?.as_str()?.to_string();
                let body = r.get("path").and_then(Value::as_str).map(|b| p.archive.join(b));
                Some((rel, Row { bytes: r.get("bytes").and_then(Value::as_u64).unwrap_or(u64::MAX), mtime: r.get("mtime").and_then(Value::as_u64).unwrap_or(u64::MAX), body_exists: body.is_some_and(|b| b.exists()) }))
            })
            .collect::<BTreeMap<_, _>>();
        Snapshot { sources, index, projects_dir_exists: exists, projects_dir_configured: configured }
    }

    fn is_space(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
    }

    /// Each value of `"key"\s*:\s*"<v>"` in `line` whose `v` holds no quote and is `min..=max`
    /// bytes long, in order (the worker's byte regexes).
    fn values<'a>(line: &'a [u8], key: &str, min: usize, max: usize) -> impl Iterator<Item = &'a [u8]> + 'a {
        let pat = format!("\"{key}\"").into_bytes();
        (0..line.len()).filter_map(move |i| {
            if !line[i..].starts_with(&pat) {
                return None;
            }
            let mut j = i + pat.len();
            while j < line.len() && is_space(line[j]) {
                j += 1;
            }
            if line.get(j) != Some(&b':') {
                return None;
            }
            j += 1;
            while j < line.len() && is_space(line[j]) {
                j += 1;
            }
            if line.get(j) != Some(&b'"') {
                return None;
            }
            let rest = &line[j + 1..];
            let end = rest.iter().position(|b| *b == b'"')?;
            (min..=max).contains(&end).then(|| &rest[..end])
        })
    }

    const SHA256_IV: [u32; 8] = [0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];

    fn compress_blocks(state: &mut [u32; 8], bytes: &[u8]) {
        let blocks: Vec<GenericArray<u8, U64>> = bytes.as_chunks::<64>().0.iter().map(|b| GenericArray::from(*b)).collect();
        sha2::compress256(state, &blocks);
    }

    /// SHA-256 whose state can be stored between passes: the state at the last 64-byte boundary
    /// and the bytes after it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Sha {
        state: [u32; 8],
        tail: Vec<u8>,
        len: u64,
    }

    impl Default for Sha {
        fn default() -> Sha {
            Sha::new()
        }
    }

    impl Sha {
        pub fn new() -> Sha {
            Sha { state: SHA256_IV, tail: vec![], len: 0 }
        }

        pub fn update(&mut self, data: &[u8]) {
            self.len += data.len() as u64;
            let mut data = data;
            if !self.tail.is_empty() {
                let take = (64 - self.tail.len()).min(data.len());
                self.tail.extend_from_slice(&data[..take]);
                data = &data[take..];
                if self.tail.len() < 64 {
                    return;
                }
                compress_blocks(&mut self.state, &self.tail);
                self.tail.clear();
            }
            let whole = data.len() / 64 * 64;
            compress_blocks(&mut self.state, &data[..whole]);
            self.tail.extend_from_slice(&data[whole..]);
        }

        pub fn hex(&self) -> String {
            let mut state = self.state;
            let mut last = self.tail.clone();
            last.push(0x80);
            while last.len() % 64 != 56 {
                last.push(0);
            }
            last.extend_from_slice(&(self.len * 8).to_be_bytes());
            compress_blocks(&mut state, &last);
            state.iter().map(|w| format!("{w:08x}")).collect()
        }
    }

    /// What the lines of a transcript tell: stored with the cursor so a pass reads only new lines.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Lines {
        first: Option<Vec<u8>>,
        last: Option<Vec<u8>>,
        /// Lines carrying `"type": "assistant"`.
        turns: u64,
        bridge: String,
        sess: String,
        parent: String,
    }

    impl Lines {
        fn feed(&mut self, line: &[u8]) {
            if values(line, "type", 0, usize::MAX).any(|v| v == b"assistant") {
                self.turns += 1;
            }
            if let Some(t) = values(line, "timestamp", 10, 40).next() {
                if self.first.as_deref().is_none_or(|f| t < f) {
                    self.first = Some(t.to_vec());
                }
                if self.last.as_deref().is_none_or(|l| t > l) {
                    self.last = Some(t.to_vec());
                }
            }
            for (key, slot) in [("bridgeSessionId", &mut self.bridge), ("sessionId", &mut self.sess), ("parentSessionId", &mut self.parent)] {
                if slot.is_empty() {
                    if let Some(v) = values(line, key, 1, 120).next() {
                        *slot = String::from_utf8_lossy(v).into_owned();
                    }
                }
            }
        }
    }

    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// An ISO-8601 instant as epoch seconds (fractions dropped; no zone is UTC), or `None`.
    pub fn parse_ts(s: &str) -> Option<i64> {
        let s = s.trim();
        let num = |t: &str| -> Option<i64> { t.bytes().all(|b| b.is_ascii_digit()).then(|| t.parse().ok()).flatten() };
        let (date, time) = match s.get(10..11) {
            Some("T" | "t" | " ") => (&s[..10], &s[11..]),
            None if s.len() == 10 => (s, ""),
            _ => return None,
        };
        let (y, mo, d) = (num(date.get(0..4)?)?, num(date.get(5..7)?)?, num(date.get(8..10)?)?);
        if date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
            return None;
        }
        let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
            Some(i) => (&time[..i], &time[i..]),
            None => (time, ""),
        };
        let mut secs = 0i64;
        if !clock.is_empty() {
            let clock = clock.split('.').next()?;
            let parts: Vec<&str> = clock.split(':').collect();
            if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.len() != 2) {
                return None;
            }
            let (hh, mm, ss) = (num(parts[0])?, num(parts[1])?, if parts.len() == 3 { num(parts[2])? } else { 0 });
            if hh > 23 || mm > 59 || ss > 59 {
                return None;
            }
            secs = hh * 3600 + mm * 60 + ss;
        }
        let off = match offset {
            "" | "Z" | "z" => 0,
            o => {
                let sign = if o.starts_with('-') { -1 } else { 1 };
                let digits: String = o[1..].chars().filter(|c| *c != ':').collect();
                if digits.len() != 4 && digits.len() != 6 {
                    return None;
                }
                sign * (num(&digits[0..2])? * 3600 + num(&digits[2..4])? * 60)
            }
        };
        Some(days_from_civil(y, mo, d) * 86_400 + secs - off)
    }

    /// Epoch seconds as `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn fmt_ts(t: i64) -> String {
        let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
    }

    /// `json.dumps(row, sort_keys=True)`: Python's separators and `ensure_ascii`, so an index
    /// this writes is byte-equal to the one the script wrote for the same rows.
    pub fn py_dumps(v: &Value) -> String {
        py_json(v, ", ", ": ")
    }

    /// `json.dumps(v, sort_keys=True, separators=(item, key))` with `ensure_ascii`.
    pub fn py_json(v: &Value, item: &str, key: &str) -> String {
        match v {
            Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}{key}{}", py_json(&Value::String(k.clone()), item, key), py_json(v, item, key))).collect::<Vec<_>>().join(item)),
            Value::Array(a) => format!("[{}]", a.iter().map(|v| py_json(v, item, key)).collect::<Vec<_>>().join(item)),
            Value::String(s) => {
                let raw = serde_json::to_string(s).unwrap_or_default();
                let mut out = String::with_capacity(raw.len());
                for c in raw.chars() {
                    if c.is_ascii() {
                        out.push(c);
                    } else {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                }
                out
            }
            other => other.to_string(),
        }
    }

    /// The bridge id inherited by session id, then sorted by `(first_ts, source)`.
    fn settle(mut rows: Vec<Map<String, Value>>) -> Vec<Map<String, Value>> {
        let s = |r: &Map<String, Value>, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let bridge: BTreeMap<String, String> = rows.iter().filter(|r| !s(r, "bridge_session_id").is_empty()).map(|r| (s(r, "session_id"), s(r, "bridge_session_id"))).collect();
        for r in rows.iter_mut() {
            if s(r, "bridge_session_id").is_empty() {
                let b = bridge.get(&s(r, "session_id")).cloned().unwrap_or_default();
                r.insert("bridge_session_id".into(), Value::String(b));
            }
        }
        rows.sort_by_key(|r| (s(r, "first_ts"), s(r, "source")));
        rows
    }

    /// One line per source, settled, rewritten whole and replaced only when the bytes differ.
    /// Returns whether it was replaced.
    pub fn write_index(path: &Path, rows: Vec<Map<String, Value>>) -> Result<bool, String> {
        let body: String = settle(rows).into_iter().map(|r| py_dumps(&Value::Object(r)) + "\n").collect();
        if std::fs::read(path).ok().as_deref() == Some(body.as_bytes()) {
            return Ok(false);
        }
        let tmp = path.with_extension("jsonl.part");
        std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(true)
    }

    /// One frame: `data` compressed with `zstd`, else `gzip`, and the extension of the one that ran.
    /// Concatenated frames decompress as one stream.
    fn compress(proc: &dyn Proc, data: &[u8]) -> Result<(&'static str, Vec<u8>), String> {
        let tries: [(&str, &'static str, &[&str]); 2] = [("zstd", ".zst", &["-q", "-c", "-"]), ("gzip", ".gz", &["-n", "-q", "-c", "-"])];
        let mut why = vec![];
        for (prog, ext, args) in tries {
            // batch-job: compresses one frame; it runs as long as the frame is large
            match proc.run(&ProcSpec { prog: prog.into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: data.to_vec(), deadline: Duration::from_secs(240) }) {
                Ok(o) if o.code == Some(0) => return Ok((ext, o.stdout)),
                Ok(o) => return Err(format!("{prog} failed (exit {:?})", o.code)),
                Err(e) => why.push(e),
            }
        }
        Err(format!("neither zstd nor gzip ran — one of them stores the bodies: {}", why.join("; ")))
    }

    /// The source is read in frames of this many bytes, each compressed on its own.
    const FRAME: usize = 8 << 20;
    /// The bytes before the cursor offset whose hash says the source still begins as it did.
    const PREFIX: u64 = 4096;

    /// Where the last pass over a transcript stopped. Anything that does not match, or that
    /// cannot be read, makes the next pass a full copy.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Cursor {
        src_ino: u64,
        offset: u64,
        prefix: String,
        sha: Sha,
        lines: Lines,
        /// The offset just past the last newline: the next pass re-reads the partial line after it.
        lines_offset: u64,
        body_ino: u64,
        body_len: u64,
        ext: String,
    }

    fn cursor_path(p: &Paths, rel: &str) -> PathBuf {
        p.archive.join("cursors").join(format!("{rel}.json"))
    }

    fn prefix_hash(f: &mut std::fs::File, offset: u64) -> std::io::Result<String> {
        let from = offset.saturating_sub(PREFIX);
        let mut buf = vec![0u8; (offset - from) as usize];
        f.seek(SeekFrom::Start(from))?;
        f.read_exact(&mut buf)?;
        Ok(ring::digest::digest(&ring::digest::SHA256, &buf).as_ref().iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The cursor if it still describes this source and this body.
    fn resumable(p: &Paths, rel: &str, f: &mut std::fs::File, st: &std::fs::Metadata, stem: &Path) -> Option<Cursor> {
        let c: Cursor = serde_json::from_slice(&std::fs::read(cursor_path(p, rel)).ok()?).ok()?;
        let body = std::fs::metadata(format!("{}{}", stem.display(), c.ext)).ok()?;
        let sound = c.src_ino == st.ino() && c.offset <= st.len() && c.lines_offset <= c.offset && c.sha.len == c.offset && body.ino() == c.body_ino && body.len() == c.body_len && matches!(c.ext.as_str(), ".zst" | ".gz");
        (sound && prefix_hash(f, c.offset).ok()? == c.prefix).then_some(c)
    }

    struct Grown {
        sha: String,
        lines: Lines,
        bytes: u64,
        ext: String,
        cursor: Cursor,
    }

    /// Reads the source from the cursor (else from 0) to `st.len()` once: hashes, scans and
    /// compresses those bytes. A full copy builds `<body>.part` and renames it; growth appends
    /// frames to the body. `None`: the compressor that ran is not the one that wrote the body.
    fn grow(proc: &dyn Proc, f: &mut std::fs::File, st: &std::fs::Metadata, stem: &Path, from: Option<&Cursor>) -> Result<Option<Grown>, String> {
        let io = |e: std::io::Error| e.to_string();
        let len = st.len();
        let start = from.map_or(0, |c| c.offset);
        let (mut sha, mut lines, mut lines_offset) = from.map_or_else(|| (Sha::new(), Lines::default(), 0), |c| (c.sha.clone(), c.lines.clone(), c.lines_offset));
        let mut line = vec![0u8; (start - lines_offset) as usize];
        f.seek(SeekFrom::Start(lines_offset)).map_err(io)?;
        f.read_exact(&mut line).map_err(io)?;
        let mut sink: Option<(std::fs::File, PathBuf, String)> = None;
        let mut pos = start;
        let mut buf = vec![];
        while pos < len || (pos == len && start == 0 && sink.is_none()) {
            buf.resize((len - pos).min(FRAME as u64) as usize, 0);
            f.read_exact(&mut buf).map_err(|e| format!("transcript shrank while read: {e}"))?;
            let (ext, frame) = compress(proc, &buf)?;
            if sink.is_none() {
                let ext = ext.to_string();
                sink = Some(match from {
                    Some(c) if c.ext != ext => return Ok(None),
                    Some(_) => {
                        let body = PathBuf::from(format!("{}{ext}", stem.display()));
                        (std::fs::OpenOptions::new().append(true).open(&body).map_err(io)?, body, ext)
                    }
                    None => {
                        let part = PathBuf::from(format!("{}{ext}.part", stem.display()));
                        (std::fs::File::create(&part).map_err(io)?, part, ext)
                    }
                });
            }
            sink.as_mut().map(|(out, ..)| out.write_all(&frame)).transpose().map_err(io)?;
            sha.update(&buf);
            let mut s = 0;
            while let Some(i) = buf[s..].iter().position(|b| *b == b'\n') {
                line.extend_from_slice(&buf[s..=s + i]);
                lines.feed(&line);
                line.clear();
                s += i + 1;
                lines_offset = pos + s as u64;
            }
            line.extend_from_slice(&buf[s..]);
            pos += buf.len() as u64;
        }
        let mut whole = lines.clone();
        if !line.is_empty() {
            whole.feed(&line);
        }
        let ext = match (&sink, from) {
            (Some((.., ext)), _) => ext.clone(),
            (None, Some(c)) => c.ext.clone(),
            (None, None) => return Err("nothing compressed".into()),
        };
        let body = PathBuf::from(format!("{}{ext}", stem.display()));
        if let Some((out, part, _)) = sink.take().filter(|_| from.is_none()) {
            out.sync_all().map_err(io)?;
            std::fs::rename(&part, &body).map_err(io)?;
            for other in [".zst", ".gz"].into_iter().filter(|o| *o != ext) {
                let _ = std::fs::remove_file(format!("{}{other}", stem.display()));
            }
        }
        let bm = std::fs::metadata(&body).map_err(io)?;
        let cursor = Cursor { src_ino: st.ino(), offset: pos, prefix: prefix_hash(f, pos).map_err(io)?, sha: sha.clone(), lines, lines_offset, body_ino: bm.ino(), body_len: bm.len(), ext: ext.clone() };
        Ok(Some(Grown { sha: sha.hex(), lines: whole, bytes: pos, ext, cursor }))
    }

    /// One transcript archived: its bytes since the last pass hashed, scanned and compressed onto
    /// `bodies/<rel>.zst|.gz`, its index row left in `pending/` for the index pass, then its
    /// cursor written. Returns the rel; a failure is `<rel>: why`.
    pub fn store(p: &Paths, proc: &dyn Proc, job: &Store, now: i64) -> Result<String, String> {
        let fail = |why: String| format!("{}: {why}", job.rel);
        if job.rel.split('/').any(|c| c == "..") || Path::new(&job.rel).is_absolute() {
            return Err(fail(format!("outside {}", p.projects.display())));
        }
        let src = p.projects.join(&job.rel);
        std::fs::metadata(&src).map_err(|e| fail(format!("cannot read: {e}")))?;
        let _lock = Lock::take(&p.archive, Duration::from_secs(240)).map_err(fail)?;
        let stem = p.archive.join("bodies").join(&job.rel);
        let made = (|| -> Result<(Grown, i64), String> {
            std::fs::create_dir_all(stem.parent().unwrap_or(&p.archive)).map_err(|e| e.to_string())?;
            let mut f = std::fs::File::open(&src).map_err(|e| e.to_string())?;
            let st = f.metadata().map_err(|e| e.to_string())?;
            let cursor = resumable(p, &job.rel, &mut f, &st, &stem);
            let grown = grow(proc, &mut f, &st, &stem, cursor.as_ref()).inspect_err(|_| undo_growth(&stem, cursor.as_ref()))?;
            let grown = match grown {
                Some(g) => g,
                None => grow(proc, &mut f, &st, &stem, None).inspect_err(|_| undo_growth(&stem, None))?.ok_or_else(|| "a full copy resumed".to_string())?,
            };
            Ok((grown, mtime_of(&st) as i64))
        })();
        let (g, mtime) = made.map_err(fail)?;
        let Lines { first, last, turns, bridge, sess, parent } = g.lines;
        let sess = if sess.is_empty() { parent } else { sess };
        let text = |v: Option<Vec<u8>>| v.map(|v| String::from_utf8_lossy(&v).into_owned());
        let (f, l) = (text(first).as_deref().and_then(parse_ts), text(last).as_deref().and_then(parse_ts));
        let kind = if f.is_some() && l.is_some() { "transcript" } else { "mtime" };
        let stem_name = Path::new(&job.rel).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut row = Map::new();
        row.insert("session_id".into(), Value::String(if sess.is_empty() { stem_name } else { sess }));
        row.insert("bridge_session_id".into(), Value::String(bridge));
        row.insert("cwd_slug".into(), Value::String(job.rel.split('/').next().unwrap_or_default().to_string()));
        row.insert("first_ts".into(), Value::String(fmt_ts(f.unwrap_or(mtime))));
        row.insert("last_ts".into(), Value::String(fmt_ts(l.unwrap_or(mtime))));
        row.insert("ts_source".into(), Value::String(kind.into()));
        row.insert("turns".into(), Value::from(turns));
        row.insert("bytes".into(), Value::from(g.bytes));
        row.insert("mtime".into(), Value::from(mtime));
        row.insert("sha256".into(), Value::String(g.sha));
        row.insert("source".into(), Value::String(job.rel.clone()));
        row.insert("path".into(), Value::String(format!("bodies/{}{}", job.rel, g.ext)));
        row.insert("archived_at".into(), Value::String(fmt_ts(now)));
        let pending = p.pending().join(format!("{}.json", job.rel));
        write_atomic(&pending, py_dumps(&Value::Object(row)).as_bytes()).map_err(|e| fail(e.to_string()))?;
        let cursor = serde_json::to_vec(&g.cursor).map_err(|e| fail(e.to_string()))?;
        write_atomic(&cursor_path(p, &job.rel), &cursor).map_err(|e| fail(e.to_string()))?;
        Ok(job.rel.clone())
    }

    /// A pass that failed after appending frames cuts the body back to what the cursor describes.
    fn undo_growth(stem: &Path, from: Option<&Cursor>) {
        match from {
            Some(c) => {
                let _ = std::fs::OpenOptions::new().write(true).open(format!("{}{}", stem.display(), c.ext)).and_then(|f| f.set_len(c.body_len));
            }
            None => {
                for ext in [".zst", ".gz"] {
                    let _ = std::fs::remove_file(format!("{}{ext}.part", stem.display()));
                }
            }
        }
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    /// The index pass: every pending row appended to the index (a torn last line is ended first),
    /// the pending rows cleared. The index is rewritten, one line per source, only when its lines
    /// exceed twice its distinct sources. Rows whose source vanished are kept.
    pub fn index(p: &Paths) -> Result<String, String> {
        let _lock = Lock::take(&p.archive, Duration::from_secs(50))?;
        let mut file = read_index(&p.index());
        let mut merged = vec![];
        let mut added = String::new();
        let mut files = vec![];
        walk_json(&p.pending(), &mut files);
        files.sort();
        for f in files {
            let Some(row) = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()) else { continue };
            let Some(rel) = row.get("source").and_then(Value::as_str).map(str::to_string) else { continue };
            added.push_str(&(py_dumps(&Value::Object(row.clone())) + "\n"));
            file.by_source.insert(rel, row);
            file.lines += 1;
            merged.push(f);
        }
        if !merged.is_empty() {
            let mut out = std::fs::OpenOptions::new().create(true).append(true).open(p.index()).map_err(|e| format!("{}: {e}", p.index().display()))?;
            let lead = if file.torn { "\n" } else { "" };
            out.write_all(format!("{lead}{added}").as_bytes()).map_err(|e| format!("{}: {e}", p.index().display()))?;
        }
        let n = file.by_source.len();
        let compact = file.lines > 2 * n;
        let verdict = if compact && write_index(&p.index(), file.by_source.into_values().collect())? {
            "index rewritten"
        } else if merged.is_empty() {
            "index untouched"
        } else {
            "index appended"
        };
        for f in &merged {
            let _ = std::fs::remove_file(f);
        }
        Ok(format!("{} row(s) merged, {n} in the index, {verdict}", merged.len()))
    }

    fn walk_json(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_json(&p, out);
            } else if p.extension().is_some_and(|x| x == "json") {
                out.push(p);
            }
        }
    }
}

/// The paths from the pinned config; `HOME` names the default projects dir the script exempts.
pub fn paths(get: &dyn Fn(&str) -> Result<String, String>, home: &std::path::Path) -> Result<archive::Paths, String> {
    Ok(archive::Paths { archive: get("SPIRA_ARCHIVE")?.into(), projects: get("SPIRA_TOKEN_PROJECTS")?.into(), default_projects: home.join(".claude").join("projects") })
}

/// The runner of [`OBSERVE`].
pub fn observe(p: &archive::Paths) -> Result<String, String> {
    serde_json::to_string(&archive::observe(p)).map_err(|e| e.to_string())
}

/// The runner of [`STORE`].
pub fn store(p: &archive::Paths, proc: &dyn runtime::ports::Proc, input: &str) -> Result<String, String> {
    let job: Store = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    archive::store(p, proc, &job, now)
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "rels", drop: &[] }, runtime::rows::Part::Field("queue"), runtime::rows::Part::Field("index")];

crate::tasks::b::hosted!(SweepArchive);

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use runtime::ports::{Proc, ProcOutput, ProcSpec};
    use serde_json::{Map, Value};

    use super::archive::{index, load_index, store, write_index, Paths, Sha};
    use super::Store;

    /// A compressor that returns its input, from stdin or the path argument, and counts it; `zstd` can be made absent.
    struct Fake {
        input: AtomicU64,
        no_zstd: bool,
    }

    impl Fake {
        fn new(no_zstd: bool) -> Fake {
            Fake { input: AtomicU64::new(0), no_zstd }
        }
        fn input(&self) -> u64 {
            self.input.load(Ordering::SeqCst)
        }
    }

    impl Proc for Fake {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            if self.no_zstd && spec.prog == "zstd" {
                return Err("spawn zstd: not found".into());
            }
            let data = match spec.args.last().map(std::path::Path::new).filter(|p| p.is_file()) {
                Some(path) => std::fs::read(path).unwrap(),
                None => spec.stdin.clone(),
            };
            self.input.fetch_add(data.len() as u64, Ordering::SeqCst);
            Ok(ProcOutput { code: Some(0), stdout: data, stderr: vec![] })
        }
    }

    fn world() -> (testkit::TempDir, Paths) {
        let dir = testkit::TempDir::new("sweep-archive");
        std::fs::create_dir_all(dir.path().join("projects/p")).unwrap();
        let paths = Paths { archive: dir.path().join("archive"), projects: dir.path().join("projects"), default_projects: dir.path().join("home") };
        (dir, paths)
    }

    fn lines(from: usize, n: usize, width: usize) -> Vec<u8> {
        (from..from + n).flat_map(|i| format!("{{\"type\":\"assistant\",\"timestamp\":\"2026-11-24T10:{:02}:{:02}Z\",\"sessionId\":\"s1\",\"bridgeSessionId\":\"b1\",\"pad\":\"{}\"}}\n", i / 60 % 60, i % 60, "x".repeat(width)).into_bytes()).collect()
    }

    const REL: &str = "p/a.jsonl";

    fn put(p: &Paths, rel: &str, bytes: &[u8]) {
        std::fs::write(p.projects.join(rel), bytes).unwrap();
    }

    fn put_at(p: &Paths, rel: &str, bytes: &[u8], mtime: u64) {
        put(p, rel, bytes);
        let t = std::time::UNIX_EPOCH + Duration::from_secs(mtime);
        std::fs::File::options().write(true).open(p.projects.join(rel)).unwrap().set_modified(t).unwrap();
    }

    fn run_store(p: &Paths, proc: &Fake, rel: &str) {
        store(p, proc, &Store { rel: rel.into(), size: 0, mtime: 0 }, 1_800_000_000).unwrap();
    }

    fn pending(p: &Paths, rel: &str) -> Map<String, Value> {
        let text = std::fs::read_to_string(p.pending().join(format!("{rel}.json"))).unwrap();
        let mut row = serde_json::from_str::<Value>(&text).unwrap().as_object().unwrap().clone();
        row.remove("archived_at");
        row
    }

    fn hex(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn body(p: &Paths, rel: &str, ext: &str) -> Vec<u8> {
        std::fs::read(p.archive.join("bodies").join(format!("{rel}{ext}"))).unwrap()
    }

    fn other_rows(p: &Paths, n: usize) {
        let rows = (0..n)
            .map(|i| {
                let v = serde_json::json!({"source": format!("o/{i:04}.jsonl"), "session_id": format!("o{i}"), "bridge_session_id": "", "first_ts": format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60), "bytes": 1, "mtime": 1, "path": "bodies/x"});
                v.as_object().unwrap().clone()
            })
            .collect();
        std::fs::create_dir_all(&p.archive).unwrap();
        write_index(&p.index(), rows).unwrap();
    }

    #[test_id(tx8d2g2)]
    #[test]
    fn a_pass_over_a_grown_transcript_compresses_and_writes_only_what_it_gained() {
        let (_d, p) = world();
        other_rows(&p, 1000);
        let proc = Fake::new(false);
        let mut data = lines(0, 1000, 900);
        assert!(data.len() > 900_000);
        put(&p, REL, &data);
        run_store(&p, &proc, REL);
        index(&p).unwrap();
        let (body1, index1) = (body(&p, REL, ".zst"), std::fs::metadata(p.index()).unwrap().len());
        let first = proc.input();
        data.extend(lines(1000, 1, 900));
        put(&p, REL, &data);
        run_store(&p, &proc, REL);
        let msg = index(&p).unwrap();
        let body2 = body(&p, REL, ".zst");
        let index2 = std::fs::metadata(p.index()).unwrap().len();
        assert_eq!(proc.input() - first, (data.len() - body1.len()) as u64, "only the appended bytes were compressed");
        assert!(((body2.len() - body1.len()) as u64) + (index2 - index1) < 8 * 1024, "{msg}");
        assert_eq!(&body2[..body1.len()], &body1[..], "earlier body bytes are unchanged");
        assert_eq!(load_index(&p.index()).len(), 1001);
    }

    #[test_id(trbqwkq)]
    #[test]
    fn a_grown_transcript_row_equals_a_fresh_full_stores_including_a_completed_partial_line() {
        let (_d, p) = world();
        let proc = Fake::new(false);
        let mut data = lines(0, 50, 100);
        let tail = lines(50, 3, 100);
        data.extend_from_slice(&tail[..40]);
        put(&p, REL, &data);
        run_store(&p, &proc, REL);
        data.extend_from_slice(&tail[40..]);
        data.extend(lines(53, 5, 100));
        put_at(&p, REL, &data, 1_795_000_000);
        run_store(&p, &proc, REL);
        let grown = pending(&p, REL);
        let (_f, fresh_p) = world();
        put_at(&fresh_p, REL, &data, 1_795_000_000);
        run_store(&fresh_p, &Fake::new(false), REL);
        assert_eq!(grown, pending(&fresh_p, REL));
        assert_eq!(grown["sha256"], Value::String(hex(&data)));
        assert_eq!(grown["turns"], Value::from(58));
    }

    #[test_id(t8tu0y8)]
    #[test]
    fn the_sha256_state_resumes_across_every_split_of_the_input() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        for cut in [0, 1, 63, 64, 65, 127, 500, 1000] {
            let mut s = Sha::new();
            s.update(&data[..cut]);
            s.update(&data[cut..]);
            assert_eq!(s.hex(), hex(&data), "cut at {cut}");
        }
    }

    fn assert_full_copy(change: impl FnOnce(&Paths, &mut Vec<u8>), no_zstd_second: bool) {
        let (_d, p) = world();
        let proc = Fake::new(false);
        let mut data = lines(0, 200, 200);
        put(&p, REL, &data);
        run_store(&p, &proc, REL);
        change(&p, &mut data);
        let proc2 = Fake::new(no_zstd_second);
        run_store(&p, &proc2, REL);
        let ext = if no_zstd_second { ".gz" } else { ".zst" };
        assert!(if no_zstd_second { proc2.input() >= data.len() as u64 } else { proc2.input() == data.len() as u64 }, "the whole source was compressed again");
        assert_eq!(body(&p, REL, ext), data);
        assert_eq!(pending(&p, REL)["sha256"], Value::String(hex(&data)));
    }

    fn cursor_edit(p: &Paths, edit: impl FnOnce(&mut Value)) {
        let path = p.archive.join("cursors").join(format!("{REL}.json"));
        let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut v);
        std::fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
    }

    #[test_id(trzcdci)]
    #[test]
    fn a_missing_cursor_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            std::fs::remove_file(p.archive.join("cursors").join(format!("{REL}.json"))).unwrap();
        }, false);
    }

    #[test_id(tg75x8l)]
    #[test]
    fn an_unparseable_cursor_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            std::fs::write(p.archive.join("cursors").join(format!("{REL}.json")), b"{\"offset\": 12").unwrap();
        }, false);
    }

    #[test_id(tdyyib2)]
    #[test]
    fn a_cursor_for_another_inode_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            cursor_edit(p, |v| v["src_ino"] = Value::from(v["src_ino"].as_u64().unwrap() + 1));
        }, false);
    }

    #[test_id(tdvpeue)]
    #[test]
    fn a_cursor_whose_hash_state_disagrees_with_its_offset_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            cursor_edit(p, |v| v["sha"]["len"] = Value::from(1));
        }, false);
    }

    #[test_id(tttaswt)]
    #[test]
    fn a_source_over_one_frame_grows_by_only_the_appended_bytes() {
        let (_d, p) = world();
        let proc = Fake::new(false);
        let mut data = lines(0, 9_500, 1_000);
        assert!(data.len() > 9 << 20);
        put(&p, REL, &data);
        run_store(&p, &proc, REL);
        let first = proc.input();
        let before = body(&p, REL, ".zst");
        let tail = lines(9_500, 3, 1_000);
        data.extend_from_slice(&tail);
        put_at(&p, REL, &data, 1_795_000_000);
        run_store(&p, &proc, REL);
        assert_eq!(proc.input() - first, tail.len() as u64);
        let after = body(&p, REL, ".zst");
        assert_eq!(&after[..before.len()], &before[..]);
        let (_f, fresh) = world();
        put_at(&fresh, REL, &data, 1_795_000_000);
        run_store(&fresh, &Fake::new(false), REL);
        assert_eq!(pending(&p, REL), pending(&fresh, REL));
    }

    #[test_id(tdq7fme)]
    #[test]
    fn a_truncated_transcript_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.truncate(d.len() / 2);
            put(p, REL, d);
        }, false);
    }

    #[test_id(tcmy48f)]
    #[test]
    fn a_longer_replacement_file_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            let tmp = p.projects.join("p/new.tmp");
            std::fs::write(&tmp, &d).unwrap();
            std::fs::rename(&tmp, p.projects.join(REL)).unwrap();
        }, false);
    }

    #[test_id(te9smaf)]
    #[test]
    fn a_transcript_rewritten_longer_in_place_is_copied_whole() {
        assert_full_copy(|p, d| {
            let n = d.len();
            d[n - 50..n - 40].copy_from_slice(b"REWRITTEN!");
            d.extend(lines(200, 20, 200));
            let f = std::fs::OpenOptions::new().write(true).open(p.projects.join(REL)).unwrap();
            std::os::unix::fs::FileExt::write_all_at(&f, d, 0).unwrap();
        }, false);
    }

    #[test_id(tlces31)]
    #[test]
    fn a_body_replaced_by_another_writer_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            let body = p.archive.join("bodies").join(format!("{REL}.zst"));
            let tmp = body.with_extension("other");
            std::fs::write(&tmp, b"someone else's bytes").unwrap();
            std::fs::rename(&tmp, &body).unwrap();
        }, false);
    }

    #[test_id(tgbufmh)]
    #[test]
    fn a_body_longer_than_its_cursor_is_copied_whole() {
        assert_full_copy(|p, d| {
            d.extend(lines(200, 20, 200));
            put(p, REL, d);
            let body = p.archive.join("bodies").join(format!("{REL}.zst"));
            let mut f = std::fs::OpenOptions::new().append(true).open(body).unwrap();
            std::io::Write::write_all(&mut f, b"torn append").unwrap();
        }, false);
    }

    #[test_id(tpnkhma)]
    #[test]
    fn the_other_compressor_makes_a_full_copy_and_removes_the_old_body() {
        assert_full_copy(
            |p, d| {
                d.extend(lines(200, 20, 200));
                put(p, REL, d);
            },
            true,
        );
        let (_d, p) = world();
        put(&p, REL, &lines(0, 5, 10));
        run_store(&p, &Fake::new(false), REL);
        put(&p, REL, &lines(0, 9, 10));
        run_store(&p, &Fake::new(true), REL);
        assert!(!p.archive.join("bodies").join(format!("{REL}.zst")).exists());
    }

    #[test_id(tc7z6fe)]
    #[test]
    fn compaction_keeps_one_sorted_line_per_source() {
        let (_d, p) = world();
        std::fs::create_dir_all(&p.archive).unwrap();
        let row = |src: &str, first: &str, turns: u64| format!("{{\"source\": \"{src}\", \"session_id\": \"{src}\", \"first_ts\": \"{first}\", \"turns\": {turns}}}\n");
        let mut text = String::new();
        for t in 0..3 {
            text += &row("b", "2026-02-01T00:00:00Z", t);
            text += &row("a", "2026-03-01T00:00:00Z", t);
        }
        text.push_str("{\"torn");
        std::fs::write(p.index(), text).unwrap();
        let msg = index(&p).unwrap();
        assert!(msg.contains("index rewritten"), "{msg}");
        let rows = load_index(&p.index());
        assert_eq!(rows.iter().map(|r| r["source"].as_str().unwrap()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(rows[0]["turns"], Value::from(2));
        let on_disk = std::fs::read_to_string(p.index()).unwrap();
        assert_eq!(on_disk.lines().count(), 2);
        assert_eq!(index(&p).unwrap(), "0 row(s) merged, 2 in the index, index untouched");
    }

    #[test_id(tb347h5)]
    #[test]
    fn an_index_whose_last_line_is_torn_is_ended_before_a_row_is_appended() {
        let (_d, p) = world();
        std::fs::create_dir_all(p.archive.join("pending")).unwrap();
        std::fs::write(p.index(), "{\"source\": \"x\", \"first_ts\": \"2026-01-01T00:00:00Z\"}\n{\"source\": \"y\", \"fir").unwrap();
        std::fs::write(p.pending().join("z.json"), "{\"source\": \"z\", \"first_ts\": \"2026-01-02T00:00:00Z\"}").unwrap();
        index(&p).unwrap();
        let rows = load_index(&p.index());
        assert_eq!(rows.iter().map(|r| r["source"].as_str().unwrap()).collect::<Vec<_>>(), ["x", "z"]);
    }

    #[test_id(tkslv9v)]
    #[test]
    fn the_last_row_per_source_wins_and_the_bridge_id_is_inherited() {
        let (_d, p) = world();
        std::fs::create_dir_all(&p.archive).unwrap();
        std::fs::write(
            p.index(),
            concat!(
                "{\"source\": \"a\", \"session_id\": \"s\", \"bridge_session_id\": \"B\", \"first_ts\": \"2026-01-02T00:00:00Z\", \"turns\": 1}\n",
                "{\"source\": \"c\", \"session_id\": \"s\", \"bridge_session_id\": \"\", \"first_ts\": \"2026-01-01T00:00:00Z\", \"turns\": 1}\n",
                "{\"source\": \"a\", \"session_id\": \"s\", \"bridge_session_id\": \"B\", \"first_ts\": \"2026-01-02T00:00:00Z\", \"turns\": 2}\n",
            ),
        )
        .unwrap();
        let rows = load_index(&p.index());
        assert_eq!(rows.iter().map(|r| (r["source"].as_str().unwrap(), r["turns"].as_u64().unwrap(), r["bridge_session_id"].as_str().unwrap())).collect::<Vec<_>>(), [("c", 1, "B"), ("a", 2, "B")]);
    }
}
