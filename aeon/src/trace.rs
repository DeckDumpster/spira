//! Trace / heartbeat (family P), groom claims and bead-named paths (family Q's live pair)
//! and the aeon-session tsd row (family AC's aeon half) — wave 4.34. These were lib.sh
//! functions reached through the bash seam by aeon itself, and (for the trace readers)
//! by cockpit-collect and sentinel as well, which now call this module in-process instead.
//!
//! Every port here keeps its string contract byte for byte: a `?`, a `-`, a `gate`, or a
//! `KEY=value` line means exactly what the bash version meant, because the other side of
//! each of these (a cockpit pane, a heartbeat decision) was never touched.
//!
//! `aeon_fuse_minutes` and `bead_named_paths` take their git-derived inputs already
//! resolved by the caller (a commit timestamp, a `git ls-files` listing) rather than a
//! `Git` port, so this module has no process-spawning dependency and the same function
//! serves aeon, cockpit-collect and sentinel without each reimplementing a git adapter.

use std::collections::BTreeSet;
use std::path::Path;

use crate::ledger::{self, SessionFields};
use crate::ports::Exec;

// ---- shared trace rendering -------------------------------------------------------------

fn allow_char(c: char) -> bool {
    c == ' ' || c.is_ascii_alphanumeric() || "._/:,()#+-".contains(c)
}

/// SAFE FOR A KEY=value FILE, AT THE SOURCE. `s` is arbitrary text an agent produced — a
/// shell command, a code fragment, a sentence — and the cockpit snapshot is a KEY=value
/// file sourced by the pane. A newline would inject extra lines and an `=` would make a
/// bogus key. An allowlist, not a blocklist, the same one trace_last/trace_stats always
/// applied in bash.
fn clean(s: &str, n: usize) -> String {
    let scrubbed: String = s.chars().map(|c| if allow_char(c) { c } else { ' ' }).collect();
    let collapsed = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(n).collect()
}

const EDITS: [&str; 3] = ["Edit", "Write", "NotebookEdit"];

fn obj_str<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}

/// One pass over a trace segment, gathering everything trace_last/trace_stats ever asked
/// for in bash — one read, one pass, matching the original's own "ONE READ, ONE PASS, ONE
/// FORK" constraint (the collector calls this once per aeon per pass).
#[derive(Default)]
struct Scan {
    model: Option<String>,
    seen_assistant: bool,
    turn_ids: BTreeSet<String>,
    ctx: Option<i64>,
    tools: i64,
    files: BTreeSet<String>,
    /// The last thing the session did: a tool call (name + arg) or a non-empty text block,
    /// whichever came later in the stream — trace_last's own "last" variable.
    act: String,
    /// The last non-empty assistant TEXT block specifically (trace_stats' SAID).
    said: String,
}

fn scan(seg: &str) -> Scan {
    let mut s = Scan::default();
    for line in seg.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        // The init event carries the model once, at the top of the attempt — checked on
        // every line, before the assistant-only filter below, the same order bash used.
        if e.get("type").and_then(|t| t.as_str()) == Some("system") && e.get("subtype").and_then(|t| t.as_str()) == Some("init") {
            let m = obj_str(&e, "model").or_else(|| e.get("message").and_then(|m| obj_str(m, "model")));
            if let Some(m) = m {
                s.model = Some(m.to_string());
            }
        }
        if e.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        s.seen_assistant = true;
        let msg = e.get("message").cloned().unwrap_or_default();
        if let Some(id) = msg.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                s.turn_ids.insert(id.to_string());
            }
        }
        if let Some(u) = msg.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| u.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            s.ctx = Some((n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens")) as i64);
        }
        let content: Vec<serde_json::Value> = msg.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        for c in &content {
            match c.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    s.tools += 1;
                    let input = c.get("input").cloned().unwrap_or_default();
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    if EDITS.contains(&name) {
                        if let Some(fp) = obj_str(&input, "file_path").or_else(|| obj_str(&input, "notebook_path")) {
                            s.files.insert(fp.to_string());
                        }
                    }
                    let arg = obj_str(&input, "command").or_else(|| obj_str(&input, "file_path")).unwrap_or("");
                    let arg: String = arg.chars().take(200).collect();
                    s.act = format!("{name} {arg}");
                }
                Some("text") => {
                    if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                        let t = t.trim();
                        if !t.is_empty() {
                            let t: String = t.replace('\n', " ").chars().take(200).collect();
                            s.said = t.clone();
                            s.act = t;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    s
}

/// `trace_last <logfile>` -> the last tool call or text the session produced, sanitized
/// and clamped to 96 chars, the same allowlist/length `trace_stats`' ACT applies.
pub fn trace_last(path: &Path, mark: &str) -> String {
    let seg = ledger::attempt_trace(path, 100_000, mark);
    let seg = String::from_utf8_lossy(&seg);
    clean(&scan(&seg).act, 96)
}

/// `trace_stats <logfile>` -> `KEY=value` lines describing the session writing it (TURNS,
/// CTX, TOOLS, FILES, QUIET, ACT, SAID, MODEL). An unreadable file renders every field `?`;
/// a readable file with no assistant event yet renders the event fields `-`; a real reading
/// renders a number or string (law-absence-needs-a-positive-control: `?` and `-` must never
/// collapse into each other or into 0).
pub fn trace_stats(path: &Path, mark: &str, now: i64) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return unreadable_stats();
    };
    if std::fs::File::open(path).is_err() {
        return unreadable_stats();
    }
    let mtime = {
        use std::os::unix::fs::MetadataExt;
        meta.mtime()
    };
    let seg = ledger::attempt_trace(path, 0, mark);
    let seg = String::from_utf8_lossy(&seg);
    let s = scan(&seg);
    let mut out = String::new();
    out.push_str(&format!("QUIET={}\n", now - mtime));
    out.push_str(&format!("MODEL={}\n", s.model.map(|m| clean(&m, 32)).filter(|m| !m.is_empty()).unwrap_or_else(|| "-".to_string())));
    if !s.seen_assistant {
        for k in ["TURNS", "CTX", "TOOLS", "FILES", "ACT", "SAID"] {
            out.push_str(&format!("{k}=-\n"));
        }
    } else {
        out.push_str(&format!("TURNS={}\n", s.turn_ids.len()));
        out.push_str(&format!("CTX={}\n", s.ctx.map(|c| c.to_string()).unwrap_or_else(|| "-".to_string())));
        out.push_str(&format!("TOOLS={}\n", s.tools));
        out.push_str(&format!("FILES={}\n", s.files.len()));
        let act = clean(&s.act, 96);
        out.push_str(&format!("ACT={}\n", if act.is_empty() { "-".to_string() } else { act }));
        let said = clean(&s.said, 96);
        out.push_str(&format!("SAID={}\n", if said.is_empty() { "-".to_string() } else { said }));
    }
    out
}

