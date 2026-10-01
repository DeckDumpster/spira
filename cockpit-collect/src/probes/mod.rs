//! The probe library — one function per `spira/cockpit.sh` `*_keys` function, each producing
//! the exact `KEY=value` lines (unquoted; the supervisor's merge applies the one layer of
//! shell quoting) that function printed. DESIGN.md "Design" names the split: data fetching
//! goes through [`crate::io`] exactly as the bash did (including the `lib.sh` bridge for
//! functions this bead does not own); the sanitizing/dating/sorting that used to be inline
//! `python3 -c` is native Rust here, unit tested against literal fixtures.

mod core_detail;
mod queue;
mod ratelim;
mod reachable;
mod unsent;

use crate::io;
use crate::quoting::{sanitize, sanitize_title};
use serde_json::Value;
use std::path::Path;

pub type Kv = Vec<(String, String)>;

fn push(out: &mut Kv, k: &str, v: impl Into<String>) {
    out.push((k.to_string(), v.into()));
}

/// Render a [`Kv`] as the raw `KEY=value` lines a probe subcommand prints on stdout —
/// exactly what `cockpit.sh <subcommand>` echoed, unquoted (the supervisor's fragment/merge
/// layer is the only place that quotes).
pub fn render(kv: &Kv) -> String {
    let mut s = String::new();
    for (k, v) in kv {
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        s.push('\n');
    }
    s
}

/// `probe()`: the full backward-compatible serial pass `cockpit.sh once`/`loop` ran, in the
/// bash's own order. SP_AT first (`now_keys` emits it as its first line), SP_PASS_SECS last.
pub fn full_pass() -> Kv {
    let start = io::now();
    let mut out = Kv::new();
    out.extend(now_keys());
    out.extend(core_detail::core_detail_keys());
    out.extend(core_counts_keys());
    out.extend(slots_keys());
    out.extend(unsent::unsent_keys());
    out.extend(queue::queue_keys());
    out.extend(reachable::reachable_keys());
    out.extend(sphere_keys());
    out.extend(repo_label_keys());
    out.extend(strand_keys());
    out.extend(dup_refs_keys());
    out.extend(livelock_keys());
    out.extend(sop_keys());
    out.extend(ratelim::ratelim_keys());
    out.extend(statute_keys());
    out.extend(czar_triggers_keys());
    out.push(("SP_PASS_SECS".to_string(), (io::now() - start).to_string()));
    out
}

/// First-wins de-dup over a [`Kv`], matching `write_snapshot`'s own merge: a key emitted
/// twice (a probe's rows followed by its own fallback firing too) keeps the first value —
/// the one that actually measured something.
pub fn dedup_first_wins(kv: Kv) -> Kv {
    let mut seen = std::collections::HashSet::new();
    kv.into_iter().filter(|(k, _)| seen.insert(k.clone())).collect()
}

