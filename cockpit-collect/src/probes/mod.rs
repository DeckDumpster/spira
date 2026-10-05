//! The probe library — one function per `spira/cockpit.sh` `*_keys` function, each producing
//! the exact `KEY=value` lines (unquoted; the supervisor's merge applies the one layer of
//! shell quoting) that function printed. DESIGN.md "Design" names the split: data fetching
//! goes through [`crate::io`] exactly as the bash did (including the `lib.sh` bridge for
//! functions this bead does not own); the sanitizing/dating/sorting that used to be inline
//! `python3 -c` is native Rust here, unit tested against literal fixtures.

mod admission;
mod core_detail;
mod lc;
mod queue;
mod ratelim;
mod reachable;
mod unsent;

use crate::io;
use crate::quoting::{sanitize, sanitize_title};
use serde_json::Value;
use spira_config::nonwork::{self, Kind, Which};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The capacity pause file's path (wave 4.26: family K's home is `aeon::capacity`, read
/// in-process here instead of shelling into lib.sh via `io::lib_call`). `SPIRA_CAPACITY_PAUSE`
/// is a lib.sh literal, never a spira-config registry key, so an operator override is read
/// straight from the environment — the same ad hoc path every other reader of this key uses.
fn capacity_pause_file(run: &Path) -> PathBuf {
    std::env::var("SPIRA_CAPACITY_PAUSE").ok().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| run.join("capacity-pause"))
}

pub type Kv = Vec<(String, String)>;

fn push(out: &mut Kv, k: &str, v: impl Into<String>) {
    out.push((k.to_string(), v.into()));
}

/// Every registered config key this probe library reads, resolved ONCE through
/// `spira_config::process::cfg`/`cfg_parse` (the one door, per Ryan 2026-10-05: one source of
/// config) and passed down to every `*_keys` function that needs a value — never read a
/// second time via `std::env::var`. Built by [`Cfg::load`], called once per `once`/`probe`
/// invocation (`main::cmd_once`/`cmd_probe`), after `io::bootstrap_config` (which still
/// exports resolved config into this process's own environment, but only so the frozen
/// `lib.sh` bridge's children see it — never as a second path back into this crate's own
/// Rust logic). `Default` exists for tests only: every field defaults to empty/zero, never a
/// value a test should mistake for a resolved one — a test sets exactly the field(s) its
/// assertion is about.
#[derive(Default)]
pub struct Cfg {
    pub instance: String,
    pub trace_lines: i64,
    pub max_live_aeons: i64,
    pub lanes_max_live: String,
    pub scope_label: String,
    pub czar_label: String,
    pub wiki: String,
    pub mail: String,
    pub queue_dir: String,
    pub express_label: String,
    pub queue_batch_max: i64,
    pub queue_batch_wait: i64,
    pub cert_window_mins: i64,
    pub prod: String,
    pub ci_label: String,
    pub queue_wait_label: String,
    /// `0` is a declared, deliberate value ("set it to 0 to disable the deadline" — conf.d's
    /// own doc), not an absence — `awaiting_ci_section` (core_detail.rs) checks for it
    /// explicitly rather than treating every elapsed second as overdue.
    pub ci_park_max: i64,
    pub ctrl: String,
    pub repo_map: String,
}