fn unreadable_stats() -> String {
    "TURNS=?\nCTX=?\nTOOLS=?\nFILES=?\nQUIET=?\nACT=?\nSAID=?\nMODEL=?\n".to_string()
}

/// `trace_tail <logfile> [n]` -> the last `n` human-readable moments of a session: what
/// the agent said, ran, and got back, plus a turn boundary on every `result` event (never
/// rendered as "session ended" — the session continues on the same session_id).
pub fn trace_tail(path: &Path, mark: &str, n: usize) -> String {
    if std::fs::File::open(path).is_err() {
        return "(no session log)".to_string();
    }
    let seg = ledger::attempt_trace(path, 200_000, mark);
    let seg = String::from_utf8_lossy(&seg);
    let mut out: Vec<String> = Vec::new();
    let mut turn_n = 0i64;
    for line in seg.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match e.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let content: Vec<serde_json::Value> = e.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
                for c in &content {
                    match c.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                let t = t.trim();
                                if !t.is_empty() {
                                    let t: String = t.replace('\n', " ").chars().take(200).collect();
                                    out.push(format!("  {t}"));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let input = c.get("input").cloned().unwrap_or_default();
                            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let arg = obj_str(&input, "command").or_else(|| obj_str(&input, "file_path")).or_else(|| obj_str(&input, "pattern")).unwrap_or("");
                            let arg: String = arg.chars().take(150).collect();
                            out.push(format!("  $ {name} {arg}"));
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let content: Vec<serde_json::Value> = e.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
                for c in &content {
                    if c.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let body = match c.get("content") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Array(a)) => a
                            .iter()
                            .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join(" "),
                        Some(v) => v.to_string(),
                        None => String::new(),
                    };
                    let body = body.trim().replace('\n', " ");
                    if !body.is_empty() {
                        let body: String = body.chars().take(160).collect();
                        out.push(format!("    -> {body}"));
                    }
                }
            }
            Some("result") => {
                turn_n += 1;
                let subtype = e.get("subtype").and_then(|t| t.as_str()).unwrap_or("?");
                out.push(format!("  [turn {turn_n}: {subtype}]"));
            }
            _ => {}
        }
    }
    if out.is_empty() {
        return "(trace had no readable events)".to_string();
    }
    let start = out.len().saturating_sub(n);
    out[start..].join("\n")
}