/// Dispatch by the same subcommand names `spira/cockpit.sh`'s case statement used.
pub fn run(subcommand: &str) -> Option<Kv> {
    match subcommand {
        "now" => Some(now_keys()),
        "core" => {
            let mut kv = core_detail::core_detail_keys();
            kv.extend(core_counts_keys());
            Some(kv)
        }
        "core_detail" => Some(core_detail::core_detail_keys()),
        "slots" => Some(slots_keys()),
        "unsent" => Some(unsent::unsent_keys()),
        "queue" => Some(queue::queue_keys()),
        "reachable" => Some(reachable::reachable_keys()),
        "sphere" => Some(sphere_keys()),
        "repo_labels" => Some(repo_label_keys()),
        "livelock" => Some(livelock_keys()),
        "dup_refs" => Some(dup_refs_keys()),
        "strands" => Some(strand_keys()),
        "ratelim" => Some(ratelim::ratelim_keys()),
        "sops" => Some(sop_keys()),
        "statute" => Some(statute_keys()),
        "mail" => Some(mail_keys()),
        "czar_triggers" => Some(czar_triggers_keys()),
        "sending" => Some(sending_keys()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// now_keys — fast tier: /proc and the filesystem only, no bd/git.
// ---------------------------------------------------------------------------------------

pub fn now_keys() -> Kv {
    let mut out = Kv::new();
    let start = io::now();
    push(&mut out, "SP_AT", start.to_string());
    let window_h = std::env::var("SPIRA_COCKPIT_WINDOW_HOURS").unwrap_or_else(|_| "24".to_string());
    push(&mut out, "SP_WINDOW_HOURS", window_h);

    let home = io::home_dir();
    let run = io::run_dir();
    let mut i = 0usize;
    if let Ok(entries) = std::fs::read_dir(&run) {
        let mut pidfiles: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("aeon-") && n.ends_with(".pid"))
                    .unwrap_or(false)
            })
            .collect();
        pidfiles.sort();
        for pf in pidfiles {
            if !aeon_alive(&pf) {
                continue;
            }
            let base = pf.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            // base = aeon-<fayth>-<bead>
            let rest = base.strip_prefix("aeon-").unwrap_or("");
            let (fay, bead) = rest.split_once('-').unwrap_or(("", rest));
            let name = io::lib_call(&home, "aeon_named", &[pf.to_str().unwrap_or("")])
                .unwrap_or_else(|| "?".to_string());
            let pid = io::read_trim(&pf).and_then(|s| s.trim().parse::<i64>().ok());
            let secs = pid.and_then(io::proc_etimes).unwrap_or(0);

            let meta = io::bdjson(&["show", bead]);
            let (pri, title, repo_name) = parse_show_meta(meta, bead);

            let partition = io::bdq(&["state", bead, "fayth"])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "?".to_string());

            push(&mut out, &format!("SP_AEON{i}_PRI"), pri);
            push(&mut out, &format!("SP_AEON{i}_PARTITION"), partition);
            push(&mut out, &format!("SP_AEON{i}_TITLE"), title);
            push(&mut out, &format!("SP_AEON{i}_NAME"), name);
            push(&mut out, &format!("SP_AEON{i}_FAYTH"), if fay.is_empty() { "?".into() } else { fay.to_string() });

            let fayth_model = std::fs::read_to_string(home.join("chamber").join(format!("{fay}.fayth")))
                .ok()
                .and_then(|c| {
                    c.lines()
                        .find_map(|l| l.strip_prefix("FAYTH_MODEL=").map(|s| s.to_string()))
                })
                .unwrap_or_else(|| "?".to_string());
            push(&mut out, &format!("SP_AEON{i}_FAYTH_MODEL"), fayth_model);
            push(&mut out, &format!("SP_AEON{i}_BEAD"), bead);
            push(&mut out, &format!("SP_AEON{i}_MIN"), (secs / 60).to_string());

            // aeon_fuse_minutes (wave 4.34, sp-27d3d): ported to aeon::trace, called
            // in-process. `base` (family W, base refs) is in-process too now (sp-k6lku,
            // "wave 4.13") through the same `io::repo_registry()` unsent.rs/queue.rs
            // already use, not the generic lib.sh bridge — the commit-ahead timestamp and
            // everything else is native.
            let wt = run.join("worktree").join(bead);
            let commit_ahead_ts = spira_config::repos::landref(&io::repo_registry(), &repo_name)
                .filter(|b| !b.is_empty())
                .and_then(|base| io::git(&wt, &["log", "--format=%ct", "-1", &format!("{base}..HEAD")]))
                .and_then(|t| t.trim().parse::<i64>().ok());
            let fuse = aeon::trace::aeon_fuse_minutes(bead, &wt, &run, commit_ahead_ts, io::now());
            push(&mut out, &format!("SP_AEON{i}_FUSE"), fuse);
            push(&mut out, &format!("SP_AEON{i}_WALL"), (secs / 60).to_string());

            let lease = aeon::trace::aeon_lease_minutes(bead, &run, io::now());
            push(&mut out, &format!("SP_AEON{i}_LEASE"), lease);

            let mark = std::env::var("SPIRA_TRACE_MARK").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "=== spira attempt".to_string());
            let log_path = run.join(format!("{bead}.log"));
            let stats = aeon::trace::trace_stats(&log_path, &mark, io::now());
            for line in stats.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    push(&mut out, &format!("SP_AEON{i}_{k}"), v.to_string());
                }
            }
            let tl: i64 = std::env::var("SPIRA_COCKPIT_TRACE_LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
            if tl > 0 {
                let tail = aeon::trace::trace_tail(&log_path, &mark, tl as usize);
                let lines: Vec<&str> = tail.lines().map(sanitize_line).collect();
                let lines: Vec<&str> = lines.into_iter().filter(|l| !l.is_empty()).collect();
                let n = lines.len();
                let start_n = n.saturating_sub(tl as usize);
                for (j, l) in lines[start_n..].iter().enumerate() {
                    push(&mut out, &format!("SP_AEON{i}_ACT{j}"), l.to_string());
                }
            }
            i += 1;
        }
    }
    push(&mut out, "SP_AEON_N", i.to_string());

    push(&mut out, "SP_SENTINEL_TIMER", unit_active_key(&home, "sentinel", "timer"));
    push(&mut out, "SP_SENTINEL_AGE", age_of(&run.join("sentinel.log")));
    push(&mut out, "SP_OPS_TIMER", unit_active_key(&home, "ops", "timer"));

    let mut ops_age = age_of(&run.join("ops.log"));
    if let Ok(entries) = std::fs::read_dir(&run) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("aeon-ops-") && name.ends_with(".pid") && aeon_alive(&e.path()) {
                ops_age = "0".to_string();
                break;
            }
        }
    }
    push(&mut out, "SP_OPS_AGE", ops_age);

    push(&mut out, "SP_AURON_TIMER", unit_active_key(&home, "auron", "timer"));
    push(&mut out, "SP_AURON_AGE", age_of(&run.join("auron.status")));
    let auron_status = run.join("auron.status");
    if auron_status.is_file() {
        let (firing, keys) = read_auron_status(&auron_status);
        push(&mut out, "SP_AURON_FIRING", firing);
        push(&mut out, "SP_AURON_KEYS", keys);
    } else {
        push(&mut out, "SP_AURON_FIRING", "?");
        push(&mut out, "SP_AURON_KEYS", "");
    }

    let rs_out = io::run_tool("release", &["status"], None).unwrap_or_default();
    let rs_line = rs_out.lines().find(|l| l.starts_with("RUNNING UNLANDED ")).unwrap_or("");
    let rs_alert = rs_out.lines().find(|l| l.starts_with("ALERT ")).unwrap_or("");
    push(&mut out, "SP_HOTFIX_LINE", rs_line);
    push(&mut out, "SP_HOTFIX_ALERT", rs_alert);

    let (ov_n, ov_failed, ov_list) = overrides_summary();
    push(&mut out, "SP_OVERRIDES_N", ov_n.to_string());
    push(&mut out, "SP_OVERRIDES_FAILED", ov_failed.to_string());
    push(&mut out, "SP_OVERRIDES_LIST", ov_list);

    let (gate_rows, gate_n, gate_live) = gate_run_scan(&run);
    for (idx, (slug, age, phase, why)) in gate_rows.iter().enumerate() {
        push(&mut out, &format!("SP_GATE{idx}_SLUG"), slug.clone());
        push(&mut out, &format!("SP_GATE{idx}_AGE"), age.clone());
        push(&mut out, &format!("SP_GATE{idx}_PHASE"), phase.clone());
        if !why.is_empty() {
            push(&mut out, &format!("SP_GATE{idx}_WHY"), why.clone());
        }
    }
    push(&mut out, "SP_GATE_N", gate_n.to_string());
    push(&mut out, "SP_GATE_LIVE", gate_live.to_string());

    let mut live_aeons = 0;
    if let Ok(entries) = std::fs::read_dir(&run) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("aeon-") && name.ends_with(".pid") && aeon_alive(&e.path()) {
                live_aeons += 1;
            }
        }
    }
    push(&mut out, "SP_AEONS", live_aeons.to_string());

    let cap_at: i64 = io::lib_call(&home, "capacity_pause_until", &[])
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let cap_now = io::now();
    if cap_at > cap_now {
        push(&mut out, "SP_CAPACITY_PAUSED", "1");
        push(&mut out, "SP_CAPACITY_LEFT", (cap_at - cap_now).to_string());
        push(&mut out, "SP_CAPACITY_AT", fmt_hm(cap_at));
        push(&mut out, "SP_CAPACITY_WHY", io::lib_call(&home, "capacity_pause_why", &[]).unwrap_or_default());
    } else {
        push(&mut out, "SP_CAPACITY_PAUSED", "0");
        push(&mut out, "SP_CAPACITY_LEFT", "0");
        push(&mut out, "SP_CAPACITY_AT", "");
        push(&mut out, "SP_CAPACITY_WHY", "");
    }

    out
}

