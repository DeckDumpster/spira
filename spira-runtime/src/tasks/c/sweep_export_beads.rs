//! # sweep.export_beads — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-export-beads.md`;
//! its Acceptance section is the test file `tests/it/task_c_sweep_export_beads.rs`.
//!
//! ## Tick
//!
//! - **Today:** `OnActiveSec=10min`, `OnUnitActiveSec=6h`, `AccuracySec=5min`, `Persistent=true`
//!   (`~/.config/systemd/user/spira-export-beads-prod.timer`). `Persistent=` applies only to
//!   `OnCalendar=`, so it is **inert** here: after downtime the timer does not catch up, it fires
//!   10 min after activation and every 6 h after the last run. (The brief said only
//!   `OnUnitActiveSec`; there is also an `OnActiveSec` anchor.)
//! - **Contract:** interval 6 h, first tick 10 min after boot; no catch-up rule (monotonic). If a
//!   missed window matters, heimdall's `mirror-stale` alarm already says so
//!   (`heimdall/src/judge.rs:313-326`).
//! - **Single-flight:** yes. No `world guard`.
//!
//! ## Observe
//!
//! `sweep.export_beads.observe` (probe, store, deadline 300 s — a full-store dump) returns:
//!
//! ```text
//! Snapshot { content: Vec<Bead>, lifecycle: BTreeMap<id, state>, memories: BTreeMap<k, v>,
//!            watermarks: String, mirrored_count: usize, mirror_bytes_sha: String }
//! ```
//!
//! - `content` = store verb `content list --id <chunk> --all --limit 0 --json`, one query per `CONTENT_CHUNK` lifecycle ids (300 s in all); `lifecycle` = `list-all`
//!   (`:29`, 120 s); `memories` = `content memories --json` (`:30`, 60 s, absent → `{}`);
//!   `watermarks` = `plane-watermarks --manifest` (`:32`, 5 s). All four through the in-process
//!   `LcClient` (S7a), not the `spira-lc` CLI.
//! - `mirrored_count` / sha: the current `$SPIRA_WIKI/raw/spira-beads/spira.jsonl` (`:35`).
//! - `SPIRA_WIKI` empty → the probe reports `disabled` (`:16-19`).
//!
//! ## Decide
//!
//! `decide(snap) -> Unchanged | Write(jsonl, manifest) | Refuse(reason)`, pure, ported from the
//! embedded Python (`:37-55`). **Load-bearing properties, verified in the script:**
//! 1. **sorted by id** — `sorted(issues, key=id)` (`:50`), `sort_keys` per line (`:52`); memories follow, sorted by key (`:53-54`). ✔
//! 2. **commit only when the .jsonl changes** — an identical render exits before the manifest is
//!    rewritten or git is touched (`:57-61`); the manifest timestamp alone never commits. ✔
//! 3. **refuse without an `sp-` bead** — `if not issues: sys.exit(...)` (`:41-42`). ✔
//! 4. Not in CLAUDE.md but also load-bearing: **refuse to shrink below half** the mirrored count (`:43-44`).
//!
//! **Fixtures:** store dumps of 0 beads, 1 bead, half-shrink, unchanged, changed.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.export_beads.write` | host | 30 s | `export/<sha of new jsonl>` | a render `decide` refused; a `SPIRA_WIKI` that is not a directory (`:20`) |
//! | `sweep.export_beads.commit` | git | 30 s (3 tries on `index.lock`, `:82-94`) | `export/commit/<sha>` | a wiki that is not a git checkout (skipped, `:96`) |
//! | `sweep.export_beads.publish` | git | 60 s pull + 60 s push, 2 attempts (`:101-115`) | `export/push/<sha>` | a failed `pull --rebase` (aborts the rebase, `:105-107`) |
//! | `sweep.export_beads.touch` | host | 5 s | `export/touch/<tick>` | — |
//!
//! `write` writes `spira.jsonl` atomically (`:62`) and `manifest.md` (`:68-80`); `commit` stages and
//! commits both (`:97-99`); `publish` pull-rebases and pushes the brain remote. `touch` exists
//! because an unchanged pass still bumps the mirror's mtime (`:58`), which is what heimdall's
//! `mirror-stale` reads (`heimdall/src/main.rs:161`).
//!
//! ## Silence
//!
//! An unchanged pass emits only `touch`. **Proposed:** heimdall reads this task's last successful
//! observe from `scheduler status` (S13) instead of the file's mtime; then `touch` goes and an idle
//! tick is fully silent. Until then T2 allows exactly one `touch` per tick (4 per day).
//!
//! ## Replaces
//!
//! - Unit `spira-export-beads-prod.service` / `.timer`; output `run/export-beads.log`.
//! - Children: `bash lib.sh` (`:14`), `spira-lc` ×4, `python3` ×2, `git` (stays), `grep`, `cmp`, `date`, `mktemp`.
//! - Config keys pinned: `SPIRA_WIKI` (default empty, `spira/conf.d.defaults.generated.sh:1228`),
//!   `SPIRA_EXPORT_GIT_RETRY_WAIT` (default 10, `:88`).
//! - Commit message `chore: spira beads JSONL mirror <date> (<n> beads)` (`:98`) kept verbatim.
//!
//! ## Reverse
//!
//! Release rollback restores unit and timer. `Observe` shadows `write`/`commit`/`publish`; heimdall's
//! `mirror-stale` fires after its threshold, which is the intended signal.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use spira_config::lc_call::LcClient;

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.export_beads";
pub const STREAM: &str = "task.sweep.export_beads";
pub const TICK: EventKindTag = "sweep.export_beads.tick";
pub const STATE: EventKindTag = "sweep.export_beads.state";