/// `wiki_write_paths <logfile> <wiki-dir>` -> paths under `wiki` (one per line in the
/// caller's output, here a sorted, deduplicated list), named by Edit/Write/NotebookEdit
/// tool calls in the CURRENT attempt of `logfile`. THE TRANSCRIPT, NOT A DIRTY SNAPSHOT: a
/// snapshot taken at session start cannot tell this session's own write from a concurrent
/// actor's — both are just "dirty now, clean at the watermark" (sp-4fl2e).
pub fn wiki_write_paths(path: &Path, wiki: &Path, mark: &str) -> Vec<String> {
    if wiki.as_os_str().is_empty() {
        return Vec::new();
    }
    let seg = ledger::attempt_trace(path, 0, mark);
    let seg = String::from_utf8_lossy(&seg);
    let wiki_real = realish(wiki);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in seg.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if e.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let content: Vec<serde_json::Value> = e.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
        for c in &content {
            if c.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !EDITS.contains(&name) {
                continue;
            }
            let input = c.get("input").cloned().unwrap_or_default();
            let Some(fp) = obj_str(&input, "file_path").or_else(|| obj_str(&input, "notebook_path")) else { continue };
            if fp.is_empty() {
                continue;
            }
            let rp = realish(Path::new(fp));
            if rp == wiki_real {
                continue;
            }
            if let Ok(rel) = rp.strip_prefix(&wiki_real) {
                seen.insert(rel.to_string_lossy().into_owned());
            }
        }
    }
    seen.into_iter().collect()
}

fn realish(p: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `wiki_commit_paths <write-paths> <dirty-paths>` -> the subset of `writes` (in `writes`
/// order) that is also in `dirty` and never `wiki/tasks.md` — the generated view regenerated
/// by brain's own SessionStart hook and never authored by an aeon.
pub fn wiki_commit_paths(writes: &[String], dirty: &[String]) -> Vec<String> {
    let dirty: BTreeSet<&str> = dirty.iter().map(String::as_str).collect();
    writes.iter().filter(|w| !w.is_empty() && w.as_str() != "wiki/tasks.md" && dirty.contains(w.as_str())).cloned().collect()
}

// ---- groom claims (family Q) -------------------------------------------------------------

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A minimal ISO-8601 timestamp parser — `YYYY-MM-DDTHH:MM:SS[.ffffff](Z|+HH:MM|-HH:MM)` —
/// matching what `datetime.fromisoformat(ts.replace("Z", "+00:00"))` accepted in bash.
fn parse_iso(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    let mut epoch = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    let mut rest = s[19..].trim();
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    if rest == "Z" || rest.is_empty() {
        return Some(epoch);
    }
    if let Some(off) = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-')) {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = off.get(0..2)?.parse().ok()?;
        let om: i64 = off.get(3..5)?.parse().ok()?;
        epoch -= sign * (oh * 3600 + om * 60);
        return Some(epoch);
    }
    Some(epoch)
}

/// `groom_claims_verified <new-log-lines> <ask-json> <epoch>` -> "" (nothing claimed, or
/// every claim verified) | comma-space-separated ids claimed but unproven. A groom pass
/// that writes ESCALATED, inquiry or flagged for bead X without a matching ask bead (type
/// decision, created at or after `epoch`, naming X in its title or description) has not
/// escalated — it has claimed to.
pub fn groom_claims_verified(log: &str, ask_json: &str, epoch: i64) -> String {
    let claim_re = regex::Regex::new(r"(?i)ESCALATED|inquiry|flagged").unwrap();
    let id_re = regex::Regex::new(r"sp-[a-z0-9-]+").unwrap();
    let mut ids: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in log.lines() {
        if !claim_re.is_match(line) {
            continue;
        }
        for m in id_re.find_iter(line) {
            let id = m.as_str().to_string();
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return String::new();
    }
    let parsed: serde_json::Value = serde_json::from_str(ask_json).unwrap_or(serde_json::Value::Array(vec![]));
    let asks: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    let mut found: BTreeSet<String> = BTreeSet::new();
    for a in &asks {
        let Some(ca) = a.get("created_at").and_then(|v| v.as_str()) else { continue };
        let Some(ts) = parse_iso(ca) else { continue };
        if ts < epoch {
            continue;
        }
        let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let desc = a.get("description").and_then(|v| v.as_str()).unwrap_or("");
        for id in ids.iter() {
            if title.contains(id.as_str()) || desc.contains(id.as_str()) {
                found.insert(id.clone());
            }
        }
    }
    ids.into_iter().filter(|id| !found.contains(id)).collect::<Vec<_>>().join(", ")
}

/// `bead_named_paths <text> <tracked>` -> path tokens in `text` that exist as tracked files
/// in `tracked` (the raw `git ls-files` listing, one path per line — the caller is
/// responsible for the empty-when-not-a-repo case), sorted and deduplicated. A bead usually
/// names the files it is about in prose — matching a bare regex against the tracked tree,
/// rather than trusting the regex alone, is what keeps a bead id or a stray URL from being
/// read as a path.
pub fn bead_named_paths(text: &str, tracked: &str) -> Vec<String> {
    let tracked: BTreeSet<&str> = tracked.lines().collect();
    if tracked.is_empty() {
        return Vec::new();
    }
    let tok_re = regex::Regex::new(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+").unwrap();
    let mut cleaned: BTreeSet<String> = BTreeSet::new();
    for m in tok_re.find_iter(text) {
        let mut tok = m.as_str();
        while let Some(c) = tok.chars().next() {
            if c == '(' || c == '`' {
                tok = &tok[1..];
            } else {
                break;
            }
        }
        while let Some(c) = tok.chars().last() {
            if ".,;:)`".contains(c) {
                tok = &tok[..tok.len() - 1];
            } else {
                break;
            }
        }
        if !tok.is_empty() {
            cleaned.insert(tok.to_string());
        }
    }
    cleaned.into_iter().filter(|t| tracked.contains(t.as_str())).collect()
}

// ---- heartbeat / fuse (family P's other half) --------------------------------------------

fn gate_run_dirs(gate_run: &Path, bead: &str) -> Vec<std::path::PathBuf> {
    let suffix = format!("_{bead}");
    let mut v = Vec::new();
    let Ok(entries) = std::fs::read_dir(gate_run) else { return v };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().ends_with(&suffix) && e.path().is_dir() {
            v.push(e.path());
        }
    }
    v
}

fn pid_cmdline_names_gate(pid: &str) -> bool {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    cmdline_names_gate(&raw)
}

fn cmdline_names_gate(raw: &[u8]) -> bool {
    let replaced: Vec<u8> = raw.iter().map(|&b| if b == 0 { b' ' } else { b }).collect();
    String::from_utf8_lossy(&replaced).contains("gate")
}

fn live_gate_running(gate_run: &Path, bead: &str, is_gate: &dyn Fn(&str) -> bool) -> bool {
    for gd in gate_run_dirs(gate_run, bead) {
        let Ok(pid_s) = std::fs::read_to_string(gd.join("pid")) else { continue };
        let pid_s = pid_s.trim();
        if pid_s.is_empty() || !pid_s.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if is_gate(pid_s) {
            return true;
        }
    }
    false
}

/// The newest mtime (lstat, matching `find`'s default of not following symlinks) of `root`
/// itself or anything under it, excluding any path containing `/.git` as a substring — the
/// same broad exclusion `find -not -path '*/.git*'` applies (it also drops `.github`,
/// `.gitignore`, `.gitmodules`; harmless, since none of those is the deliverable either).
fn newest_mtime(root: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let mut best: Option<i64> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.to_string_lossy().contains("/.git") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        let mt = meta.mtime();
        if best.is_none_or(|b| mt > b) {
            best = Some(mt);
        }
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                for e in entries.flatten() {
                    stack.push(e.path());
                }
            }
        }
    }
    best
}