fn sanitize_line(l: &str) -> &str {
    // trace_tail's own python already sanitizes and truncates each line; this is passed
    // through verbatim. Kept as a named seam in case a caller needs a second clamp.
    l
}

fn fmt_hm(epoch: i64) -> String {
    io::run_tool("date", &["-d", &format!("@{epoch}"), "+%H:%M"], None)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_auron_status(path: &Path) -> (String, String) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut firing = "?".to_string();
    let mut keys = String::new();
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("SP_AURON_FIRING=") {
            firing = v.trim_matches('\'').to_string();
        } else if let Some(v) = line.strip_prefix("SP_AURON_KEYS=") {
            keys = v.trim_matches('\'').to_string();
        }
    }
    (firing, keys)
}

fn overrides_summary() -> (usize, usize, String) {
    let out = io::run_tool("overrides.sh", &["list"], None).unwrap_or_default();
    let mut n = 0;
    let mut failed = 0;
    let mut list = Vec::new();
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(bead), state) = (it.next(), it.next(), it.next()) else { continue };
        let state = state.unwrap_or("");
        if state == "retired" {
            continue;
        }
        if state.starts_with("failed:") {
            failed += 1;
        }
        n += 1;
        list.push(format!("{name}:{bead}"));
    }
    (n, failed, list.join(","))
}

fn gate_run_scan(run: &Path) -> (Vec<(String, String, String, String)>, usize, usize) {
    let dir = run.join("gate-run");
    let mut rows = Vec::new();
    let mut n = 0;
    let mut live = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for gd in dirs {
            let pid_file = gd.join("pid");
            if !pid_file.is_file() {
                continue;
            }
            let slug = gd.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let pid: i64 = match io::read_trim(&pid_file).and_then(|s| s.parse().ok()) {
                Some(p) => p,
                None => continue,
            };
            if !io::proc_exists(pid) {
                continue;
            }
            let cmd = io::proc_cmdline(pid).unwrap_or_default();
            if !cmd.contains("gate") {
                continue;
            }
            live += 1;
            let started: Option<i64> = io::read_trim(&gd.join("started")).and_then(|s| s.parse().ok());
            let age = started.map(|s| (io::now() - s).to_string()).unwrap_or_else(|| "?".to_string());
            let out_file = gd.join("out");
            let out_has_content = std::fs::metadata(&out_file).map(|m| m.len() > 0).unwrap_or(false);
            let phase = if out_has_content { "running" } else { "waiting" };
            let mut why = String::new();
            if phase == "running" {
                if let Ok(content) = std::fs::read_to_string(&out_file) {
                    if let Some(line) = content.lines().find(|l| l.contains("selecting")) {
                        why = sanitize(line);
                        why.truncate(100);
                    }
                }
            }
            n += 1;
            rows.push((slug, age, phase.to_string(), why));
        }
    }
    (rows, n, live)
}

fn unit_active_key(home: &Path, fayth: &str, kind: &str) -> String {
    let unit = io::lib_call(home, "spira_unit", &[fayth, kind]).unwrap_or_else(|| "?".to_string());
    match io::unit_active(&unit) {
        Some(true) => "1".to_string(),
        Some(false) => "0".to_string(),
        None => "?".to_string(),
    }
}

fn age_of(path: &Path) -> String {
    io::mtime_age_secs(path).map(|s| s.to_string()).unwrap_or_else(|| "?".to_string())
}

/// `aeon_alive` (`lib.sh`): the recorded pid must be live AND its argv must actually be an
/// aeon runner, never a recycled pid — `grep -qE '(^|/)aeon( |$)|aeon\.sh'` on the
/// NUL-joined-as-spaces cmdline. Reimplemented natively (no regex crate in this workspace;
/// this runs once per live aeon on the 5s tier, so a `bash`-bridge round trip per pidfile
/// would be the wrong cost to pay) rather than bridged, unlike the `lib.sh` helpers that do
/// real work — this one is three token comparisons.
fn aeon_alive(pidfile: &Path) -> bool {
    let Some(pid) = io::read_trim(pidfile).and_then(|s| s.trim().parse::<i64>().ok()) else {
        return false;
    };
    if !io::proc_exists(pid) {
        return false;
    }
    let Some(cmd) = io::proc_cmdline(pid) else { return false };
    is_aeon_cmd(&cmd)
}

fn is_aeon_cmd(cmd: &str) -> bool {
    if cmd.contains("aeon.sh") {
        return true;
    }
    cmd.split(' ').any(|tok| tok == "aeon" || tok.ends_with("/aeon"))
}

/// `show <bead>` -> (priority, sanitized title, repo name), `?`/empty on any failure.
fn parse_show_meta(raw: Option<String>, _bead: &str) -> (String, String, String) {
    let Some(rows) = io::bd_rows(raw) else {
        return ("?".to_string(), "?".to_string(), "?".to_string());
    };
    let Some(row) = rows.first() else {
        return ("?".to_string(), "?".to_string(), "?".to_string());
    };
    let pri = row.get("priority").map(value_to_plain).unwrap_or_else(|| "?".to_string());
    let title = sanitize_title(row.get("title").and_then(Value::as_str).unwrap_or(""), 80);
    let repo = row
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .find_map(|l| l.strip_prefix("repo:"))
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    (pri, title, repo)
}