pub const OBSERVE: &str = "sweep.export_beads.observe";
pub const WRITE: &str = "sweep.export_beads.write";
pub const COMMIT: &str = "sweep.export_beads.commit";
pub const PUBLISH: &str = "sweep.export_beads.publish";
pub const TOUCH: &str = "sweep.export_beads.touch";
pub const EFFECTS: &[&str] = &[OBSERVE, WRITE, COMMIT, PUBLISH, TOUCH];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_EXPORT_BEADS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_WIKI", "SPIRA_EXPORT_GIT_RETRY_WAIT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 300_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// `content list --all --json`, one value per bead (each has an `id`).
    pub content: Vec<serde_json::Value>,
    pub lifecycle: BTreeMap<String, String>,
    pub memories: BTreeMap<String, String>,
    pub watermarks: String,
    pub mirrored_count: usize,
    pub mirror_bytes_sha: String,
    /// `SPIRA_WIKI` empty.
    pub disabled: bool,
}

/// T3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Rendered,
    Written,
    Committed,
    Published,
    Refused,
}

/// `state × event → state`. `publish` before `committed` is refused naming `written`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Rendered, "write:ok", Phase::Written), (Phase::Written, "commit:ok", Phase::Committed), (Phase::Committed, "publish:ok", Phase::Published), (Phase::Rendered, "refuse", Phase::Refused), (Phase::Published, "observed:changed", Phase::Rendered), (Phase::Refused, "observed:changed", Phase::Rendered)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    /// The sha of the render being carried through write → commit → publish.
    pub sha: Option<String>,
    /// `no-sp` or `shrink`.
    pub refused: Option<String>,
}