/// `aeon_fuse_minutes <bead> <worktree> <run-dir>` -> minutes since the deliverable (a
/// commit ahead of the base ref, or a file write in the worktree) last moved, `"gate"`
/// while a live gate runs for this bead, `"?"` when the worktree is absent or nothing has
/// moved yet. `commit_ahead_ts` is the caller's own `git log --format=%ct -1 base..HEAD`
/// reading (family W — base refs — is not yet ported, so resolving `base` itself still
/// goes through a seam or an `io::lib_call`, outside this function).
///
/// THE FUSE DOES NOT BURN WHILE A GATE RUNS FOR THIS BEAD, and a gate that just finished is
/// also progress: the gate-run directory's own `rc`/`out`/`started` mtimes fold into the
/// "last moved" clock the same as a commit or a file write would (sp-l99q6).
pub fn aeon_fuse_minutes(bead: &str, wt: &Path, run: &Path, commit_ahead_ts: Option<i64>, now: i64) -> String {
    aeon_fuse_minutes_with(bead, wt, run, commit_ahead_ts, now, &pid_cmdline_names_gate)
}

fn aeon_fuse_minutes_with(
    bead: &str,
    wt: &Path,
    run: &Path,
    commit_ahead_ts: Option<i64>,
    now: i64,
    is_gate: &dyn Fn(&str) -> bool,
) -> String {
    let gate_run = run.join("gate-run");
    if live_gate_running(&gate_run, bead, is_gate) {
        return "gate".to_string();
    }
    if !wt.is_dir() {
        return "?".to_string();
    }
    let mut last_t = commit_ahead_ts.unwrap_or(0);
    if let Some(mt) = newest_mtime(wt) {
        if mt > last_t {
            last_t = mt;
        }
    }
    use std::os::unix::fs::MetadataExt;
    for gd in gate_run_dirs(&gate_run, bead) {
        for name in ["rc", "out", "started"] {
            if let Ok(meta) = std::fs::metadata(gd.join(name)) {
                let gt = meta.mtime();
                if gt > last_t {
                    last_t = gt;
                }
            }
        }
    }
    if last_t > 0 {
        ((now - last_t) / 60).to_string()
    } else {
        "?".to_string()
    }
}

/// `aeon_lease_minutes <bead>` -> minutes left in the liveness lease file, or `"?"` if it
/// cannot be read — never 0 for "unreadable", which would read as an all-clear.
pub fn aeon_lease_minutes(bead: &str, run: &Path, now: i64) -> String {
    if bead.is_empty() {
        return "?".to_string();
    }
    let lease_file = run.join("aeon").join(format!("{bead}.lease"));
    let Ok(raw) = std::fs::read_to_string(&lease_file) else { return "?".to_string() };
    let raw = raw.trim();
    if raw.is_empty() {
        return "?".to_string();
    }
    let Ok(deadline) = raw.parse::<i64>() else { return "?".to_string() };
    ((deadline - now) / 60).to_string()
}