fn value_to_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "?".to_string(),
    }
}

/// Python's own truthiness (`if v.get("escalated")`, not `v.get("escalated") is True`):
/// `strands.json`'s `escalated` field is a *timestamp* when set (0 while unescalated,
/// a nonzero epoch once raised) — `strand_keys`' own row shape, not a boolean.
fn py_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ---------------------------------------------------------------------------------------
// core_counts_keys — SP_WAITING only (SP_READY belongs to core_detail_keys).
// ---------------------------------------------------------------------------------------

pub fn core_counts_keys() -> Kv {
    let mut out = Kv::new();
    let ask_label = std::env::var("SPIRA_ASK_LABEL").unwrap_or_else(|_| "needs-ryan".to_string());
    let rows = io::bd_rows(io::bdjson(&["list", "--status", "open", "--limit", "0", "--label", &ask_label]));
    match rows {
        None => push(&mut out, "SP_WAITING", "?"),
        Some(rows) => push(&mut out, "SP_WAITING", rows.len().to_string()),
    }
    out
}

// ---------------------------------------------------------------------------------------
// slots_keys
// ---------------------------------------------------------------------------------------

pub fn slots_keys() -> Kv {
    let mut out = Kv::new();
    let home = io::home_dir();
    let live = io::lib_call(&home, "aeons_live_total", &[]).unwrap_or_else(|| "?".to_string());
    push(&mut out, "SP_SLOTS_LIVE", live);

    // SPIRA_MAX_AEONS is never set into this process's own environment (io::NEVER_EXPORTED)
    // — read io::max_aeons() instead of std::env::var directly.
    let pool: i64 = io::max_aeons().parse().ok().unwrap_or(0);
    let lane_fayths = io::lib_call(&home, "spira_lane_fayths", &[]).unwrap_or_default();
    let mut lt = 0i64;
    for f in lane_fayths.split_whitespace() {
        let ln: i64 = io::lib_call(&home, "fayth_get", &[f, "FAYTH_MAX_CONCURRENT", "1"])
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        lt += ln;
    }
    let ceiling = std::env::var("SPIRA_MAX_LIVE_AEONS").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(pool + lt);
    push(&mut out, "SP_SLOTS_CEILING", ceiling.to_string());

    let lanes_live = io::lib_call(&home, "aeons_live_lanes", &[]).unwrap_or_else(|| "?".to_string());
    push(&mut out, "SP_SLOTS_LANES_LIVE", lanes_live);

    let snap = io::run_dir().join("cockpit.env");
    let ready = std::fs::read_to_string(&snap)
        .ok()
        .and_then(|c| {
            c.lines()
                .find_map(|l| l.strip_prefix("SP_READY=").map(|v| v.trim_matches('\'').to_string()))
        })
        .unwrap_or_else(|| "?".to_string());
    push(&mut out, "SP_SLOTS_READY", ready);

    let cap_at: i64 = io::lib_call(&home, "capacity_pause_until", &[]).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    push(&mut out, "SP_SLOTS_CAPACITY_PAUSED", if cap_at > io::now() { "1" } else { "0" });
    out
}

// ---------------------------------------------------------------------------------------
// sphere_keys
// ---------------------------------------------------------------------------------------