pub struct SweepExportBeads {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// The mirror, relative to the wiki checkout.
pub const REL: &str = "raw/spira-beads/spira.jsonl";
pub const MANIFEST: &str = "raw/spira-beads/manifest.md";

/// The JSONL mirror of `snap`: every `sp-` bead sorted by id, each with its `lifecycle_state`,
/// keys sorted and compact, then the memories sorted by key. `Err` is the refusal: `no-sp`
/// when the store returned no `sp-` bead, `shrink` when it returned fewer than half the beads
/// already mirrored.
pub fn render(snap: &Snapshot) -> Result<String, &'static str> {
    use crate::tasks::c::sweep_archive::archive::py_json;
    let id = |v: &serde_json::Value| v.get("id").and_then(|i| i.as_str()).map(str::to_string).unwrap_or_default();
    let mut issues: Vec<serde_json::Value> = snap.content.iter().filter(|v| id(v).starts_with("sp-")).cloned().collect();
    if issues.is_empty() {
        return Err("no-sp");
    }
    if snap.mirrored_count > 0 && issues.len() * 2 < snap.mirrored_count {
        return Err("shrink");
    }
    issues.sort_by_key(|v| id(v));
    let mut out = String::new();
    for mut i in issues {
        let state = snap.lifecycle.get(&id(&i)).cloned().unwrap_or_default();
        if let Some(m) = i.as_object_mut() {
            m.insert("lifecycle_state".into(), serde_json::Value::String(state));
        }
        out.push_str(&py_json(&i, ",", ":"));
        out.push('\n');
    }
    for (k, v) in &snap.memories {
        out.push_str(&py_json(&serde_json::json!({"_type": "memory", "key": k, "value": v}), ",", ":"));
        out.push('\n');
    }
    Ok(out)
}

/// The sha256 of `text`, as lowercase hex: what the probe reports of the mirror's bytes.
pub fn sha_hex(text: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, text).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Rendered => "rendered",
        Phase::Written => "written",
        Phase::Committed => "committed",
        Phase::Published => "published",
        Phase::Refused => "refused",
    }
}

fn act(name: &str, input: String, deadline_ms: u64) -> Act {
    Act { name: name.into(), input, deadline_ms, probe: false }
}

/// `Unchanged | Write(jsonl, manifest) | Refuse(reason)`, sorted by id, refusing without an
/// `sp-` bead or below half the mirrored count. An unchanged render finishes a pass a failed
/// commit or push left behind before it only touches the mirror.
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(s) if s.disabled => Ok((carry.clone(), vec![])),
        Input::Observed(s) => match render(s) {
            Err(why) => Ok((Carry { phase: Phase::Refused, sha: None, refused: Some(why.into()) }, vec![])),
            Ok(text) => {
                let sha = sha_hex(text.as_bytes());
                if sha != s.mirror_bytes_sha {
                    return Ok((Carry { phase: Phase::Rendered, sha: Some(sha), refused: None }, vec![act(WRITE, text, 30_000)]));
                }
                let pending = carry.sha.as_deref() == Some(sha.as_str());
                match carry.phase {
                    Phase::Written if pending => Ok((carry.clone(), vec![act(COMMIT, sha, 60_000)])),
                    Phase::Committed if pending => Ok((carry.clone(), vec![act(PUBLISH, sha, 150_000)])),
                    _ => Ok((carry.clone(), vec![act(TOUCH, String::new(), 5_000)])),
                }
            }
        },
        Input::Outcome { effect, ok, body } if [WRITE, COMMIT, PUBLISH].contains(&effect.as_str()) => {
            let (want, to, then) = match effect.as_str() {
                WRITE => (Phase::Rendered, Phase::Written, Some((COMMIT, 60_000))),
                COMMIT => (Phase::Written, Phase::Committed, Some((PUBLISH, 150_000))),
                _ => (Phase::Committed, Phase::Published, None),
            };
            if carry.phase != want {
                return Err(format!("{NAME}: in state {} a {effect} outcome is refused; it needs {}", phase_name(carry.phase), phase_name(want)));
            }
            if !*ok {
                return Ok((carry.clone(), vec![]));
            }
            let sha = carry.sha.clone().unwrap_or_default();
            if body.trim() != sha {
                return Err(format!("{NAME}: in state {} the {effect} answered {} for render {sha}", phase_name(carry.phase), body.trim()));
            }
            let next = Carry { phase: to, ..carry.clone() };
            Ok((next, then.map(|(n, d)| vec![act(n, sha, d)]).unwrap_or_default()))
        }
        Input::Outcome { effect, .. } if effect == TOUCH => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} no outcome of {effect} is expected", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// Beads read per content query: one unpaged dump of the whole store is a sort the store
/// spills to its temp spool.
pub const CONTENT_CHUNK: usize = 200;