impl Cfg {
    pub fn load() -> Result<Cfg, String> {
        use spira_config::process::{cfg, cfg_parse};
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = io::home_dir();
        Ok(Cfg {
            instance: spira_config::resolve::resolve_instance(&env, &home)?,
            trace_lines: cfg_parse::<i64>("COCKPIT_TRACE_LINES")?,
            max_live_aeons: cfg_parse::<i64>("SPIRA_MAX_LIVE_AEONS")?,
            lanes_max_live: cfg("SPIRA_LANES_MAX_LIVE")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            czar_label: cfg("SPIRA_CZAR_LABEL")?,
            wiki: cfg("SPIRA_WIKI")?,
            mail: cfg("SPIRA_MAIL")?,
            queue_dir: cfg("SPIRA_QUEUE_DIR")?,
            express_label: cfg("SPIRA_EXPRESS_LABEL")?,
            queue_batch_max: cfg_parse::<i64>("SPIRA_QUEUE_BATCH_MAX")?,
            queue_batch_wait: cfg_parse::<i64>("SPIRA_QUEUE_BATCH_WAIT")?,
            cert_window_mins: cfg_parse::<i64>("SPIRA_CERT_WINDOW_MINS")?,
            prod: cfg("SPIRA_PROD")?,
            ci_label: cfg("SPIRA_CI_LABEL")?,
            queue_wait_label: cfg("SPIRA_QUEUE_WAIT_LABEL")?,
            ci_park_max: cfg_parse::<i64>("SPIRA_CI_PARK_MAX")?,
            ctrl: cfg("SPIRA_CTRL")?,
            repo_map: cfg("SPIRA_REPO_MAP")?,
        })
    }
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
pub fn full_pass(cfg: &Cfg) -> Kv {
    let start = io::now();
    let mut out = Kv::new();
    out.extend(now_keys(cfg));
    out.extend(core_detail::core_detail_keys(cfg));
    out.extend(core_counts_keys());
    out.extend(slots_keys(cfg));
    out.extend(admission::admission_keys());
    out.extend(unsent::unsent_keys(cfg));
    out.extend(queue::queue_keys(cfg));
    out.extend(reachable::reachable_keys(cfg));
    out.extend(sphere_keys(cfg));
    out.extend(repo_label_keys(cfg));
    out.extend(strand_keys());
    out.extend(dup_refs_keys());
    out.extend(livelock_keys());
    out.extend(sop_keys());
    out.extend(ratelim::ratelim_keys());
    out.extend(statute_keys(cfg));
    out.extend(drift_keys());
    out.extend(czar_triggers_keys(cfg));
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
pub fn run(subcommand: &str, cfg: &Cfg) -> Option<Kv> {
    match subcommand {
        "now" => Some(now_keys(cfg)),
        "core" => {
            let mut kv = core_detail::core_detail_keys(cfg);
            kv.extend(core_counts_keys());
            Some(kv)
        }
        "core_detail" => Some(core_detail::core_detail_keys(cfg)),
        "slots" => Some(slots_keys(cfg)),
        "admission" => Some(admission::admission_keys()),
        "unsent" => Some(unsent::unsent_keys(cfg)),
        "queue" => Some(queue::queue_keys(cfg)),
        "reachable" => Some(reachable::reachable_keys(cfg)),
        "sphere" => Some(sphere_keys(cfg)),
        "repo_labels" => Some(repo_label_keys(cfg)),
        "livelock" => Some(livelock_keys()),
        "dup_refs" => Some(dup_refs_keys()),
        "strands" => Some(strand_keys()),
        "ratelim" => Some(ratelim::ratelim_keys()),
        "sops" => Some(sop_keys()),
        "statute" => Some(statute_keys(cfg)),
        "drift" => Some(drift_keys()),
        "mail" => Some(mail_keys(cfg)),
        "czar_triggers" => Some(czar_triggers_keys(cfg)),
        "sending" => Some(sending_keys()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// now_keys — fast tier: /proc and the filesystem only, no bd/git.
// ---------------------------------------------------------------------------------------

pub fn now_keys(cfg: &Cfg) -> Kv {
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

            let seed = std::fs::read_to_string(home.join("chamber").join(format!("{fay}.fayth")))
                .ok()
                .and_then(|c| c.lines().find_map(|l| l.strip_prefix("FAYTH_MODEL=").map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())));
            let fayth_model = io::lib_call(&home, "persona_model", &[fay])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "?".to_string());
            let overridden = seed.as_deref().is_some_and(|s| !s.is_empty() && s != fayth_model) && fayth_model != "?";
            push(&mut out, &format!("SP_AEON{i}_MODEL_OVERRIDE"), if overridden { "1" } else { "0" }.to_string());
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
            let tl: i64 = cfg.trace_lines;
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

    push(&mut out, "SP_SENTINEL_TIMER", unit_active_key("sentinel", "timer", &cfg.instance));
    push(&mut out, "SP_SENTINEL_AGE", age_of(&run.join("sentinel.log")));
    push(&mut out, "SP_OPS_TIMER", unit_active_key("ops", "timer", &cfg.instance));

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

    push(&mut out, "SP_AURON_TIMER", unit_active_key("auron", "timer", &cfg.instance));
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

    let cap_now = io::now();
    match aeon::capacity::pause_state(&capacity_pause_file(&run)) {
        aeon::capacity::PauseState::Paused { until, why } if until > cap_now => {
            push(&mut out, "SP_CAPACITY_PAUSED", "1");
            push(&mut out, "SP_CAPACITY_LEFT", (until - cap_now).to_string());
            push(&mut out, "SP_CAPACITY_AT", fmt_hm(until));
            push(&mut out, "SP_CAPACITY_WHY", why);
        }
        aeon::capacity::PauseState::Unknown => {
            // Fail closed (wave4-decomposition.md (c)3): an unreadable or corrupt pause
            // file must never render as "0"/open on the dashboard — `?`, never a claim
            // this probe cannot back.
            push(&mut out, "SP_CAPACITY_PAUSED", "?");
            push(&mut out, "SP_CAPACITY_LEFT", "?");
            push(&mut out, "SP_CAPACITY_AT", "?");
            push(&mut out, "SP_CAPACITY_WHY", "?");
        }
        _ => {
            push(&mut out, "SP_CAPACITY_PAUSED", "0");
            push(&mut out, "SP_CAPACITY_LEFT", "0");
            push(&mut out, "SP_CAPACITY_AT", "");
            push(&mut out, "SP_CAPACITY_WHY", "");
        }
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

/// `spira_unit` (wave 4.10, sp-wqj3o: family C7's home is `spira_config::unit`, read
/// in-process here instead of shelling into lib.sh via `io::lib_call` — the one bash bridge
/// this bead names explicitly). `instance` is the caller's resolved `SPIRA_INSTANCE`
/// (`Cfg::load`); `SPIRA_SYSTEMCTL` is not a registered config key, so it is still read
/// straight from the environment, the same per-copy-fact reading `capacity_pause_file`
/// (above) does for its own non-registered key.
fn unit_active_key(fayth: &str, kind: &str, instance: &str) -> String {
    let systemctl = std::env::var("SPIRA_SYSTEMCTL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "systemctl".to_string());
    let unit = spira_config::unit::resolve_unit(fayth, kind, instance, &systemctl);
    match io::unit_active(&unit) {
        Some(true) => "1".to_string(),
        Some(false) => "0".to_string(),
        None => "?".to_string(),
    }
}

fn age_of(path: &Path) -> String {
    io::mtime_age_secs(path).map(|s| s.to_string()).unwrap_or_else(|| "?".to_string())
}

/// `aeon_alive` (`lib.sh`): the one canonical implementation (`strand::probe::aeon_alive`,
/// wave 4.23 sp-0ffox — "collapsing the bead/cockpit-collect copies"), called in-process —
/// a Rust-to-Rust call costs nothing extra over this crate's own former copy, unlike the
/// `bash`-bridge round trip per pidfile this probe still avoids for the `lib.sh` helpers
/// that do real work (this one was always three token comparisons).
fn aeon_alive(pidfile: &Path) -> bool {
    strand::probe::aeon_alive(pidfile)
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
    let ask_label = spira_config::resolve::key_for_process("SPIRA_ASK_LABEL").unwrap_or_default(); // the configured ask label; never a literal fallback (literal-lint ask_fallback)
    // Asks are not work beads: bd status is their only state (spira_config::nonwork).
    let [flag, open] = nonwork::status_args(Kind::Ask, Which::Open);
    let rows = io::bd_rows(io::bdjson(&["list", &flag, &open, "--limit", "0", "--label", &ask_label]));
    match rows {
        None => push(&mut out, "SP_WAITING", "?"),
        Some(rows) => push(&mut out, "SP_WAITING", rows.len().to_string()),
    }
    out
}

// ---------------------------------------------------------------------------------------
// slots_keys
// ---------------------------------------------------------------------------------------

pub fn slots_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let home = io::home_dir();
    let live = io::lib_call(&home, "aeons_live_total", &[]).unwrap_or_else(|| "?".to_string());
    push(&mut out, "SP_SLOTS_LIVE", live.clone());

    let pool: i64 = io::max_aeons().parse().ok().unwrap_or(0);
    // SPIRA_MAX_LIVE_AEONS now carries a real, always-declared value (the config file); the
    // pool+lanes computation this ceiling used to fall back to when the key resolved empty
    // is gone with that default.
    let ceiling = cfg.max_live_aeons;
    push(&mut out, "SP_SLOTS_CEILING", ceiling.to_string());

    // A failed live read must never resolve to a reassuring free count.
    let free = match live.trim().parse::<i64>() {
        Ok(l) => (ceiling - l).max(0).to_string(),
        Err(_) => "?".to_string(),
    };
    push(&mut out, "SP_SLOTS_FREE", free);
    push(&mut out, "SP_SLOTS_POOL", pool.to_string());
    push(&mut out, "SP_SLOTS_LANES_CAP", cfg.lanes_max_live.clone());

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

    let run = io::run_dir();
    let cap_now = io::now();
    let capacity_paused = match aeon::capacity::pause_state(&capacity_pause_file(&run)) {
        aeon::capacity::PauseState::Paused { until, .. } if until > cap_now => "1",
        // Fail closed: never "0"/open on an unreadable file (wave4-decomposition.md (c)3).
        aeon::capacity::PauseState::Unknown => "?",
        _ => "0",
    };
    push(&mut out, "SP_SLOTS_CAPACITY_PAUSED", capacity_paused);
    out
}

// ---------------------------------------------------------------------------------------
// sphere_keys
// ---------------------------------------------------------------------------------------

pub fn sphere_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    // A work bead's state is the lifecycle row's (design §3.4); `None` renders `?`.
    let lc_rows = lc::state_index();
    match &lc_rows {
        None => push(&mut out, "SP_POISON", "?"),
        Some(lc) => push(&mut out, "SP_POISON", poison_count(lc).to_string()),
    }

    let scope = &cfg.scope_label;
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let ask = spira_config::resolve::key_for_process("SPIRA_ASK_LABEL").unwrap_or_default(); // the configured ask label; never a literal fallback (literal-lint ask_fallback)
    let rows = io::bd_rows(io::bdjson(&["list", "--limit", "0", "--label", &label]));
    match rows.zip(lc_rows.as_ref()) {
        None => {
            push(&mut out, "SP_OPEN", "?");
            push(&mut out, "SP_INPROG", "?");
            push(&mut out, "SP_NEEDSOP", "?");
        }
        Some((rows, lc)) => {
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
            let (open_n, inprog_n, needsop_n) = sphere_counts(&work, lc, &ask);
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

pub fn repo_label_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let Ok(map_content) = std::fs::read_to_string(&cfg.repo_map) else {
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
    // An incident bead is a work bead (sp-jgjvh): whether it is over is its lifecycle row's.
    let Some((rows, lc)) = rows.zip(lc::state_index()) else {
        push(&mut out, "SP_DUP_REFS", "?");
        push(&mut out, "SP_DUP_BEADS", "?");
        push(&mut out, "SP_DUP_N", "0");
        return out;
    };
    let by_ref = incidents_by_ref(&rows, &lc, &since);
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
// livelock_keys — delegates the detection itself to `strand::detectors` (wave 4.29,
// sp-8ofmt: `detect_livelocked`, `detect_invalid_closed`, called in-process instead of
// through the `lib.sh` bridge); this only classifies refusal-vs-empty and formats rows.
// ---------------------------------------------------------------------------------------

/// `strand::config::Config`, resolved fresh per call — a probe call is already a fresh
/// process-equivalent pass (DESIGN.md: data fetching goes through `io` each time), and
/// `groomer`/`maechen-trigger` resolve their own copy the same independent way.
fn strand_cfg() -> strand::config::Config {
    strand::config::Config::resolve(&strand::config::Live::load()).unwrap_or_else(|e| {
        eprintln!("cockpit-collect: {e}");
        std::process::exit(1)
    })
}

pub fn livelock_keys() -> Kv {
    let mut out = Kv::new();
    let cfg = strand_cfg();
    let ll_out = strand::detectors::detect_livelocked(&cfg);
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

    // `detect_invalid_closed` emits three row shapes: `INVALID-CLOSED <id> — ...`,
    // `UNFILED-FOLLOW <id> — ...` and `ALLOWED-IC <id> — ...` (a bead on
    // $SPIRA_RUN/invalid-closed.allow, reported but not counted as either). None of the
    // three caps at 20 rows the way LIVELOCK above does — the bash's own cap check there is
    // `[ "$_n" -ge 20 ] && true`, which never breaks — so every matching row is emitted.
    let ic_out = strand::detectors::detect_invalid_closed(&cfg);
    // An empty detector answer is only "none" when the store answers at all.
    if ic_out.is_empty() && io::bdjson(&["list", "--all", "--limit", "1"]).is_none() {
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

pub fn czar_triggers_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let label = cfg.czar_label.as_str();
    let raw = io::bdjson(&["list", "--label", label, "--all", "--limit", "0", "--brief"]);
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
    let lc_rows = lc::state_index();
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
        let outcome = match czar_progress(newest, lc_rows.as_ref()) {
            CzarProgress::Pending => "pending".to_string(),
            CzarProgress::Unknown => "-".to_string(),
            CzarProgress::Finished => {
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
            }
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

const SOP_LEDGER_REL: &str = "sop/applied.jsonl";

pub fn sop_keys() -> Kv {
    let mut out = Kv::new();
    let ledger = std::env::var("SPIRA_SOP_LEDGER").unwrap_or_else(|_| io::run_dir().join(SOP_LEDGER_REL).to_string_lossy().into_owned());
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

pub fn statute_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let wiki = &cfg.wiki;
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
// drift_keys
// ---------------------------------------------------------------------------------------

/// `OK`, `<dirty>:<n>` counting `finding_prefix` lines, or `?` when `drift.sh` could not run.
pub fn drift_value(code: Option<i32>, out: &str, dirty: &str, finding_prefix: &str) -> String {
    match code {
        Some(0) => "OK".to_string(),
        Some(1) => format!("{dirty}:{}", out.lines().filter(|l| l.starts_with(finding_prefix)).count()),
        _ => "?".to_string(),
    }
}

pub fn drift_keys() -> Kv {
    let drift_sh = io::home_dir().join("drift.sh");
    let repo = std::env::var("SPIRA_REPO").ok().filter(|s| !s.is_empty());
    let run_one = |args: &[&str]| {
        std::process::Command::new("bash")
            .envs(spira_config::release_env::child_path_env_for_process())
            .arg(&drift_sh)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| (o.status.code(), String::from_utf8_lossy(&o.stdout).into_owned()))
            .unwrap_or((None, String::new()))
    };
    let mut out = Kv::new();
    let mut co_args = vec!["checkout"];
    if let Some(r) = repo.as_deref() {
        co_args.push(r);
    }
    let (rc, text) = run_one(&co_args);
    push(&mut out, "SP_CHECKOUT_DRIFT", drift_value(rc, &text, "DIRTY", "DIRTY "));
    let (rc, text) = run_one(&["units"]);
    push(&mut out, "SP_UNIT_DRIFT", drift_value(rc, &text, "UNSHIPPED", "UNSHIPPED "));
    out
}

// ---------------------------------------------------------------------------------------
// mail_keys
// ---------------------------------------------------------------------------------------

pub fn mail_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let now = io::now();
    let mail_base = &cfg.mail;
    let dir = Path::new(mail_base).join("concierge");
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

/// SP_DUP_*: incident bead ids grouped by `external_ref`, over the incidents that still
/// count — read from each bead's lifecycle row, never bd status (sp-jgjvh). Unfinished or
/// in delivery (not terminal) counts; a terminal one counts only while its `closed_at`
/// (bd content) falls inside the dedup lookback (`since`, `YYYY-MM-DD`); a bead with no
/// lifecycle row is not live work and is not counted. A `duplicate-of:` bead is already
/// accounted for.
pub fn incidents_by_ref(rows: &[Value], lc: &HashMap<String, lc::Row>, since: &str) -> HashMap<String, Vec<String>> {
    let mut by_ref: HashMap<String, Vec<String>> = HashMap::new();
    for i in rows {
        let ref_ = i.get("external_ref").and_then(Value::as_str).unwrap_or("");
        if ref_.is_empty() {
            continue;
        }
        let labels: Vec<&str> = i.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if labels.iter().any(|l| l.starts_with("duplicate-of:")) {
            continue;
        }
        let id = i.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(row) = lc.get(&id) else { continue };
        if row.terminal() {
            let closed_at = i.get("closed_at").and_then(Value::as_str).unwrap_or("");
            let closed_date = closed_at.get(..10).unwrap_or("");
            if closed_date < since {
                continue;
            }
        }
        by_ref.entry(ref_.to_string()).or_default().push(id);
    }
    by_ref
}

/// SP_POISON: work beads the machine holds for poison, not yet over (the `spira-poison`
/// label is retired; holds replace it, design §3.4).
pub fn poison_count(lc: &HashMap<String, lc::Row>) -> usize {
    lc.values().filter(|r| r.held("poison") && !r.terminal()).count()
}

/// SP_OPEN / SP_INPROG / SP_NEEDSOP over the plan's work items: a work bead's state is its
/// lifecycle row's — open is "not over" (not terminal), in progress is WORKING. An epic has
/// no lifecycle row: it is a coordination bead, whose bd status is its only state
/// (spira_config::nonwork). Any other item with no row is not counted: the machine cannot
/// tell its state, and a rowless bead can never be claimed (CHECK-ROWLESS reports it).
pub fn sphere_counts(work: &[&Value], lc: &HashMap<String, lc::Row>, ask: &str) -> (usize, usize, usize) {
    let has = |i: &Value, lab: &str| {
        i.get("labels")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|l| l.as_str() == Some(lab)))
            .unwrap_or(false)
    };
    let (mut open_n, mut inprog_n, mut needsop_n) = (0, 0, 0);
    for i in work {
        let id = i.get("id").and_then(Value::as_str).unwrap_or("");
        let (open, working) = match lc.get(id) {
            Some(r) => (!r.terminal(), r.working()),
            None if i.get("issue_type").and_then(Value::as_str) == Some("epic") => {
                let st = nonwork::status_of(Kind::Epic, i);
                (!nonwork::is_closed(Kind::Epic, st), nonwork::is_in_progress(Kind::Epic, st))
            }
            None => (false, false),
        };
        open_n += usize::from(open);
        inprog_n += usize::from(working);
        needsop_n += usize::from(open && has(i, ask));
    }
    (open_n, inprog_n, needsop_n)
}

#[derive(Debug, PartialEq, Eq)]
pub enum CzarProgress {
    Pending,
    Finished,
    Unknown,
}

/// A czar trigger is a work bead the czar persona claims: pending while it waits for or holds
/// a builder, finished once the builder handed it on (`past_builder`), unknown with no
/// lifecycle row or no machine answer.
pub fn czar_progress(bead: &Value, lc: Option<&HashMap<String, lc::Row>>) -> CzarProgress {
    let id = bead.get("id").and_then(Value::as_str).unwrap_or("");
    match lc.and_then(|m| m.get(id)) {
        Some(r) if r.past_builder() => CzarProgress::Finished,
        Some(r) if !r.state.is_empty() => CzarProgress::Pending,
        _ => CzarProgress::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcmap(rows: &[(&str, &str, &[&str])]) -> HashMap<String, lc::Row> {
        rows.iter()
            .map(|(id, st, holds)| {
                (id.to_string(), lc::Row { bead_id: id.to_string(), state: st.to_string(), holds: holds.iter().map(|h| h.to_string()).collect(), ..Default::default() })
            })
            .collect()
    }

    /// sp-jgjvh: duplicate incident refs are counted from the lifecycle row: live and in
    /// delivery count, terminal counts only inside the lookback, rowless never.
    #[test]
    fn dup_refs_count_by_lifecycle_state_not_bd_status() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"a","status":"closed","external_ref":"r1"},
                {"id":"b","status":"open","external_ref":"r1"},
                {"id":"c","status":"open","external_ref":"r1","closed_at":"2026-01-01T00:00:00Z"},
                {"id":"d","status":"open","external_ref":"r1","closed_at":"2026-10-04T00:00:00Z"},
                {"id":"e","status":"open","external_ref":"r1"},
                {"id":"f","status":"open","external_ref":"r1","labels":["duplicate-of:a"]}]"#,
        )
        .unwrap();
        let lc = lcmap(&[("a", "WORKING", &[]), ("b", "SUBMITTED", &[]), ("c", "LANDED", &[]), ("d", "DONE", &[]), ("f", "READY", &[])]);
        let mut got = incidents_by_ref(&rows, &lc, "2026-09-28").remove("r1").unwrap();
        got.sort();
        assert_eq!(got, vec!["a", "b", "d"]);
    }

    /// sp-mve9i: the counts follow the lifecycle row, whatever bd's status says.
    #[test]
    fn sphere_counts_read_the_lifecycle_state_not_bd_status() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"a","status":"closed","issue_type":"task","labels":["ask"]},
                {"id":"b","status":"open","issue_type":"task"},
                {"id":"c","status":"in_progress","issue_type":"bug"},
                {"id":"d","status":"open","issue_type":"task"},
                {"id":"e","status":"open","issue_type":"epic"}]"#,
        )
        .unwrap();
        let work: Vec<&Value> = rows.iter().collect();
        let lc = lcmap(&[("a", "REWORK", &[]), ("b", "LANDED", &[]), ("c", "SUBMITTED", &[])]);
        // a: open (REWORK) and needs-op; b: over; c: open, not WORKING; d: rowless, not
        // counted; e: an epic, open by bd.
        assert_eq!(sphere_counts(&work, &lc, "ask"), (3, 0, 1));
        let lc = lcmap(&[("d", "WORKING", &[])]);
        assert_eq!(sphere_counts(&work, &lc, "ask"), (2, 1, 0));
    }

    #[test]
    fn poison_counts_live_poison_holds() {
        let lc = lcmap(&[("a", "REWORK", &["poison"]), ("b", "LANDED", &["poison"]), ("c", "READY", &["wait"])]);
        assert_eq!(poison_count(&lc), 1);
    }

    #[test]
    fn czar_outcome_follows_the_lifecycle_row() {
        let bead: Value = serde_json::from_str(r#"{"id":"z","status":"closed"}"#).unwrap();
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[("z", "WORKING", &[])]))), CzarProgress::Pending);
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[("z", "DONE", &[])]))), CzarProgress::Finished);
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[]))), CzarProgress::Unknown);
        assert_eq!(czar_progress(&bead, None), CzarProgress::Unknown);
    }

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
        let _env = crate::test_support::set_run(path.path());
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
    fn aeon_alive_delegates_to_strands_canonical_predicate() {
        // Wave 4.23 (sp-0ffox) retired this crate's own cmdline check; the positive/negative
        // controls for the predicate now live with strand's own suite. This just confirms
        // the probe reaches it rather than a local reimplementation.
        let dir = testkit::TempDir::new("cockpit-collect-aeon-alive");
        let pf = dir.join("x.pid");
        std::fs::write(&pf, "999999999").unwrap();
        assert!(!aeon_alive(&pf), "a pid that does not exist is never alive");
    }

    fn strand_kv(json: &str) -> impl Fn(&str) -> Option<String> {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let path = testkit::TempDir::new("cc-strand-case");
        let _env = crate::test_support::set_run(path.path());
        std::fs::write(path.path().join("strands.json"), json).unwrap();
        let kv = strand_keys();
        move |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn strand_keys_counts_two_ghosts_as_two_and_reports_no_other_class() {
        let get = strand_kv(r#"{"spira,plan:ghost:sp-a":{},"spira,plan:ghost:sp-b":{}}"#);
        assert_eq!(get("SP_STRAND_GHOST"), Some("2".into()));
        assert_eq!(get("SP_STRAND_OTHER"), Some("none".into()));
    }

    #[test]
    fn strand_keys_an_empty_epic_is_its_own_class_not_a_ghost() {
        let get = strand_kv(r#"{"spira,plan:empty:sp-jj88":{"first":1788811865,"acted":0,"escalated":1788812834}}"#);
        assert_eq!(get("SP_STRAND_GHOST"), Some("0".into()));
        assert_eq!(get("SP_STRAND_OTHER"), Some("empty=1".into()));
        assert_eq!(get("SP_STRANDS"), Some("1".into()));
    }

    #[test]
    fn strand_keys_itemises_every_other_class_and_keeps_ghost_apart() {
        let get = strand_kv(r#"{"p:ghost:sp-a":{},"p:empty:sp-b":{},"p:empty:sp-c":{},"p:stuck:sp-d":{}}"#);
        assert_eq!(get("SP_STRAND_GHOST"), Some("1".into()));
        assert_eq!(get("SP_STRAND_OTHER"), Some("empty=2,stuck=1".into()));
    }

    #[test]
    fn strand_keys_splits_from_the_right_so_a_partition_may_hold_a_colon() {
        let get = strand_kv(r#"{"spira:plan,extra:ghost:sp-a":{}}"#);
        assert_eq!(get("SP_STRAND_GHOST"), Some("1".into()));
    }

    #[test]
    fn strand_keys_an_unclassifiable_key_makes_ghost_unknown_and_is_itself_reported() {
        let get = strand_kv(r#"{"bogus":{},"p:ghost:sp-a":{}}"#);
        assert_eq!(get("SP_STRAND_GHOST"), Some("?".into()));
        assert_eq!(get("SP_STRAND_OTHER"), Some("unclassified=1".into()));
        assert_eq!(get("SP_STRANDS"), Some("2".into()));
    }

    #[test]
    fn strand_keys_an_unparsable_ledger_is_unread_not_empty() {
        let get = strand_kv("not json at all");
        assert_eq!(get("SP_STRAND_GHOST"), Some("?".into()));
    }

    #[test]
    fn strand_keys_missing_file_renders_question_marks() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let path = testkit::TempDir::new("cc-strand-missing");
        let _env = crate::test_support::set_run(path.path());
        let kv = strand_keys();
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_STRANDS"), Some("?".to_string()));
    }
}

#[cfg(test)]
mod drift_tests {
    use super::drift_value;

    #[test]
    fn a_probe_that_could_not_run_is_never_read_as_clean() {
        assert_eq!(drift_value(Some(0), "", "DIRTY", "DIRTY "), "OK");
        assert_eq!(drift_value(Some(1), "DIRTY a\nDIRTY b\nnote\n", "DIRTY", "DIRTY "), "DIRTY:2");
        assert_eq!(drift_value(Some(3), "", "DIRTY", "DIRTY "), "?");
        assert_eq!(drift_value(None, "", "DIRTY", "DIRTY "), "?");
    }
}
