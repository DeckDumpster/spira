//! Orchestration: `sweep`, `list`, `now`, `mark`, `state`, `record`, `digest-send`, and
//! the one-session `archive()` they all bottom out in. Composes the pure modules
//! ([`crate::state`], [`crate::candidates`], [`crate::digest`], [`crate::prompt`]) with
//! the impure boundary ([`crate::seam::Seam`], [`crate::lock`], and plain `std::fs` for
//! this crate's own private state files, which no other script reads or writes).

use std::path::{Path, PathBuf};

use crate::candidates::{self, Candidate};
use crate::config::Env;
use crate::lock::{self, Acquire};
use crate::prompt;
use crate::seam::{AgentSpec, Seam};
use crate::state::{self, State, ARC_RC_CAPACITY};
use crate::transcripts;

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` — lib.sh's `log()` timestamp, without a date library.
pub fn utc_now() -> String {
    let epoch = now_epoch();
    let days = epoch.div_euclid(86_400);
    let s = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, (s % 3600) / 60, s % 60)
}

/// lib.sh's `log`: `<ts> spira: <msg>` on stdout.
pub fn log(msg: &str) -> String {
    let line = format!("{} spira: {msg}", utc_now());
    println!("{line}");
    line
}

/// The covered cursor, raw — `None` if the file is absent; may hold a non-numeric
/// sentinel (`-`/`?`) if a prior meter read was unreadable. Callers coerce it through
/// [`numeric_logged`], which is where `arc_numeric`'s log line lives.
fn covered_raw(arc: &Path, sid: &str) -> Option<String> {
    let text = std::fs::read_to_string(arc.join(format!("{sid}.covered"))).ok()?;
    state::key(&text, "turn")
}

fn set_covered(arc: &Path, sid: &str, turn: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(arc)?;
    let tmp = arc.join(format!(".{sid}.covered.{}", std::process::id()));
    std::fs::write(&tmp, format!("turn={turn}\n"))?;
    std::fs::rename(&tmp, arc.join(format!("{sid}.covered")))
}

/// `arc_numeric`: coerce a meter or cursor value to a non-negative integer, LOGGING the
/// coercion — the established "unreadable" sentinel in this codebase is `-` or `?`, and a
/// sweep that silently treated one as 0 would be indistinguishable from one that read a
/// real 0, which is exactly the ambiguity this log line exists to remove.
fn numeric_logged(v: Option<&str>, context: &str) -> u64 {
    let (n, ok) = state::numeric_or_zero(v);
    if !ok {
        log(&format!(
            "archivist: {context} is '{}' — treating as 0 (not covered)",
            v.filter(|s| !s.is_empty()).unwrap_or("(empty)")
        ));
    }
    n
}

/// What came before this transcript, as a hint for the agent — never a dependency: a
/// session cleared minutes ago may not be indexed yet, in which case this reads as a
/// chain of one.
fn lineage_brief(seam: &dyn Seam, sid: &str, tp: &str) -> String {
    let rows = seam.archive_lineage(sid);
    let mut lines = Vec::new();
    for line in rows.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(source) = v.get("source").and_then(|s| s.as_str()) else { continue };
        if source == tp {
            continue;
        }
        let turns = v.get("turns").map(|t| t.to_string()).unwrap_or_else(|| "null".into());
        let last_ts = v.get("last_ts").and_then(|s| s.as_str()).unwrap_or("null").to_string();
        lines.push(format!("    {source}  ({turns} turns, up to {last_ts})"));
    }
    if lines.is_empty() {
        "A chain of one — nothing earlier belongs to this conversation, or the archive has\nnot indexed it yet.".to_string()
    } else {
        format!(
            "This session is a continuation. Earlier transcripts in the same conversation,\noldest first:\n\n{}\n\nThey were swept when they were live, so read one only to resolve something the\ncurrent transcript refers to and does not contain.",
            lines.join("\n")
        )
    }
}

/// One archive run, mirroring `archivist.sh archive()`. `wait`: the archivist-wide lock
/// blocks rather than skips (the manual `now` path; an operator told "busy" would just
/// run it again in a loop).
pub fn archive(seam: &dyn Seam, cfg: &Env, arc: &Path, sid: &str, tp: &Path, at: u64, ctx: &str, why: &str, wait: bool) -> i32 {
    let from = numeric_logged(covered_raw(arc, sid).as_deref(), &format!("session {sid} prior cursor"));
    let tc_file = arc.join(format!("{sid}.timeout_count"));
    let mut tc: u32 = std::fs::read_to_string(&tc_file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let effective_timeout = cfg.timeout * (tc as u64 + 1);

    let prompt_file = Path::new(&cfg.chamber).join("archivist.md");
    let Ok(template) = std::fs::read_to_string(&prompt_file) else {
        log(&format!("archivist: no prompt at {}", prompt_file.display()));
        return 1;
    };
    let _ = std::fs::create_dir_all(arc.join("cwd"));

    let wide_path = arc.join("archivist.lock");
    let _wide = match if wait { lock::wait_lock(&wide_path) } else { lock::try_lock(&wide_path) } {
        Acquire::Held(g) => g,
        _ => {
            log(&format!(
                "archivist: {sid} {}",
                if wait { "could not take the archivist-wide lock" } else { "skipped — another archive is already running" }
            ));
            return 75;
        }
    };
    let session_path = arc.join(format!("{sid}.lock"));
    let _session = match lock::try_lock(&session_path) {
        Acquire::Held(g) => g,
        _ => {
            log(&format!("archivist: {sid} is already being archived"));
            return 75;
        }
    };

    let _ = state::write_state(arc, sid, State::Sweeping, at, 0);

    let lineage = lineage_brief(seam, sid, &tp.to_string_lossy());
    let wiki_text = match cfg.wiki.as_deref().filter(|w| !w.is_empty()) {
        Some(w) => format!(
            "Craft knowledge — how a tool really behaves, which approach failed and why, the\nshape of a recurring hazard — goes on a page under `{w}`, not into a bead.\nAppend to the page it belongs on; create one only if none fits."
        ),
        None => "There is no wiki configured on this installation, so craft knowledge has nowhere\nbetter to go than an insight. Record it as one and say in the body that it wants a\nhome.".to_string(),
    };
    let tp_disp = tp.to_string_lossy().into_owned();
    let at_s = at.to_string();
    let from_s = from.to_string();
    let prompt = prompt::substitute(
        &template,
        &[
            ("TRANSCRIPT", tp_disp.as_str()),
            ("SESSION", sid),
            ("DB", cfg.db.as_str()),
            ("CTX", ctx),
            ("TURNS", at_s.as_str()),
            ("FROM_TURN", from_s.as_str()),
            ("WHY", why),
            ("LINEAGE", lineage.as_str()),
            ("ARCHIVIST", "archivist"),
            ("NOTIFY", "mail.sh"),
            ("WIKI", wiki_text.as_str()),
        ],
    );

    let logf = Path::new(&cfg.run).join(format!("archivist-{sid}.log"));
    log(&format!("archivist: {sid} — {why} (ctx {ctx}, turn {at}) -> {}", logf.display()));

    let (sysfile_contents, taskfile_contents) = prompt::system_prompt_split("", &prompt);
    let sysfile = Path::new(&cfg.run).join(format!("archivist-{sid}.system.md"));
    let taskfile = Path::new(&cfg.run).join(format!("archivist-{sid}.task.md"));
    let _ = std::fs::write(&sysfile, &sysfile_contents);
    let _ = std::fs::write(&taskfile, &taskfile_contents);

    let spec = AgentSpec {
        agent_bin: &cfg.agent,
        timeout_secs: effective_timeout,
        system_flag: "--system-prompt-file",
        sysfile: &sysfile,
        model: &cfg.model,
        wiki_dir: cfg.wiki.as_deref(),
        task_stdin: &taskfile_contents,
        logfile: &logf,
        cwd: &arc.join("cwd"),
        mail_from: "Archivist <archivist@spira>",
    };
    let rc = seam.run_agent(&spec);

    let items: u64 = state::read_state_key(arc, sid, "items_filed").and_then(|s| s.parse().ok()).unwrap_or(0);

    if rc == 0 {
        let _ = state::write_state(arc, sid, State::Safe, at, items);
        let _ = set_covered(arc, sid, at);
        let _ = std::fs::remove_file(&tc_file);
        log(&format!("archivist: {sid} safe to clear — {items} item(s) filed"));
        let _ = digest_send(seam, arc, &cfg.tz);
        0
    } else if rc == 124 {
        let budget = cfg.timeout_retries;
        tc += 1;
        if tc >= budget {
            let _ = state::write_state(arc, sid, State::Failed, at, items);
            log(&format!("archivist: {sid} FAILED (timeout budget {budget} exhausted) — see {}", logf.display()));
        } else {
            let _ = std::fs::write(&tc_file, tc.to_string());
            let _ = state::write_state(arc, sid, State::Timeout, at, items);
            log(&format!("archivist: {sid} timed out (attempt {tc}/{budget}, timeout was {effective_timeout}s) — will retry"));
        }
        rc
    } else if let Some(reset_at) = seam.capacity_reset_at(&logf) {
        seam.capacity_pause_set(reset_at, &format!("archivist/{sid}"));
        let _ = state::write_state(arc, sid, State::Capacity, at, items);
        log(&format!("archivist: {sid} deferred — the account refused the session, retrying when the window reopens"));
        ARC_RC_CAPACITY
    } else {
        let _ = state::write_state(arc, sid, State::Failed, at, items);
        log(&format!("archivist: {sid} FAILED rc={rc} after {items} item(s) — see {}", logf.display()));
        rc
    }
}

fn build_candidates(seam: &dyn Seam, cfg: &Env, arc: &Path, now: i64) -> Vec<Candidate> {
    let projects = Path::new(&cfg.token_projects);
    let run_dir = Path::new(&cfg.run);
    let live = transcripts::live_transcripts(projects, run_dir, cfg.idle, now);
    let mut out = Vec::new();
    for (sid, tp) in live {
        let env = seam.ctx_meter_env(&tp);
        let ctx_raw = env.get("SP_CTX_NOW").cloned().unwrap_or_default();
        let turns_raw = env.get("SP_CTX_TURNS").cloned().unwrap_or_default();
        let next = env.get("SP_CTX_NEXT").cloned().unwrap_or_default();
        let band = state::crossed(&next);
        if band < 0 {
            continue; // the meter could not read it
        }
        let cov = numeric_logged(covered_raw(arc, &sid).as_deref(), &format!("session {sid} cursor"));
        let turns_n = numeric_logged(Some(&turns_raw), &format!("session {sid} turns"));
        let drift = turns_n as i64 - cov as i64;
        let prev_state = state::read_state_key(arc, &sid, "state");
        let would = candidates::would_archive(drift, cfg.every, prev_state.as_deref());
        let ctx_display = if ctx_raw.is_empty() { "-".to_string() } else { ctx_raw.clone() };
        let turns_display = if turns_raw.is_empty() { "-".to_string() } else { turns_raw.clone() };
        let ctx = if ctx_raw.is_empty() { "0".to_string() } else { ctx_raw };
        out.push(Candidate { sid, tp: tp.to_string_lossy().into_owned(), ctx, turns: turns_n, ctx_display, turns_display, next, band, drift, would_archive: would, prev_state });
    }
    out
}

/// `archivist list` — the table the sweep would act on, without acting. Shows a meter
/// that could not be read as `-`, never as `0` — a `0` reads as "measured, and empty".
pub fn list(seam: &dyn Seam, cfg: &Env, arc: &Path) -> String {
    let mut out = format!("{:<40} {:>10} {:>6} {:>8} {:>5} {}\n", "SESSION", "CONTEXT", "TURNS", "BAND", "DRIFT", "WOULD");
    for c in build_candidates(seam, cfg, arc, now_epoch()) {
        out.push_str(&format!("{:<40} {:>10} {:>6} {:>8} {:>5} {}\n", c.sid, c.ctx_display, c.turns_display, c.band, c.drift, if c.would_archive { "archive" } else { "hold" }));
    }
    out
}

/// `archivist sweep` — honours the capacity pause, archives the most-drifted candidates
/// up to the per-pass budget, and forgets bookkeeping for sessions whose transcript is
/// gone. Returns the log lines it printed, for a caller that wants them (e.g. a test).
pub fn sweep(seam: &dyn Seam, cfg: &Env, arc: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(left) = seam.capacity_paused() {
        lines.push(log(&format!("archivist: skipped — account capacity paused for another {left}s")));
        let _ = std::fs::create_dir_all(arc);
        let _ = std::fs::write(arc.join("sweep.state"), format!("sweep_state=skipped\nreason=capacity\nat={}\n", now_epoch()));
        return lines;
    }

    let mut cands = build_candidates(seam, cfg, arc, now_epoch());
    candidates::sort_by_drift_desc(&mut cands);

    let mut archived = 0u32;
    for c in &cands {
        if !c.would_archive {
            continue;
        }
        if archived >= cfg.per_pass {
            lines.push(log(&format!("archivist: {} deferred — budget of {} reached (drift {})", c.sid, cfg.per_pass, c.drift)));
            continue;
        }
        let why = format!("turns since last sweep ({}) >= {}", c.drift, cfg.every);
        let rc = archive(seam, cfg, arc, &c.sid, Path::new(&c.tp), c.turns, &c.ctx, &why, false);
        if rc == ARC_RC_CAPACITY {
            lines.push(log("archivist: sweep stopped — the account's window is shut; the rest of this pass is deferred"));
            break;
        }
        if rc == 0 {
            archived += 1;
        }
    }

    let _ = std::fs::remove_file(arc.join("sweep.state"));
    forget_stale_sessions(cfg, arc, &mut lines);
    lines
}

fn forget_stale_sessions(cfg: &Env, arc: &Path, lines: &mut Vec<String>) {
    let projects = Path::new(&cfg.token_projects);
    let Ok(rd) = std::fs::read_dir(arc) else { return };
    let mut sessions: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().map(|x| x == "state").unwrap_or(false)).then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))?
        })
        .filter(|s| s != "sweep")
        .collect();
    sessions.sort();
    sessions.dedup();
    for sid in sessions {
        if transcript_exists(projects, &sid) {
            continue;
        }
        for suffix in [".state", ".hwm", ".covered", ".notified", ".lock", ".timeout_count"] {
            let _ = std::fs::remove_file(arc.join(format!("{sid}{suffix}")));
        }
        let _ = std::fs::remove_file(Path::new(&cfg.run).join(format!("archivist-{sid}.log")));
        lines.push(log(&format!("archivist: forgot {sid} — its transcript is gone")));
    }
}

fn transcript_exists(projects: &Path, sid: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(projects) else { return false };
    for dir in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        if dir.join(format!("{sid}.jsonl")).is_file() {
            return true;
        }
    }
    false
}

/// `archivist now [<session-or-path>]` — the manual "hibernate" path.
pub fn now_cmd(seam: &dyn Seam, cfg: &Env, arc: &Path, arg: Option<&str>) -> i32 {
    let projects = Path::new(&cfg.token_projects);
    let (sid, tp): (String, PathBuf) = match arg {
        Some(a) if a.contains('/') => {
            let tp = PathBuf::from(a);
            let sid = tp.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            (sid, tp)
        }
        Some(a) => {
            let live = transcripts::live_transcripts(projects, Path::new(&cfg.run), cfg.idle, now_epoch());
            match live.into_iter().find(|(s, _)| s == a) {
                Some((s, p)) => (s, p),
                None => {
                    eprintln!("archivist: no transcript for '{a}'");
                    return 1;
                }
            }
        }
        None => match transcripts::newest_transcript(projects) {
            Some(x) => x,
            None => {
                eprintln!("archivist: no transcript for 'the newest session'");
                return 1;
            }
        },
    };
    if !tp.is_file() {
        eprintln!("archivist: no transcript for '{}'", arg.unwrap_or("the newest session"));
        return 1;
    }
    let env = seam.ctx_meter_env(&tp);
    let ctx = numeric_logged(env.get("SP_CTX_NOW").map(String::as_str), &format!("session {sid} ctx")).to_string();
    let turns = numeric_logged(env.get("SP_CTX_TURNS").map(String::as_str), &format!("session {sid} turns"));
    archive(seam, cfg, arc, &sid, &tp, turns, &ctx, "asked for by hand", true)
}

/// `archivist mark <session> <state> [<items>]`.
pub fn mark(arc: &Path, sid: &str, st: &str, items_arg: Option<&str>) -> Result<(), String> {
    let state = State::parse(st).ok_or_else(|| format!("unknown archivist state '{st}'"))?;
    let at: u64 = state::read_state_key(arc, sid, "at_turn")
        .ok_or_else(|| format!("no archivist run in progress for {sid} — nothing to mark"))?
        .parse()
        .unwrap_or(0);
    let items: u64 = items_arg
        .map(str::to_string)
        .or_else(|| state::read_state_key(arc, sid, "items_filed"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    state::write_state(arc, sid, state, at, items).map_err(|e| e.to_string())
}

/// `archivist state [<session>]`.
pub fn state_cmd(arc: &Path, sid: Option<&str>) -> String {
    if let Some(s) = sid {
        return std::fs::read_to_string(arc.join(format!("{s}.state"))).unwrap_or_else(|_| "state=none\n".to_string());
    }
    let Ok(rd) = std::fs::read_dir(arc) else {
        return "no session has been archived\n".to_string();
    };
    let mut sessions: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().map(|x| x == "state").unwrap_or(false)).then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))?
        })
        .collect();
    if sessions.is_empty() {
        return "no session has been archived\n".to_string();
    }
    sessions.sort();
    let mut out = String::new();
    for s in sessions {
        if s == "sweep" {
            continue;
        }
        let state = state::read_state_key(arc, &s, "state").unwrap_or_default();
        let at = state::read_state_key(arc, &s, "at_turn").unwrap_or_default();
        let items = state::read_state_key(arc, &s, "items_filed").unwrap_or_default();
        out.push_str(&format!("{s}\t{state}\t{at} turn\t{items} filed\n"));
    }
    out
}

/// `archivist record <line>` — queue one finding's location for the next digest.
pub fn digest_record(arc: &Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(arc)?;
    let _g = lock::wait_lock(&arc.join("digest.lock"));
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(arc.join("digest.pending"))?;
    use std::io::Write;
    writeln!(f, "{line}")
}

/// `archivist digest-send` — mail today's digest at most once a day, then empty the
/// queue. Every finding's durable home is a bead, a note or a wiki page; this index is
/// disposable — deleting it loses nothing.
pub fn digest_send(seam: &dyn Seam, arc: &Path, tz: &str) -> Result<(), String> {
    std::fs::create_dir_all(arc).map_err(|e| e.to_string())?;
    let _g = lock::wait_lock(&arc.join("digest.lock"));
    let pending_path = arc.join("digest.pending");
    let pending = std::fs::read_to_string(&pending_path).unwrap_or_default();
    if pending.trim().is_empty() {
        log("archivist: digest — nothing pending");
        return Ok(());
    }
    let today = seam.today(tz);
    let sent_path = arc.join("digest.sent");
    let sent_on = std::fs::read_to_string(&sent_path).unwrap_or_default().trim().to_string();
    let n = pending.lines().filter(|l| !l.is_empty()).count();
    if sent_on == today {
        log(&format!("archivist: digest — already sent today ({today}); {n} item(s) held for the next one"));
        return Ok(());
    }
    let body = format!("## Note\nRecorded today, and where:\n\n{pending}");
    match seam.mail_send_digest(&format!("Archivist digest: {n} item(s) recorded today"), &body) {
        Ok(true) => {
            let _ = std::fs::write(&sent_path, format!("{today}\n"));
            let _ = std::fs::write(&pending_path, "");
            log(&format!("archivist: digest sent — {n} item(s)"));
            Ok(())
        }
        Ok(false) | Err(_) => {
            log(&format!("archivist: digest send failed — {n} item(s) left pending"));
            Err("digest send failed".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seam::fake::FakeSeam;

    fn cfg() -> Env {
        crate::config::resolve(&std::collections::HashMap::new())
    }

    #[test]
    fn sweep_skips_entirely_while_capacity_is_paused() {
        let dir = testkit::TempDir::new("archivist-run");
        let seam = FakeSeam::new();
        *seam.paused.borrow_mut() = Some(120);
        let lines = sweep(&seam, &cfg(), &dir);
        assert!(lines.iter().any(|l| l.contains("capacity paused for another 120s")));
        assert!(dir.join("sweep.state").is_file());
        let text = std::fs::read_to_string(dir.join("sweep.state")).unwrap();
        assert!(text.contains("reason=capacity"));
    }

    #[test]
    fn mark_requires_a_run_in_progress() {
        let dir = testkit::TempDir::new("archivist-run");
        let err = mark(&dir, "sess-1", "archiving", None).unwrap_err();
        assert!(err.contains("nothing to mark"));
    }

    #[test]
    fn mark_rejects_an_unknown_state() {
        let dir = testkit::TempDir::new("archivist-run");
        state::write_state(&dir, "sess-1", State::Sweeping, 5, 0).unwrap();
        let err = mark(&dir, "sess-1", "bogus", None).unwrap_err();
        assert!(err.contains("unknown archivist state"));
    }

    #[test]
    fn mark_updates_state_and_items_keeping_at_turn() {
        let dir = testkit::TempDir::new("archivist-run");
        state::write_state(&dir, "sess-1", State::Sweeping, 5, 0).unwrap();
        mark(&dir, "sess-1", "archiving", Some("3")).unwrap();
        assert_eq!(state::read_state_key(&dir, "sess-1", "state"), Some("archiving".into()));
        assert_eq!(state::read_state_key(&dir, "sess-1", "at_turn"), Some("5".into()));
        assert_eq!(state::read_state_key(&dir, "sess-1", "items_filed"), Some("3".into()));
    }

    #[test]
    fn state_cmd_with_no_sessions_says_so() {
        let dir = testkit::TempDir::new("archivist-run");
        assert_eq!(state_cmd(&dir, None), "no session has been archived\n");
    }

    #[test]
    fn state_cmd_lists_every_session_but_the_sweep_pseudo_session() {
        let dir = testkit::TempDir::new("archivist-run");
        state::write_state(&dir, "sess-1", State::Safe, 10, 2).unwrap();
        std::fs::write(dir.join("sweep.state"), "sweep_state=skipped\n").unwrap();
        let out = state_cmd(&dir, None);
        assert!(out.contains("sess-1\tsafe\t10 turn\t2 filed\n"));
        assert!(!out.contains("sweep\t"));
    }

    #[test]
    fn digest_record_appends_a_line() {
        let dir = testkit::TempDir::new("archivist-run");
        digest_record(&dir, "sp-1: a finding").unwrap();
        digest_record(&dir, "sp-2: another").unwrap();
        let text = std::fs::read_to_string(dir.join("digest.pending")).unwrap();
        assert_eq!(text, "sp-1: a finding\nsp-2: another\n");
    }

    #[test]
    fn digest_send_with_nothing_pending_does_not_mail() {
        let dir = testkit::TempDir::new("archivist-run");
        let seam = FakeSeam::new();
        digest_send(&seam, &dir, "UTC").unwrap();
        assert!(seam.digests_sent.borrow().is_empty());
    }

    #[test]
    fn digest_send_mails_once_and_then_holds_for_the_same_day() {
        let dir = testkit::TempDir::new("archivist-run");
        digest_record(&dir, "sp-1: found something").unwrap();
        let seam = FakeSeam::new();
        *seam.today_value.borrow_mut() = "2026-09-30".to_string();
        digest_send(&seam, &dir, "UTC").unwrap();
        assert_eq!(seam.digests_sent.borrow().len(), 1);
        assert!(std::fs::read_to_string(dir.join("digest.pending")).unwrap().is_empty());

        // a second finding arrives the same day: not sent again, held instead.
        digest_record(&dir, "sp-2: found another thing").unwrap();
        digest_send(&seam, &dir, "UTC").unwrap();
        assert_eq!(seam.digests_sent.borrow().len(), 1, "same-day digest must not be sent twice");
    }

    #[test]
    fn digest_send_on_a_new_day_sends_again() {
        let dir = testkit::TempDir::new("archivist-run");
        digest_record(&dir, "sp-1: day one").unwrap();
        let seam = FakeSeam::new();
        *seam.today_value.borrow_mut() = "2026-09-30".to_string();
        digest_send(&seam, &dir, "UTC").unwrap();

        digest_record(&dir, "sp-2: day two").unwrap();
        *seam.today_value.borrow_mut() = "2026-10-01".to_string();
        digest_send(&seam, &dir, "UTC").unwrap();
        assert_eq!(seam.digests_sent.borrow().len(), 2);
    }

    #[test]
    fn archive_refuses_a_second_concurrent_run_of_the_same_session() {
        let dir = testkit::TempDir::new("archivist-run");
        let chamber = testkit::TempDir::new("archivist-chamber");
        std::fs::write(chamber.join("archivist.md"), "prompt {{SESSION}}").unwrap();
        let mut c = cfg();
        c.chamber = chamber.path().to_string_lossy().into_owned();
        c.run = dir.path().to_string_lossy().into_owned();
        let seam = FakeSeam::new();

        // Hold the per-session lock ourselves, simulating another archive in flight.
        let held = lock::try_lock(&dir.join("sess-1.lock"));
        assert!(matches!(held, Acquire::Held(_)));
        let rc = archive(&seam, &c, &dir, "sess-1", Path::new("/tmp/sess-1.jsonl"), 10, "5", "test", false);
        assert_eq!(rc, 75);
        assert!(seam.agent_calls.borrow().is_empty(), "the agent must never run while the session lock is held elsewhere");
    }

    #[test]
    fn archive_writes_safe_state_and_advances_the_cursor_on_success() {
        let dir = testkit::TempDir::new("archivist-run");
        let chamber = testkit::TempDir::new("archivist-chamber");
        std::fs::write(chamber.join("archivist.md"), "system\n<!-- task -->\ntask for {{SESSION}}").unwrap();
        let mut c = cfg();
        c.chamber = chamber.path().to_string_lossy().into_owned();
        c.run = dir.path().to_string_lossy().into_owned();
        let seam = FakeSeam::new();
        *seam.agent_rc.borrow_mut() = 0;

        let rc = archive(&seam, &c, &dir, "sess-1", Path::new("/tmp/sess-1.jsonl"), 10, "5", "test run", false);
        assert_eq!(rc, 0);
        assert_eq!(state::read_state_key(&dir, "sess-1", "state"), Some("safe".into()));
        assert_eq!(covered_raw(&dir, "sess-1"), Some("10".to_string()));
        assert_eq!(seam.agent_calls.borrow().len(), 1);
        assert!(seam.agent_calls.borrow()[0].contains("task for sess-1"));
    }

    #[test]
    fn archive_on_timeout_retries_until_the_budget_then_fails() {
        let dir = testkit::TempDir::new("archivist-run");
        let chamber = testkit::TempDir::new("archivist-chamber");
        std::fs::write(chamber.join("archivist.md"), "task").unwrap();
        let mut c = cfg();
        c.chamber = chamber.path().to_string_lossy().into_owned();
        c.run = dir.path().to_string_lossy().into_owned();
        c.timeout_retries = 2;
        let seam = FakeSeam::new();
        *seam.agent_rc.borrow_mut() = 124;

        let rc1 = archive(&seam, &c, &dir, "sess-1", Path::new("/tmp/s.jsonl"), 10, "5", "t", false);
        assert_eq!(rc1, 124);
        assert_eq!(state::read_state_key(&dir, "sess-1", "state"), Some("timeout".into()));

        let rc2 = archive(&seam, &c, &dir, "sess-1", Path::new("/tmp/s.jsonl"), 10, "5", "t", false);
        assert_eq!(rc2, 124);
        assert_eq!(state::read_state_key(&dir, "sess-1", "state"), Some("failed".into()), "budget of 2 exhausted on the second timeout");
    }

    #[test]
    fn archive_on_capacity_refusal_pauses_and_returns_the_capacity_code() {
        let dir = testkit::TempDir::new("archivist-run");
        let chamber = testkit::TempDir::new("archivist-chamber");
        std::fs::write(chamber.join("archivist.md"), "task").unwrap();
        let mut c = cfg();
        c.chamber = chamber.path().to_string_lossy().into_owned();
        c.run = dir.path().to_string_lossy().into_owned();
        let seam = FakeSeam::new();
        *seam.agent_rc.borrow_mut() = 1;
        *seam.reset_at.borrow_mut() = Some(999_999);

        let rc = archive(&seam, &c, &dir, "sess-1", Path::new("/tmp/s.jsonl"), 10, "5", "t", false);
        assert_eq!(rc, ARC_RC_CAPACITY);
        assert_eq!(seam.pause_calls.borrow()[0], (999_999, "archivist/sess-1".to_string()));
        assert_eq!(state::read_state_key(&dir, "sess-1", "state"), Some("capacity".into()));
    }

    #[test]
    fn sweep_defers_past_the_per_pass_budget() {
        let dir = testkit::TempDir::new("archivist-run");
        let projects = testkit::TempDir::new("archivist-projects");
        std::fs::create_dir_all(projects.join("p")).unwrap();
        std::fs::write(projects.join("p").join("sess-a.jsonl"), "x").unwrap();
        std::fs::write(projects.join("p").join("sess-b.jsonl"), "x").unwrap();
        let chamber = testkit::TempDir::new("archivist-chamber");
        std::fs::write(chamber.join("archivist.md"), "task").unwrap();

        let mut c = cfg();
        c.chamber = chamber.path().to_string_lossy().into_owned();
        c.run = dir.path().to_string_lossy().into_owned();
        c.token_projects = projects.path().to_string_lossy().into_owned();
        c.per_pass = 1;
        c.every = 1;

        let seam = FakeSeam::new();
        *seam.agent_rc.borrow_mut() = 0;
        let a_path = projects.join("p").join("sess-a.jsonl").to_string_lossy().into_owned();
        let b_path = projects.join("p").join("sess-b.jsonl").to_string_lossy().into_owned();
        let mut env_a = std::collections::HashMap::new();
        env_a.insert("SP_CTX_NOW".into(), "10".into());
        env_a.insert("SP_CTX_TURNS".into(), "50".into());
        env_a.insert("SP_CTX_NEXT".into(), "warn".into());
        seam.ctx_env.borrow_mut().insert(a_path, env_a.clone());
        let mut env_b = env_a.clone();
        env_b.insert("SP_CTX_TURNS".into(), "5".into());
        seam.ctx_env.borrow_mut().insert(b_path, env_b);

        let lines = sweep(&seam, &c, &dir);
        // a has more drift (50) than b (5), so a is archived and b is deferred.
        assert_eq!(seam.agent_calls.borrow().len(), 1);
        assert!(lines.iter().any(|l| l.contains("sess-b deferred")));
    }

    #[test]
    fn list_shows_a_dash_for_an_unreadable_meter_never_a_zero() {
        let dir = testkit::TempDir::new("archivist-run");
        let projects = testkit::TempDir::new("archivist-projects");
        std::fs::create_dir_all(projects.join("p")).unwrap();
        std::fs::write(projects.join("p").join("sess-a.jsonl"), "x").unwrap();

        let mut c = cfg();
        c.run = dir.path().to_string_lossy().into_owned();
        c.token_projects = projects.path().to_string_lossy().into_owned();

        let seam = FakeSeam::new();
        let a_path = projects.join("p").join("sess-a.jsonl").to_string_lossy().into_owned();
        // The meter answered SP_CTX_NEXT (so the row is not dropped as unreadable) but
        // left SP_CTX_NOW/SP_CTX_TURNS blank.
        let mut env_a = std::collections::HashMap::new();
        env_a.insert("SP_CTX_NEXT".into(), "warn".into());
        seam.ctx_env.borrow_mut().insert(a_path, env_a);

        let out = list(&seam, &c, &dir);
        let row = out.lines().find(|l| l.starts_with("sess-a")).expect("sess-a row");
        let fields: Vec<&str> = row.split_whitespace().collect();
        // SESSION CONTEXT TURNS BAND DRIFT WOULD — band and drift are legitimately 0
        // here; CONTEXT and TURNS are the unreadable ones and must be "-", never "0".
        assert_eq!(fields[1], "-", "context column: {row}");
        assert_eq!(fields[2], "-", "turns column: {row}");
    }
}