/// The runner of [`OBSERVE`]: the content, lifecycle and memory dumps and the planes'
/// watermarks over the socket, and what is mirrored now.
pub fn observe(lc: &LcClient, wiki: &str) -> Result<String, String> {
    if wiki.trim().is_empty() {
        return serde_json::to_string(&Snapshot { disabled: true, ..Snapshot::default() }).map_err(|e| e.to_string());
    }
    let lifecycle: BTreeMap<String, String> = lc
        .answer(&["list-all"])
        .map_err(|e| format!("lifecycle list failed: {e}"))?
        .lines()
        .filter_map(|l| {
            let mut parts = l.split('\t');
            let (id, state) = (parts.next()?, parts.next()?);
            id.starts_with("sp-").then(|| (id.to_string(), state.to_string()))
        })
        .collect();
    let ids: Vec<&str> = lifecycle.keys().map(String::as_str).collect();
    let mut content: Vec<serde_json::Value> = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(CONTENT_CHUNK) {
        let ids = chunk.join(",");
        let page = lc.answer(&["content", "list", "--id", &ids, "--all", "--limit", "0", "--json"]).map_err(|e| format!("content list failed: {e}"))?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(&page).map_err(|e| format!("content list did not answer JSON: {e}"))?;
        content.extend(rows);
    }
    let memories = lc
        .answer(&["content", "memories", "--json"])
        .ok()
        .and_then(|t| serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
        .collect();
    let watermarks = match lc.call(&["plane-watermarks", "--manifest"]) {
        Ok(r) if r.code == 0 => r.text(),
        Ok(r) => format!("Planning watermarks: unreadable ({}).", r.text()),
        Err(e) => format!("Planning watermarks: unreadable ({e})."),
    };
    let mirror = std::fs::read(std::path::Path::new(wiki).join(REL)).unwrap_or_default();
    let text = String::from_utf8_lossy(&mirror);
    let mirrored_count = text.lines().filter(|l| l.split("\"id\":").nth(1).is_some_and(|r| r.trim_start().starts_with("\"sp-"))).count();
    let mirror_bytes_sha = if mirror.is_empty() { String::new() } else { sha_hex(&mirror) };
    serde_json::to_string(&Snapshot { content, lifecycle, memories, watermarks, mirrored_count, mirror_bytes_sha, disabled: false }).map_err(|e| e.to_string())
}

fn mirror_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| l.contains("\"id\":\"sp-"))
}

/// The runner of [`WRITE`]: the mirror replaced atomically and its manifest rewritten. Answers
/// the render's sha.
pub fn write(lc: &LcClient, wiki: &str, text: &str, now_secs: u64) -> Result<String, String> {
    let root = std::path::Path::new(wiki);
    if !root.is_dir() {
        return Err(format!("SPIRA_WIKI '{wiki}' is not a directory"));
    }
    let out = root.join(REL);
    let dir = out.parent().unwrap_or(root);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = out.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    let count = mirror_lines(text).count();
    let newest = mirror_lines(text).filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter_map(|v| v.get("created_at").and_then(|c| c.as_str()).map(str::to_string)).max().unwrap_or_default();
    let watermarks = match lc.call(&["plane-watermarks", "--manifest"]) {
        Ok(r) if r.code == 0 => r.text(),
        Ok(r) => format!("Planning watermarks: unreadable ({}).", r.text()),
        Err(e) => format!("Planning watermarks: unreadable ({e})."),
    };
    let stamp = crate::tasks::c::sweep_archive::archive::fmt_ts(now_secs as i64);
    let manifest = format!(
        "# Spira beads — JSONL mirror\n\nMachine-written by `spira-runtime` (task `{NAME}`). **Do not hand-edit**: the next run overwrites it.\n\nSnapshot: **{} {} UTC** — {count} beads, newest created {newest}.\n\n{watermarks}\n\n`spira.jsonl` is the current mirror. A `spira-pre-*.jsonl` beside it is frozen history from before\na store reset, never current. Each bead carries `lifecycle_state` (authoritative) beside the\ncontent store's `status`; dependencies and comments are not mirrored.\n",
        &stamp[..10],
        &stamp[11..16]
    );
    std::fs::write(root.join(MANIFEST), manifest).map_err(|e| format!("{MANIFEST}: {e}"))?;
    Ok(sha_hex(text.as_bytes()))
}