pub fn sphere_keys() -> Kv {
    let mut out = Kv::new();
    let poison_rows = io::bd_rows(io::bdjson(&["list", "--all", "--limit", "0", "--label", "spira-poison"]));
    match poison_rows {
        None => push(&mut out, "SP_POISON", "?"),
        Some(rows) => {
            let n = rows.iter().filter(|r| r.get("status").and_then(Value::as_str) != Some("closed")).count();
            push(&mut out, "SP_POISON", n.to_string());
        }
    }

    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let ask = std::env::var("SPIRA_ASK_LABEL").unwrap_or_else(|_| "needs-ryan".to_string());
    let rows = io::bd_rows(io::bdjson(&["list", "--limit", "0", "--label", &label]));
    match rows {
        None => {
            push(&mut out, "SP_OPEN", "?");
            push(&mut out, "SP_INPROG", "?");
            push(&mut out, "SP_NEEDSOP", "?");
        }
        Some(rows) => {
            const WORK: &[&str] = &["task", "bug", "feature", "epic", "chore", "spike"];
            let work: Vec<&Value> = rows
                .iter()
                .filter(|i| {
                    let is_work = i.get("_is_work").and_then(Value::as_i64);
                    match is_work {
                        Some(1) => true,
                        Some(_) => false,
                        None => i
                            .get("issue_type")
                            .and_then(Value::as_str)
                            .map(|t| WORK.contains(&t))
                            .unwrap_or(false),
                    }
                })
                .collect();
            let has = |i: &Value, lab: &str| {
                i.get("labels")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().any(|l| l.as_str() == Some(lab)))
                    .unwrap_or(false)
            };
            let open_n = work.iter().filter(|i| i.get("status").and_then(Value::as_str) != Some("closed")).count();
            let inprog_n = work.iter().filter(|i| i.get("status").and_then(Value::as_str) == Some("in_progress")).count();
            let needsop_n = work
                .iter()
                .filter(|i| i.get("status").and_then(Value::as_str) != Some("closed") && has(i, &ask))
                .count();
            push(&mut out, "SP_OPEN", open_n.to_string());
            push(&mut out, "SP_INPROG", inprog_n.to_string());
            push(&mut out, "SP_NEEDSOP", needsop_n.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// repo_label_keys
// ---------------------------------------------------------------------------------------

pub fn repo_label_keys() -> Kv {
    let mut out = Kv::new();
    let map_path = std::env::var("SPIRA_REPO_MAP").unwrap_or_default();
    let Ok(map_content) = std::fs::read_to_string(&map_path) else {
        push(&mut out, "SP_REPO_UNMAPPED", "?");
        push(&mut out, "SP_REPO_ABSENT", "?");
        return out;
    };
    let valid: std::collections::HashSet<String> = map_content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split('|').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let rows = io::bd_rows(io::bdjson(&["list", "--limit", "0"]));
    match rows {
        None => {
            push(&mut out, "SP_REPO_UNMAPPED", "?");
            push(&mut out, "SP_REPO_ABSENT", "?");
        }
        Some(rows) => {
            let mut unmapped = 0;
            let mut absent = 0;
            for i in &rows {
                let labels: Vec<&str> = i.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let repo_labels: Vec<&str> = labels.iter().filter_map(|l| l.strip_prefix("repo:")).collect();
                if repo_labels.is_empty() {
                    absent += 1;
                } else if !repo_labels.iter().any(|r| valid.contains(*r)) {
                    unmapped += 1;
                }
            }
            push(&mut out, "SP_REPO_UNMAPPED", unmapped.to_string());
            push(&mut out, "SP_REPO_ABSENT", absent.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// dup_refs_keys
// ---------------------------------------------------------------------------------------

pub fn dup_refs_keys() -> Kv {
    let mut out = Kv::new();
    let lookback: i64 = std::env::var("SPIRA_INCIDENT_DEDUP_LOOKBACK").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let since = io::run_tool("date", &["-u", "-d", &format!("-{lookback} days"), "+%Y-%m-%d"], None).map(|s| s.trim().to_string());
    let Some(since) = since else {
        push(&mut out, "SP_DUP_REFS", "?");
        push(&mut out, "SP_DUP_BEADS", "?");
        push(&mut out, "SP_DUP_N", "0");
        return out;
    };
    let rows = io::bd_rows(io::bdjson(&["list", "--all", "--limit", "0", "--label", "spira,incident"]));
    let Some(rows) = rows else {
        push(&mut out, "SP_DUP_REFS", "?");
        push(&mut out, "SP_DUP_BEADS", "?");
        push(&mut out, "SP_DUP_N", "0");
        return out;
    };
    let mut by_ref: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for i in &rows {
        let ref_ = i.get("external_ref").and_then(Value::as_str).unwrap_or("");
        if ref_.is_empty() {
            continue;
        }
        let labels: Vec<&str> = i.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if labels.iter().any(|l| l.starts_with("duplicate-of:")) {
            continue;
        }
        if i.get("status").and_then(Value::as_str) == Some("closed") {
            let closed_at = i.get("closed_at").and_then(Value::as_str).unwrap_or("");
            let closed_date = closed_at.get(..10).unwrap_or("");
            if closed_date < since.as_str() {
                continue;
            }
        }
        let id = i.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        by_ref.entry(ref_.to_string()).or_default().push(id);
    }
    let mut dup: Vec<(String, Vec<String>)> = by_ref.into_iter().filter(|(_, ids)| ids.len() > 1).collect();
    push(&mut out, "SP_DUP_REFS", dup.len().to_string());
    push(&mut out, "SP_DUP_BEADS", dup.iter().map(|(_, ids)| ids.len() - 1).sum::<usize>().to_string());
    dup.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let rows: Vec<_> = dup.into_iter().take(5).collect();
    for (n, (ref_, ids)) in rows.iter().enumerate() {
        let line = format!("{ref_} +{} {}", ids.len() - 1, ids.iter().take(5).cloned().collect::<Vec<_>>().join(" "));
        let mut line = line;
        line.truncate(120);
        push(&mut out, &format!("SP_DUP_ROW{n}"), line);
    }
    push(&mut out, "SP_DUP_N", rows.len().to_string());
    out
}

// ---------------------------------------------------------------------------------------
// livelock_keys — delegates the detection itself to lib.sh (`detect_livelocked`,
// `detect_invalid_closed`); this only classifies refusal-vs-empty and formats rows.
// ---------------------------------------------------------------------------------------

pub fn livelock_keys() -> Kv {
    let mut out = Kv::new();
    let home = io::home_dir();
    let ll_out = io::lib_call(&home, "detect_livelocked", &[]).unwrap_or_default();
    if ll_out.is_empty() && io::bdjson(&["list", "--limit", "1"]).is_none() {
        push(&mut out, "SP_LIVELOCKED", "?");
        push(&mut out, "SP_LIVELOCK_N", "?");
    } else {
        let mut n = 0;
        for line in ll_out.lines() {
            if !line.starts_with("LIVELOCK ") {
                continue;
            }
            let mut s = sanitize(line);
            s.truncate(120);
            push(&mut out, &format!("SP_LIVELOCK{n}"), s);
            n += 1;
            if n >= 20 {
                break;
            }
        }
        push(&mut out, "SP_LIVELOCKED", n.to_string());
        push(&mut out, "SP_LIVELOCK_N", n.to_string());
    }

    // `detect_invalid_closed` (lib.sh) emits three row shapes: `INVALID-CLOSED <id> — ...`,
    // `UNFILED-FOLLOW <id> — ...` and `ALLOWED-IC <id> — ...` (a bead on
    // $SPIRA_RUN/invalid-closed.allow, reported but not counted as either). None of the
    // three caps at 20 rows the way LIVELOCK above does — the bash's own cap check there is
    // `[ "$_n" -ge 20 ] && true`, which never breaks — so every matching row is emitted.
    let ic_out = io::lib_call(&home, "detect_invalid_closed", &[]).unwrap_or_default();
    if ic_out.is_empty() && io::bdjson(&["list", "--status", "closed", "--limit", "1"]).is_none() {
        push(&mut out, "SP_INVALID_CLOSED", "?");
        push(&mut out, "SP_INVCLSD_N", "?");
        push(&mut out, "SP_UNFILED_FOLLOW", "?");
        push(&mut out, "SP_UNFLFLW_N", "?");
    } else {
        let mut inv_n = 0;
        let mut unf_n = 0;
        let mut alw_n = 0;
        for line in ic_out.lines() {
            if line.is_empty() {
                continue;
            }
            let mut s = sanitize(line);
            s.truncate(120);
            if line.starts_with("INVALID-CLOSED ") {
                push(&mut out, &format!("SP_INVCLSD{inv_n}"), s);
                inv_n += 1;
            } else if line.starts_with("UNFILED-FOLLOW ") {
                push(&mut out, &format!("SP_UNFLFLW{unf_n}"), s);
                unf_n += 1;
            } else if line.starts_with("ALLOWED-IC ") {
                push(&mut out, &format!("SP_ALLOWEDIC{alw_n}"), s);
                alw_n += 1;
            }
        }
        push(&mut out, "SP_INVALID_CLOSED", inv_n.to_string());
        push(&mut out, "SP_INVCLSD_N", inv_n.to_string());
        push(&mut out, "SP_UNFILED_FOLLOW", unf_n.to_string());
        push(&mut out, "SP_UNFLFLW_N", unf_n.to_string());
        push(&mut out, "SP_ALLOWED_IC", alw_n.to_string());
        push(&mut out, "SP_ALLOWEDIC_N", alw_n.to_string());
    }
    out
}

// ---------------------------------------------------------------------------------------
// strand_keys
// ---------------------------------------------------------------------------------------

pub fn strand_keys() -> Kv {
    let mut out = Kv::new();
    let path = io::run_dir().join("strands.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        for k in ["SP_STRANDS", "SP_STRANDS_ESCALATED", "SP_STRAND_GHOST", "SP_STRAND_OTHER"] {
            push(&mut out, k, "?");
        }
        return out;
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&content) else {
        for k in ["SP_STRANDS", "SP_STRANDS_ESCALATED", "SP_STRAND_GHOST", "SP_STRAND_OTHER"] {
            push(&mut out, k, "?");
        }
        return out;
    };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut bad = 0usize;
    let mut escalated = 0usize;
    for (k, v) in &map {
        let parts: Vec<&str> = k.rsplitn(3, ':').collect();
        // rsplitn gives reverse order; need the middle field of a 3-part colon split.
        if parts.len() != 3 || parts[1].is_empty() {
            bad += 1;
        } else {
            *counts.entry(parts[1].to_string()).or_insert(0) += 1;
        }
        if py_truthy(v.get("escalated")) {
            escalated += 1;
        }
    }
    push(&mut out, "SP_STRANDS", map.len().to_string());
    push(&mut out, "SP_STRANDS_ESCALATED", escalated.to_string());
    if bad > 0 {
        push(&mut out, "SP_STRAND_GHOST", "?");
    } else {
        push(&mut out, "SP_STRAND_GHOST", counts.get("ghost").copied().unwrap_or(0).to_string());
    }
    let mut rest: Vec<String> = counts.iter().filter(|(k, _)| k.as_str() != "ghost").map(|(k, v)| format!("{k}={v}")).collect();
    if bad > 0 {
        rest.push(format!("unclassified={bad}"));
    }
    push(&mut out, "SP_STRAND_OTHER", if rest.is_empty() { "none".to_string() } else { rest.join(",") });
    out
}

// ---------------------------------------------------------------------------------------
// czar_triggers_keys
// ---------------------------------------------------------------------------------------

const CZAR_CLASSES: &[(&str, &str)] = &[
    ("incident:queue-deadlock-batch-open", "DEADLOCK"),
    ("incident:queue-attribution-failed-requeue", "ATTRIB"),
    ("incident:queue-sort-failed-ranking", "SORT"),
    ("incident:queue-loop-stalled", "STALL"),
];

pub fn czar_triggers_keys() -> Kv {
    let mut out = Kv::new();
    let label = std::env::var("SPIRA_CZAR_LABEL").unwrap_or_else(|_| "czar-trigger".to_string());
    let raw = io::bdjson(&["list", "--label", &label, "--all", "--limit", "0", "--brief"]);
    let rows = match &raw {
        Some(s) if !s.trim().is_empty() => serde_json::from_str::<Value>(s.trim()).ok().map(|v| match v {
            Value::Array(a) => a,
            other => vec![other],
        }),
        _ => None,
    };
    let refused = rows.as_ref().map(|r| r.first().and_then(|b| b.get("_refused")).and_then(Value::as_bool).unwrap_or(false)).unwrap_or(true);
    if rows.is_none() || refused {
        for (_, tag) in CZAR_CLASSES {
            for sfx in ["FIRED", "BY", "OUTCOME"] {
                push(&mut out, &format!("SP_CZAR_{tag}_{sfx}"), "?");
            }
        }
        return out;
    }
    let rows = rows.unwrap();
    let mut by_ref: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
    for b in &rows {
        let r = b.get("external_ref").and_then(Value::as_str).unwrap_or("").to_string();
        by_ref.entry(r).or_default().push(b);
    }
    for (r, tag) in CZAR_CLASSES {
        let mut beads: Vec<&&Value> = by_ref.get(*r).map(|v| v.iter().collect()).unwrap_or_default();
        beads.sort_by_key(|b| b.get("created_at").and_then(Value::as_str).and_then(crate::quoting::parse_iso8601).unwrap_or(0));
        if beads.is_empty() {
            push(&mut out, &format!("SP_CZAR_{tag}_FIRED"), "-");
            push(&mut out, &format!("SP_CZAR_{tag}_BY"), "-");
            push(&mut out, &format!("SP_CZAR_{tag}_OUTCOME"), "-");
            continue;
        }
        let newest = *beads.last().unwrap();
        let created = newest.get("created_at").and_then(Value::as_str).unwrap_or("");
        let fired = if created.is_empty() {
            "-".to_string()
        } else {
            format!("{}Z", created.get(..16).unwrap_or(created).trim_end_matches('T'))
        };
        let by = newest.get("assignee").and_then(Value::as_str).unwrap_or("-").split_whitespace().next().unwrap_or("-").to_string();
        let status = newest.get("status").and_then(Value::as_str).unwrap_or("");
        let outcome = if status == "open" || status == "in_progress" {
            "pending".to_string()
        } else if status == "closed" {
            let mut outcome = "yes".to_string();
            for (idx, bead) in beads.iter().enumerate().take(beads.len().saturating_sub(1)) {
                let cla = bead.get("closed_at").and_then(Value::as_str).and_then(crate::quoting::parse_iso8601);
                if let Some(cla) = cla {
                    let recurred = beads[idx + 1..].iter().any(|b2| {
                        b2.get("created_at").and_then(Value::as_str).and_then(crate::quoting::parse_iso8601).map(|ct| ct > cla).unwrap_or(false)
                    });
                    if recurred {
                        outcome = "no".to_string();
                        break;
                    }
                }
            }
            outcome
        } else {
            "-".to_string()
        };
        push(&mut out, &format!("SP_CZAR_{tag}_FIRED"), fired);
        push(&mut out, &format!("SP_CZAR_{tag}_BY"), by);
        push(&mut out, &format!("SP_CZAR_{tag}_OUTCOME"), outcome);
    }
    out
}

// ---------------------------------------------------------------------------------------
// sop_keys
// ---------------------------------------------------------------------------------------

pub fn sop_keys() -> Kv {
    let mut out = Kv::new();
    let ledger = std::env::var("SPIRA_SOP_LEDGER").unwrap_or_else(|_| io::run_dir().join("sop/applied.jsonl").to_string_lossy().into_owned());
    let raw = io::bdjson(&["memories"]);
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        push(&mut out, "SP_SOP_NEVER_FIRED", "?");
        push(&mut out, "SP_SOP_RECURRED", "?");
        push(&mut out, "SP_SWEEP_AGE", "?");
        return out;
    };
    let Ok(Value::Object(shelf)) = serde_json::from_str::<Value>(raw.trim()) else {
        push(&mut out, "SP_SOP_NEVER_FIRED", "?");
        push(&mut out, "SP_SOP_RECURRED", "?");
        push(&mut out, "SP_SWEEP_AGE", "?");
        return out;
    };
    let window_h: f64 = std::env::var("SPIRA_SOP_WINDOW_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24.0);
    let now = std::env::var("SPIRA_NOW").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or_else(io::now);
    let since = now - (window_h * 3600.0) as i64;

    let sop_keys_set: std::collections::HashSet<&str> = shelf.iter().filter(|(k, v)| v.is_string() && k.starts_with("sop-")).map(|(k, _)| k.as_str()).collect();
    let mut ledger_sops = std::collections::HashSet::new();
    let mut recurred = std::collections::HashSet::new();
    // The newest SWEEP timestamp comes from the ledger row's own `epoch` field — a plain
    // number every row carries — never from parsing `ts` (an ISO string used only for the
    // recurrence window). Unlike `recurred`, `epoch` is read from EVERY row, not gated on
    // `check=="pass" && held=="no"`.
    let mut newest_epoch: Option<i64> = None;
    // A MISSING LEDGER IS A VALID STATE (no SOP has ever been applied — every SOP is
    // never-fired). An UNREADABLE ledger (present but cannot be opened, e.g. a directory)
    // is the failure case and renders `?` for all three keys.
    if Path::new(&ledger).exists() {
        let Ok(content) = std::fs::read_to_string(&ledger) else {
            push(&mut out, "SP_SOP_NEVER_FIRED", "?");
            push(&mut out, "SP_SOP_RECURRED", "?");
            push(&mut out, "SP_SWEEP_AGE", "?");
            return out;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
            let k = r.get("sop").and_then(Value::as_str).unwrap_or("");
            if k.is_empty() {
                continue;
            }
            ledger_sops.insert(k.to_string());
            if r.get("check").and_then(Value::as_str) == Some("pass") && r.get("held").and_then(Value::as_str) == Some("no") {
                if let Some(ts) = r.get("ts").and_then(Value::as_str).and_then(crate::quoting::parse_iso8601) {
                    if ts >= since {
                        recurred.insert(k.to_string());
                    }
                }
            }
            if let Some(ep) = r.get("epoch").and_then(Value::as_f64) {
                if ep > 0.0 {
                    let ep = ep as i64;
                    if newest_epoch.map(|n| ep > n).unwrap_or(true) {
                        newest_epoch = Some(ep);
                    }
                }
            }
        }
    }
    let never_fired = sop_keys_set.iter().filter(|k| !ledger_sops.contains(**k)).count();
    push(&mut out, "SP_SOP_NEVER_FIRED", never_fired.to_string());
    push(&mut out, "SP_SOP_RECURRED", recurred.len().to_string());
    push(&mut out, "SP_SWEEP_AGE", newest_epoch.map(|e| (now - e).max(0).to_string()).unwrap_or_else(|| "?".to_string()));
    out
}

// ---------------------------------------------------------------------------------------
// statute_keys
// ---------------------------------------------------------------------------------------

pub fn statute_keys() -> Kv {
    let mut out = Kv::new();
    let wiki = std::env::var("SPIRA_WIKI").unwrap_or_default();
    if wiki.is_empty() {
        push(&mut out, "SP_STATUTE_DB_N", "?");
        push(&mut out, "SP_STATUTE_PAGE_N", "?");
        push(&mut out, "SP_STATUTE_SKEW", "?");
        return out;
    }
    let db_n: Option<usize> = io::bdjson(&["memories"]).filter(|s| !s.trim().is_empty()).and_then(|s| {
        serde_json::from_str::<Value>(s.trim()).ok()
    }).and_then(|v| v.as_object().map(|m| m.iter().filter(|(k, v)| v.is_string() && k.starts_with("law-")).count()));

    let page_n: Option<usize> = io::git(Path::new(&wiki), &["show", "HEAD:wiki/notes/common-law.md"])
        .map(|s| s.lines().filter(|l| l.starts_with("### ")).count());

    push(&mut out, "SP_STATUTE_DB_N", db_n.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string()));
    push(&mut out, "SP_STATUTE_PAGE_N", page_n.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string()));

    match (db_n, page_n) {
        (Some(db), Some(page)) => {
            let skew = if db == 0 {
                format!("MISMATCH:DB=0,PAGE={page}")
            } else if page > 0 && db * 2 < page {
                format!("MISMATCH:DB={db},PAGE={page}")
            } else {
                "OK".to_string()
            };
            push(&mut out, "SP_STATUTE_SKEW", skew);
        }
        _ => push(&mut out, "SP_STATUTE_SKEW", "?"),
    }
    out
}

// ---------------------------------------------------------------------------------------
// mail_keys
// ---------------------------------------------------------------------------------------

pub fn mail_keys() -> Kv {
    let mut out = Kv::new();
    let now = io::now();
    let mail_base = std::env::var("SPIRA_MAIL").unwrap_or_default();
    let dir = Path::new(&mail_base).join("concierge");
    let new_dir = dir.join("new");
    if mail_base.is_empty() || !new_dir.is_dir() {
        push(&mut out, "SP_MAIL_UNREAD", "?");
        push(&mut out, "SP_MAIL_OLDEST_AGE", "?");
        push(&mut out, "SP_MAIL_N", "0");
        return out;
    }
    let mut unread = 0;
    let mut oldest_t: Option<i64> = None;
    let mut rows: Vec<(i64, &'static str, String)> = Vec::new();
    for (sub, is_new) in [("new", true), ("cur", false)] {
        let d = dir.join(sub);
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
            if is_new {
                unread += 1;
                if oldest_t.map(|t| mtime < t).unwrap_or(true) {
                    oldest_t = Some(mtime);
                }
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let state = if is_new {
                "NEW"
            } else if fname.contains(":2,") && fname.rsplit_once(":2,").map(|(_, f)| f.contains('R')).unwrap_or(false) {
                "DONE"
            } else {
                "READ"
            };
            let subj = read_mail_subject(&path);
            rows.push((mtime, state, subj));
        }
    }
    push(&mut out, "SP_MAIL_UNREAD", unread.to_string());
    push(&mut out, "SP_MAIL_OLDEST_AGE", oldest_t.map(|t| (now - t).to_string()).unwrap_or_else(|| "-".to_string()));
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (i, (mtime, state, subj)) in rows.iter().take(5).enumerate() {
        push(&mut out, &format!("SP_MAIL{i}"), format!("'{}|{}|{}'", now - mtime, state, subj));
    }
    push(&mut out, "SP_MAIL_N", rows.len().min(5).to_string());
    out
}

fn read_mail_subject(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else { return String::new() };
    for line in content.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("subject:").map(|_| &line[8..]) {
            let mut s = sanitize(v.trim_start());
            s.truncate(60);
            return s.trim().to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------------------
// sending_keys — entirely delegated to cockpit-metrics.py, unchanged from the bash.
// ---------------------------------------------------------------------------------------

const SENDING_KEYS: &[&str] = &[
    "SP_SENT", "SP_SENT_HELD", "SP_SENT_KEPT", "SP_SENT_FAILED", "SP_SENT_FAILED_AGE_M",
    "SP_PASSES", "SP_ACTS", "SP_FALSE_ACTS", "SP_FALSE_PER_PASS", "SP_SINCE_JUDGEMENT",
    "SP_STARVED_PASSES", "SP_AEON_BORN", "SP_AEON_LIVED", "SP_AEON_STILLBORN",
    "SP_AEON_WORKED", "SP_AEON_THRASH", "SP_SELF_REPEATING_N", "SP_SELF_STILLBORN_W",
    "SP_SELF_STILLBORN_LAST", "SP_SELF_STARVED_W", "SP_SELF_STARVED_LAST",
];

pub fn sending_keys() -> Kv {
    let run = io::run_dir();
    let sentinel_log = run.join("sentinel.log");
    let ledger = run.join("aeon-ledger.log");
    match io::run_tool("cockpit-metrics.py", &[sentinel_log.to_str().unwrap_or(""), ledger.to_str().unwrap_or("")], None) {
        Some(out) => out
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        None => SENDING_KEYS.iter().map(|k| (k.to_string(), "?".to_string())).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_reports_zero_not_question_mark_on_empty() {
        // sphere_keys hits bd for real; this only checks the pure classification helper
        // it shares with the other probes stays honest about refusal vs empty.
        assert_eq!(io::bd_rows(Some("[]".to_string())), Some(vec![]));
    }

    #[test]
    fn strand_keys_parses_ghost_and_other() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let path = testkit::TempDir::new("cc-strand");
        std::env::set_var("SPIRA_RUN", path.path());
        // `escalated` is a timestamp (0 while unescalated, a nonzero epoch once raised —
        // strand.sh's own row shape), never a JSON boolean; this fixture uses the real shape.
        std::fs::write(
            path.path().join("strands.json"),
            r#"{"a:ghost:1":{"escalated":0},"b:other:2":{"escalated":1700000500}}"#,
        )
        .unwrap();
        let kv = strand_keys();
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_STRANDS"), Some("2".to_string()));
        assert_eq!(get("SP_STRANDS_ESCALATED"), Some("1".to_string()));
        assert_eq!(get("SP_STRAND_GHOST"), Some("1".to_string()));
        assert_eq!(get("SP_STRAND_OTHER"), Some("other=1".to_string()));
    }

    #[test]
    fn is_aeon_cmd_matches_the_bash_regex() {
        assert!(is_aeon_cmd("/usr/local/bin/aeon builder sp-1"));
        assert!(is_aeon_cmd("bash /release/spira/aeon.sh builder"));
        assert!(is_aeon_cmd("aeon"));
        assert!(!is_aeon_cmd("/usr/bin/claude --dangerous"));
        assert!(!is_aeon_cmd("aeon-something else"));
    }

    #[test]
    fn strand_keys_missing_file_renders_question_marks() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let path = testkit::TempDir::new("cc-strand-missing");
        std::env::set_var("SPIRA_RUN", path.path());
        let kv = strand_keys();
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_STRANDS"), Some("?".to_string()));
    }
}