// ---- the aeon-session tsd row (family AC's aeon half) -------------------------------------

/// `_tsd_aeon_session <bead> <fayth> <rc> <status> <fields>` — appends this session's
/// aeon-session row (run/tsd/), reusing the `SessionFields` `ledger_done` already computed
/// for its own ledger line rather than re-deriving them from a rendered string. Best-effort,
/// like every tsd producer: never affects the caller's exit status.
pub fn tsd_aeon_session(exec: &dyn Exec, run_root: &str, bead: &str, fayth: &str, rc: i32, status: &str, fields: &SessionFields) {
    let q = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string());
    let args = vec![
        "--family".to_string(),
        "aeon-session".to_string(),
        "--root".to_string(),
        run_root.to_string(),
        "--field-str".to_string(),
        format!("bead={bead}"),
        "--field-str".to_string(),
        format!("fayth={fayth}"),
        "--field".to_string(),
        format!("rc={rc}"),
        "--field-str".to_string(),
        format!("status={status}"),
        "--field".to_string(),
        format!("wall_s={}", q(fields.wall_s)),
        "--field".to_string(),
        format!("api_s={}", q(fields.api_s)),
        "--field".to_string(),
        format!("turns={}", q(fields.turns)),
        "--field".to_string(),
        format!("cost_usd={}", fields.cost_usd.map(|c| format!("{c:.4}")).unwrap_or_else(|| "?".to_string())),
    ];
    let _ = exec.exec("tsd-write", &args, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("aeon-trace-{name}"))
    }

    const MARK: &str = "=== spira attempt";

    // ---- trace_last / trace_stats (test-cockpit-*.sh had no dedicated bash suite for
    // these two; this is their first direct test) ----

    #[test]
    fn trace_last_reports_the_last_tool_call_or_text() {
        let d = tmp("last");
        let p = d.join("sp-a.log");
        std::fs::write(
            &p,
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"ls -la\"}}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"content\":[{\"type\":\"text\",\"text\":\"done\\nwith it\"}]}}\n",
        )
        .unwrap();
        assert_eq!(trace_last(&p, MARK), "done with it");
        assert_eq!(trace_last(&d.join("missing"), MARK), "");
    }

    #[test]
    fn trace_last_strips_unsafe_characters() {
        let d = tmp("last-clean");
        let p = d.join("sp-a.log");
        std::fs::write(&p, "{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"a=b\\nKEY=inject\"}]}}\n").unwrap();
        let last = trace_last(&p, MARK);
        assert!(!last.contains('='), "got [{last}]");
    }

    #[test]
    fn trace_stats_unreadable_file_is_all_question_marks() {
        let d = tmp("stats-missing");
        let out = trace_stats(&d.join("missing.log"), MARK, 1000);
        assert_eq!(out, "TURNS=?\nCTX=?\nTOOLS=?\nFILES=?\nQUIET=?\nACT=?\nSAID=?\nMODEL=?\n");
    }

    #[test]
    fn trace_stats_readable_with_no_assistant_event_is_dashes_not_zero() {
        let d = tmp("stats-empty");
        let p = d.join("sp-a.log");
        std::fs::write(&p, "not json at all\n").unwrap();
        let out = trace_stats(&p, MARK, 1000);
        assert!(out.contains("TURNS=-\n"), "{out}");
        assert!(out.contains("CTX=-\n"), "{out}");
        assert!(out.contains("MODEL=-\n"), "{out}");
        assert!(out.contains("QUIET="), "{out}");
        assert!(!out.contains('?'), "a readable, event-free trace must not render ?: {out}");
    }

    #[test]
    fn trace_stats_counts_turns_tools_files_and_last_usage_wins() {
        let d = tmp("stats-full");
        let p = d.join("sp-a.log");
        std::fs::write(
            &p,
            "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-x\"}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":5},\"content\":[{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/a\"}}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":20},\"content\":[{\"type\":\"tool_use\",\"name\":\"Write\",\"input\":{\"file_path\":\"/b\"}},{\"type\":\"text\",\"text\":\"said this\"}]}}\n",
        )
        .unwrap();
        let out = trace_stats(&p, MARK, 1000);
        assert!(out.contains("TURNS=1\n"), "dedup by message id: {out}");
        assert!(out.contains("CTX=20\n"), "last usage wins: {out}");
        assert!(out.contains("TOOLS=2\n"), "{out}");
        assert!(out.contains("FILES=2\n"), "{out}");
        assert!(out.contains("MODEL=claude-x\n"), "{out}");
        assert!(out.contains("SAID=said this\n"), "{out}");
    }

    // ---- trace_tail (test-cockpit-fuse.sh Part 4) ----

    #[test]
    fn trace_tail_renders_result_as_a_turn_index_not_session_ended() {
        let d = tmp("tail");
        let p = d.join("sp-turn.log");
        std::fs::write(
            &p,
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]}}\n\
             {\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s1\",\"is_error\":false}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"/tmp/x\"}}]}}\n\
             {\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s1\",\"is_error\":false}\n",
        )
        .unwrap();
        let out = trace_tail(&p, MARK, 10);
        assert!(!out.contains("session ended"), "{out}");
        assert!(out.contains("turn 1"), "{out}");
        assert!(out.contains("turn 2"), "{out}");
    }

    #[test]
    fn trace_tail_missing_file_renders_no_session_log() {
        let d = tmp("tail-missing");
        assert_eq!(trace_tail(&d.join("missing"), MARK, 10), "(no session log)");
    }

    // ---- wiki_write_paths / wiki_commit_paths (test-aeon-wiki-dirty.sh T1) ----

    #[test]
    fn wiki_commit_paths_selects_own_dirty_writes_never_tasks_md() {
        let writes = vec!["wiki/notes/new.md".to_string()];
        let dirty = vec!["wiki/notes/new.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), vec!["wiki/notes/new.md".to_string()]);

        // a pre-existing dirty file never named by this session's writes is not selected.
        let writes = vec!["wiki/notes/new.md".to_string()];
        let dirty = vec!["wiki/notes/preexist.md".to_string(), "wiki/notes/new.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), vec!["wiki/notes/new.md".to_string()]);

        // an own write that is no longer dirty has nothing left to stage.
        let writes = vec!["wiki/notes/reverted.md".to_string()];
        let dirty = vec!["wiki/notes/other.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), Vec::<String>::new());

        // wiki/tasks.md is excluded even as an own dirty write, alone or mixed.
        let writes = vec!["wiki/tasks.md".to_string()];
        let dirty = vec!["wiki/tasks.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), Vec::<String>::new());
        let writes = vec!["wiki/tasks.md".to_string(), "wiki/notes/new.md".to_string()];
        let dirty = writes.clone();
        assert_eq!(wiki_commit_paths(&writes, &dirty), vec!["wiki/notes/new.md".to_string()]);

        // a concurrent actor's write is never selected.
        let writes = vec!["wiki/notes/mine.md".to_string()];
        let dirty = vec!["wiki/notes/concurrent-other.md".to_string(), "wiki/notes/mine.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), vec!["wiki/notes/mine.md".to_string()]);

        // multiple own writes, all dirty: every one, in writes order.
        let writes = vec!["wiki/a.md".to_string(), "wiki/b.md".to_string(), "wiki/c.md".to_string()];
        let dirty = vec!["wiki/c.md".to_string(), "wiki/a.md".to_string(), "wiki/b.md".to_string()];
        assert_eq!(wiki_commit_paths(&writes, &dirty), writes);

        // no writes at all: nothing selected.
        assert_eq!(wiki_commit_paths(&[], &["wiki/notes/concurrent-other.md".to_string()]), Vec::<String>::new());
    }

    #[test]
    fn wiki_write_paths_reads_edit_write_notebookedit_under_the_wiki_dir() {
        let d = tmp("wiki-write");
        let wiki = d.join("wiki");
        std::fs::create_dir_all(wiki.join("notes")).unwrap();
        let target = wiki.join("notes").join("sop.md");
        std::fs::write(&target, "x").unwrap();
        let outside = d.join("elsewhere.md");
        std::fs::write(&outside, "x").unwrap();
        let log = d.join("sp-a.log");
        std::fs::write(
            &log,
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"id\":\"m1\",\"content\":[{{\"type\":\"tool_use\",\"name\":\"Write\",\"input\":{{\"file_path\":\"{}\"}}}},{{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{{\"file_path\":\"{}\"}}}},{{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{{\"command\":\"ls\"}}}}]}}}}\n",
                target.display(),
                outside.display()
            ),
        )
        .unwrap();
        let got = wiki_write_paths(&log, &wiki, MARK);
        assert_eq!(got, vec!["notes/sop.md".to_string()]);
    }

    // ---- groom_claims_verified (test-groom-escalation-check.sh T1) ----

    #[test]
    fn groom_claims_verified_table() {
        let epoch = 1_900_000_000i64;
        let new_ts = "2030-03-17T17:47:40Z"; // epoch + 60
        let old_ts = "2030-03-17T16:46:40Z"; // epoch - 3600
        let claim = "2026-09-20T00:00:00Z groom: pass complete. Actions: ESCALATED sp-gc1.";
        let noclaim = "2026-09-20T00:00:00Z groom: pass complete. Actions: CLOSED sp-xx premise-gone.";
        let multi = "2026-09-20T00:00:00Z groom: Actions: ESCALATED sp-gc1. Also flagged sp-gc2 for review.";
        let ask_for = |id: &str, ts: &str, field: &str| -> String {
            let (title, desc) = if field == "description" { ("unrelated".to_string(), id.to_string()) } else { (id.to_string(), String::new()) };
            serde_json::json!([{"title": title, "description": desc, "created_at": ts}]).to_string()
        };

        assert_eq!(groom_claims_verified(noclaim, "[]", epoch), "", "no escalation keyword at all");
        assert_eq!(groom_claims_verified(claim, "[]", epoch), "sp-gc1", "a claim with no ask bead at all is unproven");
        assert_eq!(groom_claims_verified(claim, &ask_for("sp-gc1", new_ts, "title"), epoch), "", "a fresh matching ask verifies the claim");
        assert_eq!(groom_claims_verified(claim, &ask_for("sp-gc1", old_ts, "title"), epoch), "sp-gc1", "an ask created before the session epoch does not excuse the claim");
        assert_eq!(groom_claims_verified(claim, &ask_for("sp-gc1", new_ts, "description"), epoch), "", "the id may be named in the description");
        assert_eq!(groom_claims_verified(multi, &ask_for("sp-gc1", new_ts, "title"), epoch), "sp-gc2", "two claims, only one backed");
        assert_eq!(groom_claims_verified(multi, "[]", epoch), "sp-gc1, sp-gc2", "'flagged' is recognised as a claim keyword");
        assert_eq!(groom_claims_verified("", "[]", epoch), "", "an empty log");
    }

    // ---- bead_named_paths ----

    #[test]
    fn bead_named_paths_matches_only_tracked_files_sorted() {
        let tracked = "spira/lib.sh\naeon/src/run.rs\nREADME.md\n";
        let text = "See (spira/lib.sh) and aeon/src/run.rs`, also docs/missing.md and sp-abc123.";
        let got = bead_named_paths(text, tracked);
        assert_eq!(got, vec!["aeon/src/run.rs".to_string(), "spira/lib.sh".to_string()]);
    }

    #[test]
    fn bead_named_paths_empty_tracked_is_empty_output() {
        assert_eq!(bead_named_paths("spira/lib.sh", ""), Vec::<String>::new());
    }

    // ---- aeon_fuse_minutes (test-thrash.sh) ----

    fn make_worktree(wt: &Path) {
        std::fs::create_dir_all(wt).unwrap();
        std::process::Command::new("git").arg("init").arg("-q").arg(wt).status().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "t@t"]).current_dir(wt).status().unwrap();
        std::process::Command::new("git").args(["config", "user.name", "T"]).current_dir(wt).status().unwrap();
        std::fs::write(wt.join("file.txt"), "init").unwrap();
        std::process::Command::new("git").args(["add", "file.txt"]).current_dir(wt).status().unwrap();
        std::process::Command::new("git").args(["commit", "-q", "-m", "initial"]).current_dir(wt).status().unwrap();
    }

    #[test]
    fn aeon_fuse_minutes_no_worktree_is_question_mark() {
        let d = tmp("fuse-nowt");
        let run = d.join("run");
        std::fs::create_dir_all(run.join("gate-run")).unwrap();
        assert_eq!(aeon_fuse_minutes("sp-x", &d.join("worktree/sp-x"), &run, None, 1000), "?");
    }

    #[test]
    fn aeon_fuse_minutes_just_written_worktree_is_zero() {
        let d = tmp("fuse-fresh");
        let run = d.join("run");
        std::fs::create_dir_all(run.join("gate-run")).unwrap();
        let wt = d.join("worktree/sp-x");
        make_worktree(&wt);
        let now = crate::util::now_epoch();
        let fuse = aeon_fuse_minutes("sp-x", &wt, &run, None, now);
        assert_eq!(fuse, "0", "got [{fuse}]");
    }

    #[test]
    fn aeon_fuse_minutes_stale_worktree_is_a_positive_number() {
        let d = tmp("fuse-stale");
        let run = d.join("run");
        std::fs::create_dir_all(run.join("gate-run")).unwrap();
        let wt = d.join("worktree/sp-x");
        std::fs::create_dir_all(&wt).unwrap();
        let old = crate::util::now_epoch() - 90 * 60;
        std::fs::write(wt.join("stale.txt"), "x").unwrap();
        set_mtime(&wt.join("stale.txt"), old);
        set_mtime(&wt, old);
        let fuse = aeon_fuse_minutes("sp-x", &wt, &run, None, crate::util::now_epoch());
        let n: i64 = fuse.parse().unwrap();
        assert!((85..=95).contains(&n), "got {n}");
    }

    #[test]
    fn aeon_fuse_minutes_live_gate_suppresses_it_dead_gate_does_not() {
        let d = tmp("fuse-gate");
        let run = d.join("run");
        let gate_dir = run.join("gate-run").join("testname.spira_sp-x");
        std::fs::create_dir_all(&gate_dir).unwrap();
        let wt = d.join("worktree/sp-x");
        std::fs::create_dir_all(&wt).unwrap();
        let old = crate::util::now_epoch() - 60 * 60;
        std::fs::write(wt.join("old.txt"), "x").unwrap();
        set_mtime(&wt.join("old.txt"), old);
        set_mtime(&wt, old);

        // no gate: a number (positive control).
        let no_gate = aeon_fuse_minutes("sp-x", &wt, &run, None, crate::util::now_epoch());
        assert!(no_gate.parse::<i64>().is_ok(), "got [{no_gate}]");

        let alive = |pid: &str| pid == "4242";
        std::fs::write(gate_dir.join("pid"), "4242").unwrap();
        let gate_fuse = aeon_fuse_minutes_with("sp-x", &wt, &run, None, crate::util::now_epoch(), &alive);
        assert_eq!(gate_fuse, "gate");

        std::fs::write(gate_dir.join("pid"), "4243").unwrap();
        let dead = aeon_fuse_minutes_with("sp-x", &wt, &run, None, crate::util::now_epoch(), &alive);
        assert_ne!(dead, "gate");
    }

    #[test]
    fn cmdline_names_gate_reads_nul_joined_argv() {
        assert!(cmdline_names_gate(b"gate.sh\0sleep\09999\0"));
        assert!(!cmdline_names_gate(b"sleep\09999\0"));
        assert!(!cmdline_names_gate(b""));
    }

    #[test]
    fn aeon_fuse_minutes_a_gate_that_just_finished_resets_it() {
        let d = tmp("fuse-gatefin");
        let run = d.join("run");
        let gate_dir = run.join("gate-run").join("testname.spira_sp-x");
        std::fs::create_dir_all(&gate_dir).unwrap();
        let wt = d.join("worktree/sp-x");
        std::fs::create_dir_all(&wt).unwrap();
        let old = crate::util::now_epoch() - 60 * 60;
        std::fs::write(wt.join("old.txt"), "x").unwrap();
        set_mtime(&wt.join("old.txt"), old);
        set_mtime(&wt, old);

        let before = aeon_fuse_minutes("sp-x", &wt, &run, None, crate::util::now_epoch());
        let n: i64 = before.parse().unwrap();
        assert!(n >= 55, "positive control: stale worktree should read ~60m, got {n}");

        std::fs::write(gate_dir.join("rc"), "0\n").unwrap();
        std::fs::write(gate_dir.join("out"), "gate output\n").unwrap();
        std::fs::write(gate_dir.join("started"), format!("{}\n", crate::util::now_epoch())).unwrap();
        let after = aeon_fuse_minutes("sp-x", &wt, &run, None, crate::util::now_epoch());
        let n: i64 = after.parse().unwrap();
        assert!(n < 5, "a gate that just finished resets the fuse, got {n}");
    }

    fn set_mtime(p: &Path, epoch: i64) {
        let t = filetime_like(epoch);
        let times = (t, t);
        unsafe {
            let cpath = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
            let utimbuf = libc::utimbuf { actime: times.0, modtime: times.1 };
            libc::utime(cpath.as_ptr(), &utimbuf);
        }
    }
    fn filetime_like(epoch: i64) -> libc::time_t {
        epoch as libc::time_t
    }

    // ---- aeon_lease_minutes (test-aeon-lease.sh) ----

    #[test]
    fn aeon_lease_minutes_table() {
        let d = tmp("lease");
        let run = d.join("run");
        std::fs::create_dir_all(run.join("aeon")).unwrap();
        let bead = "sp-test01";

        assert_eq!(aeon_lease_minutes(bead, &run, 1000), "?", "no lease file");

        let now = 1_000_000i64;
        std::fs::write(run.join("aeon").join(format!("{bead}.lease")), (now + 600).to_string()).unwrap();
        assert_eq!(aeon_lease_minutes(bead, &run, now), "10", "a 600s future deadline renders ~10m");

        std::fs::write(run.join("aeon").join(format!("{bead}.lease")), (now - 120).to_string()).unwrap();
        assert_eq!(aeon_lease_minutes(bead, &run, now), "-2", "a past deadline renders negative minutes");

        std::fs::write(run.join("aeon").join(format!("{bead}.lease")), "").unwrap();
        assert_eq!(aeon_lease_minutes(bead, &run, now), "?", "an empty lease file");

        std::fs::write(run.join("aeon").join(format!("{bead}.lease")), "not-a-number").unwrap();
        assert_eq!(aeon_lease_minutes(bead, &run, now), "?", "a non-numeric lease file");

        assert_eq!(aeon_lease_minutes("", &run, now), "?", "an empty bead id");
    }

    // ---- _tsd_aeon_session ----

    #[test]
    fn tsd_aeon_session_is_best_effort_and_never_panics_without_tsd_write() {
        struct NoExec;
        impl Exec for NoExec {
            fn exec(&self, _prog: &str, _args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> crate::util::Out {
                crate::util::Out::fail(127, "no such program")
            }
        }
        let fields = SessionFields { wall_s: Some(12), api_s: Some(9), turns: Some(3), cost_usd: Some(0.12345), ..Default::default() };
        tsd_aeon_session(&NoExec, "/run", "sp-a", "builder", 0, "closed", &fields);
        // reaching here without panicking is the whole assertion: best-effort, exit status ignored.
    }
}