/// `git -C <wiki> <args>` through `proc`: `Ok` on exit 0, else git's own words.
fn git(proc: &dyn Proc, wiki: &str, args: &[&str], secs: u64) -> Result<String, String> {
    let mut argv = vec!["-C".to_string(), wiki.to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let o = proc.run(&ProcSpec { prog: "git".into(), args: argv, stdin: vec![], deadline: Duration::from_secs(secs) })?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if o.code == Some(0) {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

/// A git step retried only on `index.lock` contention, three tries `wait` apart.
fn git_step(proc: &dyn Proc, wiki: &str, args: &[&str], wait: Duration) -> Result<(), String> {
    for n in 1..=3 {
        match git(proc, wiki, args, 30) {
            Ok(_) => return Ok(()),
            Err(e) if n < 3 && e.contains("index.lock") => std::thread::sleep(wait),
            Err(e) => return Err(format!("git {} failed (try {n} of 3): {e}", args[0])),
        }
    }
    Ok(())
}

/// The runner of [`COMMIT`]: the mirror and manifest staged and committed, when the wiki is a
/// git checkout (otherwise nothing to do). Answers the sha.
pub fn commit(proc: &dyn Proc, wiki: &str, retry_wait: &str, sha: &str, now_secs: u64) -> Result<String, String> {
    if git(proc, wiki, &["rev-parse", "--git-dir"], 10).is_err() {
        return Ok(sha.to_string());
    }
    let wait = Duration::from_secs(retry_wait.trim().parse().map_err(|_| format!("SPIRA_EXPORT_GIT_RETRY_WAIT must be seconds, got {retry_wait:?}"))?);
    let text = std::fs::read_to_string(std::path::Path::new(wiki).join(REL)).map_err(|e| format!("{REL}: {e}"))?;
    if sha_hex(text.as_bytes()) != sha {
        return Err(format!("the mirror is no longer render {sha} — refusing to commit it"));
    }
    let date = &crate::tasks::c::sweep_archive::archive::fmt_ts(now_secs as i64)[..10];
    let msg = format!("chore: spira beads JSONL mirror {date} ({} beads)", mirror_lines(&text).count());
    git_step(proc, wiki, &["add", "--", REL, MANIFEST], wait)?;
    git_step(proc, wiki, &["commit", "-q", "-m", &msg, "--", REL, MANIFEST], wait)?;
    Ok(sha.to_string())
}

/// The runner of [`PUBLISH`]: pull-rebase and push the brain remote, two attempts; a failed
/// pull aborts its rebase. Nothing to do without a checkout or a remote. Answers the sha.
pub fn publish(proc: &dyn Proc, wiki: &str, sha: &str) -> Result<String, String> {
    if git(proc, wiki, &["rev-parse", "--git-dir"], 10).is_err() || git(proc, wiki, &["remote"], 10).map(|r| r.trim().is_empty()).unwrap_or(true) {
        return Ok(sha.to_string());
    }
    let mut last = String::new();
    for _ in 0..2 {
        if let Err(e) = git(proc, wiki, &["pull", "--rebase", "--autostash"], 60) {
            let _ = git(proc, wiki, &["rebase", "--abort"], 10);
            return Err(format!("git pull --rebase failed: {e}"));
        }
        match git(proc, wiki, &["push"], 60) {
            Ok(_) => return Ok(sha.to_string()),
            Err(e) => last = e,
        }
    }
    Err(format!("git push refused or failed: {last}"))
}

/// The runner of [`TOUCH`]: the mirror's mtime bumped, which heimdall's `mirror-stale` reads.
pub fn touch(wiki: &str) -> Result<String, String> {
    let out = std::path::Path::new(wiki).join(REL);
    std::fs::OpenOptions::new().append(true).open(&out).and_then(|f| f.set_modified(std::time::SystemTime::now())).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok("touched".into())
}

crate::tasks::b::hosted!(SweepExportBeads);
