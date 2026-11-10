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
//! **Fixtures:** `spira/test-archive-no-sessions.sh` and the archive test scripts' trees, ported.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.archive.store` (one per changed rel, one in flight at a time) | host | 300 s per transcript | `archive/<rel>/<size>/<mtime>` | a source outside `$SPIRA_TOKEN_PROJECTS` (`:350-352`); an unreadable source (skipped, `:345-348`); the lock held > 900 s by the hook (`:75-77`) |
//! | `sweep.archive.index` | host | 60 s | `archive/index/<sha of row set>` | — ; replaces the index only when the bytes differ (`:241-266`) |
//!
//! `store` = snapshot the first `size` bytes (`:168-181`), scan+sha256 (`:129-158`), compress to
//! `bodies/<rel>.zst|.gz` atomically and remove the other extension (`:204-220`). Ported to Rust;
//! the compressor becomes in-process (a zstd crate) or stays a child — **`zstd`/`gzip` are not on
//! the W1 allow-list** (§6), so one of the two must change (unverified which crate is vendored).
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

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 60_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

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
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
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
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// `archive.sh sweep`'s worker, in-process: the probe's stat walk, a body's snapshot, scan and
/// compression, and the index rewrite. The compressor (`zstd`, else `gzip`) stays a child, run
/// through `runtime::ports::Proc`.
pub mod archive {
    use std::collections::BTreeMap;
    use std::io::{BufRead, Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use runtime::ports::{Proc, ProcSpec};
    use serde_json::{Map, Value};

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

    /// The index rows, each as its JSON object; unparseable lines are skipped.
    pub fn load_index(path: &Path) -> Vec<Map<String, Value>> {
        let Ok(f) = std::fs::File::open(path) else { return vec![] };
        std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).filter_map(|v| v.as_object().cloned()).collect()
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

    /// What one pass over a transcript reads.
    pub struct Scan {
        pub sha256: String,
        pub first_ts: Option<String>,
        pub last_ts: Option<String>,
        /// Lines carrying `"type": "assistant"`.
        pub turns: u64,
        pub bridge: String,
        /// The session id, or else the parent session id.
        pub session: String,
    }

    /// One pass over a transcript, hashing as it reads.
    pub fn scan(path: &Path) -> Result<Scan, String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut r = std::io::BufReader::new(f);
        let mut h = ring::digest::Context::new(&ring::digest::SHA256);
        let (mut first, mut last): (Option<Vec<u8>>, Option<Vec<u8>>) = (None, None);
        let (mut turns, mut bridge, mut sess, mut parent) = (0u64, String::new(), String::new(), String::new());
        let mut line = vec![];
        loop {
            line.clear();
            if r.read_until(b'\n', &mut line).map_err(|e| e.to_string())? == 0 {
                break;
            }
            h.update(&line);
            if values(&line, "type", 0, usize::MAX).any(|v| v == b"assistant") {
                turns += 1;
            }
            if let Some(t) = values(&line, "timestamp", 10, 40).next() {
                if first.as_deref().is_none_or(|f| t < f) {
                    first = Some(t.to_vec());
                }
                if last.as_deref().is_none_or(|l| t > l) {
                    last = Some(t.to_vec());
                }
            }
            for (key, slot) in [("bridgeSessionId", &mut bridge), ("sessionId", &mut sess), ("parentSessionId", &mut parent)] {
                if slot.is_empty() {
                    if let Some(v) = values(&line, key, 1, 120).next() {
                        *slot = String::from_utf8_lossy(v).into_owned();
                    }
                }
            }
        }
        let hex: String = h.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let text = |v: Option<Vec<u8>>| v.map(|v| String::from_utf8_lossy(&v).into_owned());
        Ok(Scan { sha256: hex, first_ts: text(first), last_ts: text(last), turns, bridge, session: if sess.is_empty() { parent } else { sess } })
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

    /// Sorted by `(first_ts, source)`, the bridge id inherited by session id, rewritten whole and
    /// replaced only when the bytes differ. Returns whether it was replaced.
    pub fn write_index(path: &Path, mut rows: Vec<Map<String, Value>>) -> Result<bool, String> {
        let s = |r: &Map<String, Value>, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let bridge: BTreeMap<String, String> = rows.iter().filter(|r| !s(r, "bridge_session_id").is_empty()).map(|r| (s(r, "session_id"), s(r, "bridge_session_id"))).collect();
        for r in rows.iter_mut() {
            if s(r, "bridge_session_id").is_empty() {
                let b = bridge.get(&s(r, "session_id")).cloned().unwrap_or_default();
                r.insert("bridge_session_id".into(), Value::String(b));
            }
        }
        rows.sort_by_key(|r| (s(r, "first_ts"), s(r, "source")));
        let body: String = rows.into_iter().map(|r| py_dumps(&Value::Object(r)) + "\n").collect();
        if std::fs::read(path).ok().as_deref() == Some(body.as_bytes()) {
            return Ok(false);
        }
        let tmp = path.with_extension("jsonl.part");
        std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(true)
    }

    /// Compresses `src` to `dest` atomically with `zstd`, else `gzip`, and removes a body the
    /// other compressor wrote. `dest` is given without its extension; returns the extension.
    fn compress(proc: &dyn Proc, src: &Path, dest_stem: &Path) -> Result<&'static str, String> {
        let s = src.display().to_string();
        let tries: [(&str, &'static str, Vec<String>); 2] = [("zstd", ".zst", vec!["-q".into(), "-c".into(), s.clone()]), ("gzip", ".gz", vec!["-n".into(), "-q".into(), "-c".into(), s])];
        let mut why = vec![];
        for (prog, ext, args) in tries {
            // batch-job: compresses a whole archived file; it runs as long as the file is large
            match proc.run(&ProcSpec { prog: prog.into(), args, stdin: vec![], deadline: Duration::from_secs(240) }) {
                Ok(o) if o.code == Some(0) => {
                    let dest = PathBuf::from(format!("{}{ext}", dest_stem.display()));
                    let tmp = PathBuf::from(format!("{}.part", dest.display()));
                    std::fs::File::create(&tmp).and_then(|mut f| f.write_all(&o.stdout)).map_err(|e| format!("{}: {e}", tmp.display()))?;
                    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
                    for other in [".zst", ".gz"].into_iter().filter(|o| *o != ext) {
                        let _ = std::fs::remove_file(format!("{}{other}", dest_stem.display()));
                    }
                    return Ok(ext);
                }
                Ok(o) => return Err(format!("{prog} failed on {} (exit {:?})", src.display(), o.code)),
                Err(e) => why.push(e),
            }
        }
        Err(format!("neither zstd nor gzip ran — one of them stores the bodies: {}", why.join("; ")))
    }

    /// One transcript archived: its first `size` bytes snapshotted, scanned and hashed,
    /// compressed into `bodies/<rel>.zst|.gz`, and its index row left in `pending/` for the
    /// index pass. Returns the rel; a failure is `<rel>: why`.
    pub fn store(p: &Paths, proc: &dyn Proc, job: &Store, now: i64) -> Result<String, String> {
        let fail = |why: String| format!("{}: {why}", job.rel);
        if job.rel.split('/').any(|c| c == "..") || Path::new(&job.rel).is_absolute() {
            return Err(fail(format!("outside {}", p.projects.display())));
        }
        let src = p.projects.join(&job.rel);
        let st = std::fs::metadata(&src).map_err(|e| fail(format!("cannot read: {e}")))?;
        let _lock = Lock::take(&p.archive, Duration::from_secs(240)).map_err(fail)?;
        let stem = p.archive.join("bodies").join(&job.rel);
        let snap = PathBuf::from(format!("{}.snap", stem.display()));
        let made = (|| -> Result<(Map<String, Value>, &'static str), String> {
            std::fs::create_dir_all(snap.parent().unwrap_or(&p.archive)).map_err(|e| e.to_string())?;
            let mut fi = std::fs::File::open(&src).map_err(|e| e.to_string())?.take(st.len());
            let mut fo = std::fs::File::create(&snap).map_err(|e| e.to_string())?;
            std::io::copy(&mut fi, &mut fo).map_err(|e| e.to_string())?;
            drop(fo);
            let Scan { sha256: sha, first_ts: first, last_ts: last, turns, bridge, session: sess } = scan(&snap)?;
            let mtime = mtime_of(&st) as i64;
            let (f, l) = (first.as_deref().and_then(parse_ts), last.as_deref().and_then(parse_ts));
            let kind = if f.is_some() && l.is_some() { "transcript" } else { "mtime" };
            let bytes = std::fs::metadata(&snap).map(|m| m.len()).map_err(|e| e.to_string())?;
            let ext = compress(proc, &snap, &stem)?;
            let stem_name = Path::new(&job.rel).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut row = Map::new();
            row.insert("session_id".into(), Value::String(if sess.is_empty() { stem_name } else { sess }));
            row.insert("bridge_session_id".into(), Value::String(bridge));
            row.insert("cwd_slug".into(), Value::String(job.rel.split('/').next().unwrap_or_default().to_string()));
            row.insert("first_ts".into(), Value::String(fmt_ts(f.unwrap_or(mtime))));
            row.insert("last_ts".into(), Value::String(fmt_ts(l.unwrap_or(mtime))));
            row.insert("ts_source".into(), Value::String(kind.into()));
            row.insert("turns".into(), Value::from(turns));
            row.insert("bytes".into(), Value::from(bytes));
            row.insert("mtime".into(), Value::from(mtime));
            row.insert("sha256".into(), Value::String(sha));
            row.insert("source".into(), Value::String(job.rel.clone()));
            row.insert("path".into(), Value::String(format!("bodies/{}{ext}", job.rel)));
            row.insert("archived_at".into(), Value::String(fmt_ts(now)));
            Ok((row, ext))
        })();
        let _ = std::fs::remove_file(&snap);
        let (row, _) = made.map_err(fail)?;
        let pending = p.pending().join(format!("{}.json", job.rel));
        std::fs::create_dir_all(pending.parent().unwrap_or(&p.archive)).map_err(|e| fail(e.to_string()))?;
        std::fs::write(&pending, py_dumps(&Value::Object(row))).map_err(|e| fail(e.to_string()))?;
        Ok(job.rel.clone())
    }

    /// The index pass: every pending row merged over the index by source (rows whose source
    /// vanished are kept), the index rewritten when its bytes differ, the merged rows cleared.
    pub fn index(p: &Paths) -> Result<String, String> {
        let _lock = Lock::take(&p.archive, Duration::from_secs(50))?;
        let mut by_rel: BTreeMap<String, Map<String, Value>> = load_index(&p.index()).into_iter().filter_map(|r| Some((r.get("source")?.as_str()?.to_string(), r))).collect();
        let mut merged = vec![];
        let mut files = vec![];
        walk_json(&p.pending(), &mut files);
        for f in files {
            let Some(row) = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()) else { continue };
            let Some(rel) = row.get("source").and_then(Value::as_str).map(str::to_string) else { continue };
            by_rel.insert(rel, row);
            merged.push(f);
        }
        let n = by_rel.len();
        let changed = write_index(&p.index(), by_rel.into_values().collect())?;
        for f in &merged {
            let _ = std::fs::remove_file(f);
        }
        Ok(format!("{} row(s) merged, {n} in the index, {}", merged.len(), if changed { "index rewritten" } else { "index untouched" }))
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

const ROW_PARTS: &[runtime::rows::Part] = &[];

crate::tasks::b::hosted!(SweepArchive);
