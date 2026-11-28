//! The probe library — one function per `spira/cockpit.sh` `*_keys` function, each producing
//! the exact `KEY=value` lines (unquoted; the supervisor's merge applies the one layer of
//! shell quoting) that function printed. DESIGN.md "Design" names the split: data fetching
//! goes through [`crate::io`] exactly as the bash did (including the `lib.sh` bridge for
//! functions this bead does not own); the sanitizing/dating/sorting that used to be inline
//! `python3 -c` is native Rust here, unit tested against literal fixtures.

mod admission;
mod core_detail;
mod goodput;
mod inflight;
pub mod lc;
pub mod queue;
mod ratelim;
mod reachable;
pub mod round;
mod tokens;
mod tsd;
mod unsent;

use crate::io;
use crate::quoting::{sanitize, sanitize_title};
use serde_json::Value;
use spira_config::nonwork::{self, Kind};
use spira_config::scheduler_link::Limit;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The capacity pause file's path (wave 4.26: family K's home is `dwarf::capacity`, read
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
    pub max_live_dwarves: i64,
    pub scope_label: String,
    pub czar_label: String,
    pub wiki: String,
    pub mail: String,
    pub queue_batch_max: i64,
    pub queue_batch_wait: i64,
    pub batched_stranded_grace: i64,
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
    pub round_cap_secs: i64,
}

impl Cfg {
    pub fn load() -> Result<Cfg, String> {
        use spira_config::process::{cfg, cfg_parse};
        Ok(Cfg {
            instance: spira_config::process::instance()?,
            trace_lines: cfg_parse::<i64>("COCKPIT_TRACE_LINES")?,
            max_live_dwarves: cfg_parse::<i64>("SPIRA_MAX_LIVE_DWARVES")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            czar_label: cfg("SPIRA_CZAR_LABEL")?,
            wiki: cfg("SPIRA_WIKI")?,
            mail: cfg("SPIRA_MAIL")?,
            queue_batch_max: cfg_parse::<i64>("SPIRA_QUEUE_BATCH_MAX")?,
            queue_batch_wait: cfg_parse::<i64>("SPIRA_QUEUE_BATCH_WAIT")?,
            batched_stranded_grace: cfg_parse::<i64>("SPIRA_BATCHED_STRANDED_GRACE_SECS")?,
            cert_window_mins: cert_window_mins(&cfg("SPIRA_CERT_WINDOW_MINS")?)?,
            prod: cfg("SPIRA_PROD")?,
            ci_label: cfg("SPIRA_CI_LABEL")?,
            queue_wait_label: cfg("SPIRA_QUEUE_WAIT_LABEL")?,
            ci_park_max: cfg_parse::<i64>("SPIRA_CI_PARK_MAX")?,
            ctrl: cfg("SPIRA_CTRL")?,
            repo_map: cfg("SPIRA_REPO_MAP")?,
            round_cap_secs: cfg_parse::<i64>("SPIRA_ROUND_CERTIFY_WALL_SECS")?,
        })
    }
}

const CERT_WINDOW_MINS_DEFAULT: i64 = 90;

/// An empty value is "unset": one key read by a single probe must not fail the load for all.
fn cert_window_mins(raw: &str) -> Result<i64, String> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok(CERT_WINDOW_MINS_DEFAULT);
    }
    v.parse::<i64>().map_err(|e| format!("SPIRA_CERT_WINDOW_MINS = {raw:?} does not parse: {e}"))
}

#[cfg(test)]
mod cert_window_tests {
    use super::*;

    #[test_id(t0zvgaz)]
    #[test]
    fn empty_is_the_default_and_a_value_is_parsed() {
        assert_eq!(cert_window_mins("").unwrap(), 90);
        assert_eq!(cert_window_mins("  ").unwrap(), 90);
        assert_eq!(cert_window_mins("45").unwrap(), 45);
        assert!(cert_window_mins("abc").is_err());
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
    out.extend(tsd::tsd_keys());
    out.extend(unsent::unsent_keys(cfg));
    out.extend(queue::queue_keys(cfg));
    out.extend(round::round_keys(cfg.round_cap_secs));
    out.extend(inflight::inflight_keys(cfg));
    out.extend(goodput::goodput_keys());
    out.extend(reachable::reachable_keys(cfg));
    out.extend(well_keys(cfg));
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

/// Dispatch by the same subcommand names `spira/cockpit.sh`'s case statement used.
pub fn run(subcommand: &str, cfg: &Cfg) -> Option<Kv> {
    match subcommand {
        "now" => Some(now_keys(cfg)),
        "core" => Some(core_counts_keys()),
        "core_detail" => Some(core_detail::core_detail_keys(cfg)),
        "slots" => Some(slots_keys(cfg)),
        "admission" => Some(admission::admission_keys()),
        "tsd" => Some(tsd::tsd_keys()),
        "unsent" => Some(unsent::unsent_keys(cfg)),
        "queue" => Some(queue::queue_keys(cfg)),
        "round" => Some(round::round_keys(cfg.round_cap_secs)),
        "inflight" => Some(inflight::inflight_keys(cfg)),
        "goodput" => Some(goodput::goodput_keys()),
        "reachable" => Some(reachable::reachable_keys(cfg)),
        "well" => Some(well_keys(cfg)),
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
        "hel" => Some(hel_keys()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// now_keys — fast tier: /proc and the filesystem only, no bd/git.
// ---------------------------------------------------------------------------------------

/// The keys every pass opens with: when it began, and the window its counts cover. `SP_AT`
/// comes first so the snapshot's age bounds the oldest reading in it.
fn stamp_keys(start: i64) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_AT", start.to_string());
    push(&mut out, "SP_WINDOW_HOURS", std::env::var("SPIRA_COCKPIT_WINDOW_HOURS").unwrap_or_else(|_| "24".to_string()));
    out
}

/// The `dwarf-*.pid` and `cockpit-dwarf-*.cache` paths in the run directory, from one listing.
#[derive(Default)]
struct DwarfFiles {
    pids: Vec<PathBuf>,
    caches: Vec<PathBuf>,
}

fn list_dwarf_files(run: &Path) -> DwarfFiles {
    let mut found = DwarfFiles::default();
    for e in std::fs::read_dir(run).into_iter().flatten().flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("dwarf-") && name.ends_with(".pid") {
            found.pids.push(e.path());
        } else if name.starts_with("cockpit-dwarf-") && name.ends_with(".cache") {
            found.caches.push(e.path());
        }
    }
    found.pids.sort();
    found
}

pub fn now_keys(cfg: &Cfg) -> Kv {
    now_keys_with(cfg, &io::run_dir(), &list_dwarf_files)
}

fn now_keys_with(cfg: &Cfg, run: &Path, list: &dyn Fn(&Path) -> DwarfFiles) -> Kv {
    let start = io::now();
    let mut out = stamp_keys(start);

    let home = io::home_dir();
    let run = run.to_path_buf();
    let mut i = 0usize;
    let files = list(&run);
    let mut live_dwarves = 0;
    {
        for pf in &files.pids {
            if !dwarf_alive(pf) {
                continue;
            }
            live_dwarves += 1;
            let base = pf.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            // base = dwarf-<rune>-<bead>
            let rest = base.strip_prefix("dwarf-").unwrap_or("");
            let (fay, bead) = rest.split_once('-').unwrap_or(("", rest));
            let st = dwarf_static(&run, &home, bead, fay, pf);
            let pid = io::read_trim(pf).and_then(|s| s.trim().parse::<i64>().ok());
            let secs = pid.and_then(io::proc_etimes).unwrap_or(0);

            push(&mut out, &format!("SP_DWARF{i}_PRI"), st.pri.clone());
            push(&mut out, &format!("SP_DWARF{i}_PARTITION"), st.partition.clone());
            push(&mut out, &format!("SP_DWARF{i}_TITLE"), st.title.clone());
            push(&mut out, &format!("SP_DWARF{i}_NAME"), st.name.clone());
            push(&mut out, &format!("SP_DWARF{i}_RUNE"), if fay.is_empty() { "?".into() } else { fay.to_string() });

            let seed = std::fs::read_to_string(home.join("runes").join(format!("{fay}.rune")))
                .ok()
                .and_then(|c| c.lines().find_map(|l| l.strip_prefix("RUNE_MODEL=").map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())));
            let rune_model = st.rune_model.clone();
            let repo_name = st.repo.clone();
            let overridden = seed.as_deref().is_some_and(|s| !s.is_empty() && s != rune_model) && rune_model != "?";
            push(&mut out, &format!("SP_DWARF{i}_MODEL_OVERRIDE"), if overridden { "1" } else { "0" }.to_string());
            push(&mut out, &format!("SP_DWARF{i}_RUNE_MODEL"), rune_model);
            push(&mut out, &format!("SP_DWARF{i}_BEAD"), bead);
            push(&mut out, &format!("SP_DWARF{i}_MIN"), (secs / 60).to_string());

            // dwarf_fuse_minutes (wave 4.34, sp-27d3d): ported to dwarf::trace, called
            // in-process. `base` (family W, base refs) is in-process too now (sp-k6lku,
            // "wave 4.13") through the same `io::repo_registry()` unsent.rs/queue.rs
            // already use, not the generic lib.sh bridge — the commit-ahead timestamp and
            // everything else is native.
            let wt = run.join("worktree").join(bead);
            let commit_ahead_ts = spira_config::repos::landref(&io::repo_registry(), &repo_name)
                .filter(|b| !b.is_empty())
                .and_then(|base| commit_ahead_ts(&wt, &base));
            let fuse = dwarf::trace::dwarf_fuse_minutes(bead, &wt, &run, commit_ahead_ts, io::now());
            push(&mut out, &format!("SP_DWARF{i}_FUSE"), fuse);
            push(&mut out, &format!("SP_DWARF{i}_WALL"), (secs / 60).to_string());

            let lease = dwarf::trace::dwarf_lease_minutes(bead, &run, io::now());
            push(&mut out, &format!("SP_DWARF{i}_LEASE"), lease);

            let mark = spira_config::process::cfg("SPIRA_TRACE_MARK").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "=== spira attempt".to_string());
            let log_path = run.join(format!("{bead}.log"));
            let stats = dwarf::trace::trace_stats(&log_path, &mark, io::now());
            for line in stats.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    push(&mut out, &format!("SP_DWARF{i}_{k}"), v.to_string());
                }
            }
            let tl: i64 = cfg.trace_lines;
            if tl > 0 {
                let tail = dwarf::trace::trace_tail(&log_path, &mark, tl as usize);
                let lines: Vec<&str> = tail.lines().map(sanitize_line).collect();
                let lines: Vec<&str> = lines.into_iter().filter(|l| !l.is_empty()).collect();
                let n = lines.len();
                let start_n = n.saturating_sub(tl as usize);
                for (j, l) in lines[start_n..].iter().enumerate() {
                    push(&mut out, &format!("SP_DWARF{i}_ACT{j}"), l.to_string());
                }
            }
            i += 1;
        }
    }
    push(&mut out, "SP_DWARF_N", i.to_string());
    sweep_dwarf_static(&files.caches, io::now());

    for (key, value) in loop_pulse(scheduler_status()) {
        push(&mut out, key, value);
    }

    let (firing, keys) = heimdall_alerts(lc::verb_read(&["condition", "list", "--owner", "heimdall"]).ok());
    push(&mut out, "SP_HEIMDALL_FIRING", firing);
    push(&mut out, "SP_HEIMDALL_KEYS", keys);

    let (rs_line, rs_alert) = hotfix_lines();
    push(&mut out, "SP_HOTFIX_LINE", rs_line);
    push(&mut out, "SP_HOTFIX_ALERT", rs_alert);

    push(&mut out, "SP_LC_STALE_S", lc::stale_age().to_string());
    if lc::not_running() {
        push(&mut out, "SP_LC_NOT_RUNNING", "1");
    }

    let overrides_dir = spira_config::process::cfg("SPIRA_OVERRIDES").unwrap_or_default();
    let (ov_n, ov_failed, ov_list) = overrides_summary(Path::new(&overrides_dir));
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

    push(&mut out, "SP_DWARVES", live_dwarves.to_string());

    let cap_now = io::now();
    match dwarf::capacity::pause_state(&capacity_pause_file(&run), cap_now) {
        dwarf::capacity::PauseState::Paused { until, why } => {
            push(&mut out, "SP_CAPACITY_PAUSED", "1");
            push(&mut out, "SP_CAPACITY_LEFT", (until - cap_now).to_string());
            push(&mut out, "SP_CAPACITY_AT", fmt_hm(until));
            push(&mut out, "SP_CAPACITY_WHY", why);
        }
        dwarf::capacity::PauseState::Unknown => {
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
    dwarf::util::local_hm(epoch).unwrap_or_default()
}

type AheadMemo = (String, String, Option<i64>);

/// The newest commit time past `base` in the checkout at `wt`, asked of git only when `HEAD` or the
/// base moved since the last answer, or either cannot be read from the ref files.
fn commit_ahead_ts(wt: &Path, base: &str) -> Option<i64> {
    let keys = spira_config::git_refs::head_sha(wt).zip(spira_config::git_refs::sha_of(wt, base));
    commit_ahead_memo(wt, keys, || io::git(wt, &["log", "--format=%ct", "-1", &format!("{base}..HEAD")]).and_then(|t| t.trim().parse::<i64>().ok()))
}

fn commit_ahead_memo(wt: &Path, keys: Option<(String, String)>, ask: impl FnOnce() -> Option<i64>) -> Option<i64> {
    static MEMO: std::sync::Mutex<Option<HashMap<PathBuf, AheadMemo>>> = std::sync::Mutex::new(None);
    if let Some((head, base_sha)) = &keys {
        let memo = MEMO.lock().unwrap();
        if let Some((h, b, ts)) = memo.as_ref().and_then(|m| m.get(wt)) {
            if h == head && b == base_sha {
                return *ts;
            }
        }
    }
    let ts = ask();
    let mut memo = MEMO.lock().unwrap();
    let memo = memo.get_or_insert_with(Default::default);
    match keys {
        Some((head, base_sha)) => memo.insert(wt.to_path_buf(), (head, base_sha, ts)),
        None => memo.remove(wt),
    };
    ts
}

/// (count, comma-joined keys) of the heimdall conditions the store holds as firing; `?` when it did not answer.
fn heimdall_alerts(reply: Option<String>) -> (String, String) {
    let rows = reply.and_then(|r| serde_json::from_str::<Value>(&r).ok()).and_then(|v| v["conditions"].as_array().cloned());
    let Some(rows) = rows else { return ("?".into(), String::new()) };
    let keys: Vec<&str> = rows.iter().filter(|c| c["status"] == "True").filter_map(|c| c["key"].as_str()).collect();
    (keys.len().to_string(), keys.join(","))
}

/// The `RUNNING UNLANDED` and `ALERT` lines of `release status`, read in-process.
fn hotfix_lines() -> (String, String) {
    let cfg = release::config::Config::resolve(&release::config::Flags::default(), &release::config::process_env());
    cfg.ok().map(|c| hotfix_lines_of(&c)).unwrap_or_default()
}

fn hotfix_lines_of(cfg: &release::config::Config) -> (String, String) {
    let status = release::activate::status(cfg).unwrap_or_default();
    let pick = |prefix: &str| status.lines().find(|l| l.starts_with(prefix)).unwrap_or("").to_string();
    (pick("RUNNING UNLANDED "), pick("ALERT "))
}

/// (active + failed count, failed count, `name:bead,…`) over the override specs in `dir`;
/// retired specs are skipped. A spec is failed when `.state/<name>.failed` exists.
/// The value a spec's `NAME=value` line assigns: the quoted text, or the first word unquoted.
fn spec_value(v: &str) -> String {
    let v = v.trim();
    match v.chars().next() {
        Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or("").to_string(),
        _ => v.split_whitespace().next().unwrap_or("").to_string(),
    }
}

fn overrides_summary(dir: &Path) -> (usize, usize, String) {
    let specs = |d: &Path| -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.strip_suffix(".override")?.to_string();
                let body = std::fs::read_to_string(e.path()).unwrap_or_default();
                let bead = body
                    .lines()
                    .rev()
                    .find_map(|l| l.strip_prefix("BEAD=").map(spec_value))
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| "?".to_string());
                Some((name, bead))
            })
            .collect();
        found.sort();
        found
    };
    let mut failed = 0;
    let mut list = Vec::new();
    for (name, bead) in specs(dir) {
        if dir.join(".state").join(format!("{name}.failed")).is_file() {
            failed += 1;
        }
        list.push(format!("{name}:{bead}"));
    }
    (list.len(), failed, list.join(","))
}

#[allow(clippy::type_complexity)]
pub(super) fn gate_run_scan(run: &Path) -> (Vec<(String, String, String, String)>, usize, usize) {
    gate_scan_with(run, io::now(), &|pid| io::proc_exists(pid).then(|| io::proc_cmdline(pid).unwrap_or_default()))
}

/// `argv_of` answers the process's command line, or `None` when no such process is alive.
#[allow(clippy::type_complexity)]
fn gate_scan_with(run: &Path, now: i64, argv_of: &dyn Fn(i64) -> Option<String>) -> (Vec<(String, String, String, String)>, usize, usize) {
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
            let Some(cmd) = argv_of(pid) else { continue };
            if !cmd.contains("gate") {
                continue;
            }
            live += 1;
            let started: Option<i64> = io::read_trim(&gd.join("started")).and_then(|s| s.parse().ok());
            let age = started.map(|s| (now - s).to_string()).unwrap_or_else(|| "?".to_string());
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

/// Liveness keys the loop's readers consume, each fed by the runtime task that does the work.
const LOOP_PULSE: [(&str, &str, &str); 3] = [
    ("SP_SENTINEL_TIMER", "SP_SENTINEL_AGE", "sentinel-pass"),
    ("SP_OPS_TIMER", "SP_OPS_AGE", "persona.ops"),
    ("SP_HEIMDALL_TIMER", "SP_HEIMDALL_AGE", "reconcile.heimdall"),
];

/// The loop tasks' keys from one `scheduler status` reply: TIMER is `1` armed / `0` not, AGE the
/// seconds since the task last fired. Any key is `?` when the reply failed, did not parse or lacks the task.
fn loop_pulse(reply: Result<String, String>) -> Vec<(&'static str, String)> {
    let doc = reply.ok().and_then(|r| serde_json::from_str::<Value>(&r).ok());
    let at = doc.as_ref().and_then(|d| d["at"].as_u64());
    LOOP_PULSE
        .iter()
        .flat_map(|(timer_key, age_key, task)| {
            let row = doc.as_ref().and_then(|d| d["tasks"].as_array()?.iter().find(|t| t["name"] == *task));
            let timer = row.map_or("?", |t| if t["next_due_at"].is_null() { "0" } else { "1" });
            let age = row.and_then(|t| Some(at?.saturating_sub(t["last_fired_at"].as_u64()?) / 1000)).map_or("?".to_string(), |n| n.to_string());
            [(*timer_key, timer.to_string()), (*age_key, age)]
        })
        .collect()
}

fn scheduler_status() -> Result<String, String> {
    let sock = spira_config::process::cfg("SPIRA_RUNTIME_SOCKET")?;
    spira_config::scheduler_link::call(std::path::Path::new(sock.trim()), &["scheduler", "status"])
}

/// `dwarf_alive` (`lib.sh`): the one canonical implementation (`strand::probe::dwarf_alive`,
/// wave 4.23 sp-0ffox — "collapsing the bead/cockpit-collect copies"), called in-process —
/// a Rust-to-Rust call costs nothing extra over this crate's own former copy, unlike the
/// `bash`-bridge round trip per pidfile this probe still avoids for the `lib.sh` helpers
/// that do real work (this one was always three token comparisons).
fn dwarf_alive(pidfile: &Path) -> bool {
    strand::probe::dwarf_alive(pidfile)
}

/// `show <bead>` -> (priority, sanitized title, repo name), `?`/empty on any failure.
const DWARF_STATIC_TTL_S: i64 = 300;

struct DwarfStatic {
    name: String,
    pri: String,
    title: String,
    repo: String,
    partition: String,
    rune_model: String,
}

fn dwarf_static_path(run: &std::path::Path, bead: &str) -> std::path::PathBuf {
    run.join(format!("cockpit-dwarf-{bead}.cache"))
}

fn dwarf_static_load(path: &std::path::Path, now: i64) -> Option<DwarfStatic> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if now - v.get("at")?.as_i64()? > DWARF_STATIC_TTL_S {
        return None;
    }
    let f = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(DwarfStatic {
        name: f("name")?,
        pri: f("pri")?,
        title: f("title")?,
        repo: f("repo")?,
        partition: f("partition")?,
        rune_model: f("rune_model")?,
    })
}

/// What a running dwarf's row shows that does not change while it runs: read once per
/// `DWARF_STATIC_TTL_S` instead of costing ~4 subprocesses per live dwarf per 5 s tick. A
/// cannot-tell field (`?`) is never cached, so a transient failure heals on the next tick.
fn dwarf_static(run: &std::path::Path, home: &std::path::Path, bead: &str, fay: &str, pf: &std::path::Path) -> DwarfStatic {
    let path = dwarf_static_path(run, bead);
    let now = io::now();
    if let Some(hit) = dwarf_static_load(&path, now) {
        return hit;
    }
    let name = io::lib_call(home, "dwarf_named", &[pf.to_str().unwrap_or("")]).unwrap_or_else(|| "?".to_string());
    let (pri, title, repo) = parse_show_meta(io::contentjson(&["show", bead]), bead);
    let partition = io::content(&["state", bead, "rune"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".to_string());
    let rune_model = io::lib_call(home, "persona_model", &[fay])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".to_string());
    let st = DwarfStatic { name, pri, title, repo, partition, rune_model };
    let whole = [&st.name, &st.pri, &st.title, &st.partition, &st.rune_model].iter().all(|s| s.as_str() != "?");
    if whole {
        let body = serde_json::json!({
            "at": now, "name": st.name, "pri": st.pri, "title": st.title,
            "repo": st.repo, "partition": st.partition, "rune_model": st.rune_model,
        });
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, body.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    st
}

fn sweep_dwarf_static(caches: &[PathBuf], now: i64) {
    for path in caches {
        let age = std::fs::metadata(path).and_then(|m| m.modified()).ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| now - d.as_secs() as i64);
        if age.is_some_and(|a| a > 2 * DWARF_STATIC_TTL_S) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn parse_show_meta(raw: Option<String>, _bead: &str) -> (String, String, String) {
    let Some(rows) = io::json_rows(raw) else {
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
// core_counts_keys — SP_WAITING only (the claimable count is the claim view's).
// ---------------------------------------------------------------------------------------

pub fn core_counts_keys() -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_WAITING", waiting_count(lc::verb_read(&["list-asks"])));
    out
}

/// The open asks the store's ask record holds; an unreadable record is `?`, never 0.
fn waiting_count(read: Result<String, lc::Gone>) -> String {
    read.ok().and_then(|t| spira_store::asks::parse(&t).ok()).map_or("?".into(), |a| a.len().to_string())
}

#[cfg(test)]
mod sop_tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const SHELF_FLAT: &str = r#"{"sop-disk-full":"MATCH: a","sop-clock-skew":"MATCH: b"}"#;

    fn entry(sop: &str, check: &str, held: &str, epoch: i64) -> String {
        let (days, s) = (epoch.div_euclid(86400), epoch.rem_euclid(86400));
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(r#"{{"ts":"{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z","epoch":{epoch},"sop":"{sop}","check":"{check}","held":"{held}"}}"#, s / 3600, s % 3600 / 60, s % 60) + "\n"
    }

    fn probe(ledger: Option<&str>, shelf: &str) -> Vec<(String, String)> {
        let dir = testkit::TempDir::new("sop-ledger");
        let path = dir.path().join("applied.jsonl");
        if let Some(text) = ledger {
            std::fs::write(&path, text).unwrap();
        }
        sop_keys_from(Some(shelf.to_string()), path.to_str().unwrap(), 24.0, NOW)
    }

    fn get(kv: &[(String, String)], k: &str) -> String {
        kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    // covers: UC-cockpit-observability-14
    #[test_id(t87h2ne)]
    #[test]
    fn never_fired_and_recurred_respect_the_window_held_and_check_fail() {
        let none = probe(None, SHELF_FLAT);
        assert_eq!((get(&none, "SP_SOP_NEVER_FIRED").as_str(), get(&none, "SP_SOP_RECURRED").as_str(), get(&none, "SP_SWEEP_AGE").as_str()), ("2", "0", "?"));
        let held = probe(Some(&entry("sop-disk-full", "pass", "yes", NOW)), SHELF_FLAT);
        assert_eq!((get(&held, "SP_SOP_NEVER_FIRED").as_str(), get(&held, "SP_SOP_RECURRED").as_str(), get(&held, "SP_SWEEP_AGE").as_str()), ("1", "0", "0"));
        let recurred = probe(Some(&entry("sop-disk-full", "pass", "no", NOW)), SHELF_FLAT);
        assert_eq!(get(&recurred, "SP_SOP_RECURRED"), "1");
        let outside = probe(Some(&(entry("sop-clock-skew", "pass", "no", NOW - 3 * 86400) + &entry("sop-disk-full", "pass", "yes", NOW))), SHELF_FLAT);
        assert_eq!((get(&outside, "SP_SOP_RECURRED").as_str(), get(&outside, "SP_SOP_NEVER_FIRED").as_str()), ("0", "0"), "an entry outside the window is no recurrence");
        let unfixed = probe(Some(&entry("sop-disk-full", "fail", "no", NOW)), SHELF_FLAT);
        assert_eq!(get(&unfixed, "SP_SOP_RECURRED"), "0", "check=fail means the fix was never run");
    }

    // covers: UC-cockpit-observability-14
    #[test_id(tf6ryeq)]
    #[test]
    fn the_sweep_age_is_the_newest_ledger_entry_and_unreadable_sources_are_unknown() {
        let stale = probe(Some(&entry("sop-disk-full", "pass", "yes", NOW - 7200)), SHELF_FLAT);
        assert_eq!(get(&stale, "SP_SWEEP_AGE"), "7200");
        let newest = probe(Some(&(entry("sop-disk-full", "pass", "yes", NOW - 7200) + &entry("sop-clock-skew", "pass", "yes", NOW))), SHELF_FLAT);
        assert_eq!(get(&newest, "SP_SWEEP_AGE"), "0", "the newest entry wins");
        let empty = probe(Some(""), SHELF_FLAT);
        assert_eq!(get(&empty, "SP_SWEEP_AGE"), "?");
        for broken in [None, Some(String::new()), Some("not json".to_string())] {
            let kv = sop_keys_from(broken, "nonexistent/applied.jsonl", 24.0, NOW);
            assert_eq!(kv.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["?", "?", "?"]);
        }
        let dir = testkit::TempDir::new("sop-dir-ledger");
        let kv = sop_keys_from(Some(SHELF_FLAT.into()), dir.path().to_str().unwrap(), 24.0, NOW);
        assert_eq!(kv.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["?", "?", "?"], "a ledger that cannot be read is unknown, not zero");
    }
}

#[cfg(test)]
mod mail_tests {
    use super::*;

    // covers: UC-cockpit-observability-26
    #[test_id(ti9bv0w)]
    #[test]
    fn mail_keys_count_unread_and_label_each_message_new_read_or_done() {
        let dir = testkit::TempDir::new("mail-keys");
        let concierge = dir.path().join("concierge");
        for sub in ["new", "cur", "tmp"] {
            std::fs::create_dir_all(concierge.join(sub)).unwrap();
        }
        let msg = |subject: &str| format!("From: Operator <op@h>\nSubject: {subject}\n\nbody\n");
        std::fs::write(concierge.join("new/n1"), msg("New message")).unwrap();
        std::fs::write(concierge.join("cur/r1"), msg("Read message")).unwrap();
        std::fs::write(concierge.join("cur/d1:2,R"), msg("Done message")).unwrap();
        let cfg = Cfg { mail: dir.path().display().to_string(), ..Cfg::default() };
        let kv = mail_keys(&cfg);
        let get = |k: &str| kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!((get("SP_MAIL_UNREAD").as_str(), get("SP_MAIL_N").as_str()), ("1", "3"));
        let rows: String = (0..3).map(|i| get(&format!("SP_MAIL{i}"))).collect();
        for state in ["|NEW|New message", "|READ|Read message", "|DONE|Done message"] {
            assert!(rows.contains(state), "{state} missing from {rows}");
        }
        let none = mail_keys(&Cfg { mail: "nonexistent/mail".into(), ..Cfg::default() });
        assert_eq!(none.iter().find(|(n, _)| n == "SP_MAIL_UNREAD").map(|(_, v)| v.as_str()), Some("?"), "an unreadable mailbox is unknown, not zero");
    }
}

#[cfg(test)]
mod now_listing_tests {
    use super::*;
    use std::cell::Cell;

    fn pid_file(run: &Path, rune: &str, bead: &str) -> PathBuf {
        run.join(format!("dwarf-{rune}-{bead}.{}", "pid"))
    }

    fn live_pid(run: &Path, rune: &str, bead: &str) {
        let pf = pid_file(run, rune, bead);
        std::fs::write(&pf, "1").unwrap();
        std::fs::write(pf.with_extension("lease"), (io::now() + 3600).to_string()).unwrap();
    }

    #[test_id(tdt9ga9)]
    #[test]
    fn one_now_pass_lists_the_run_directory_once() {
        let dir = testkit::TempDir::new("cockpit-now-listing");
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let world = spira_config::process::fixture_world_env(dir.path(), &run, &["spira"]);
        let edits: Vec<(&str, Option<&str>)> = world.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        live_pid(&run, "a", "sp-one");
        live_pid(&run, "b", "sp-two");
        std::fs::write(pid_file(&run, "c", "sp-dead"), "1").unwrap();
        let expired = run.join("cockpit-dwarf-sp-old.cache");
        let fresh = run.join("cockpit-dwarf-sp-new.cache");
        std::fs::write(&expired, "{}").unwrap();
        std::fs::write(&fresh, "{}").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * DWARF_STATIC_TTL_S as u64);
        std::fs::File::options().write(true).open(&expired).unwrap().set_modified(old).unwrap();

        let listings = Cell::new(0);
        let kv = now_keys_with(&Cfg::default(), &run, &|r| {
            listings.set(listings.get() + 1);
            list_dwarf_files(r)
        });
        let get = |k: &str| kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());

        assert_eq!(listings.get(), 1, "the run directory is listed once per pass");
        assert_eq!(get("SP_DWARF_N").as_deref(), Some("2"), "only live pids are rows");
        assert_eq!(get("SP_DWARVES").as_deref(), Some("2"), "only live pids are counted");
        assert!(!expired.exists(), "the expired cache is removed");
        assert!(fresh.exists(), "a fresh cache stays");
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    // covers: UC-cockpit-observability-02
    #[test_id(tb2f6tx)]
    #[test]
    fn a_pass_opens_with_its_stamp_then_the_window() {
        let _env = testkit::env(&[("SPIRA_COCKPIT_WINDOW_HOURS", Some("6"))]);
        assert_eq!(stamp_keys(1234), vec![("SP_AT".to_string(), "1234".to_string()), ("SP_WINDOW_HOURS".to_string(), "6".to_string())]);
        drop(_env);
        let _unset = testkit::env(&[("SPIRA_COCKPIT_WINDOW_HOURS", None)]);
        assert_eq!(stamp_keys(1)[1].1, "24", "the window defaults to a day");
    }
}

#[cfg(test)]
mod gate_scan_tests {
    use super::*;

    fn gate_dir(run: &Path, slug: &str, pid: i64) {
        let d = run.join("gate-run").join(slug);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pid"), pid.to_string()).unwrap();
        std::fs::write(d.join("started"), "900").unwrap();
        std::fs::write(d.join("out"), "test").unwrap();
    }

    // covers: UC-cockpit-observability-15
    #[test_id(tu06juz)]
    #[test]
    fn a_gate_is_live_only_when_its_pid_is_alive_and_its_cmdline_is_the_gate() {
        let run = testkit::TempDir::new("gate-scan");
        gate_dir(run.path(), "dead", 11);
        gate_dir(run.path(), "recycled", 22);
        gate_dir(run.path(), "live", 33);
        let argv_of = |pid: i64| match pid {
            22 => Some("sleep 3600".to_string()),
            33 => Some("bin/bash gate-run.sh spira/sp-x".to_string()),
            _ => None,
        };
        let (rows, n, live) = gate_scan_with(run.path(), 1000, &argv_of);
        assert_eq!((n, live), (1, 1));
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str(), rows[0].2.as_str()), ("live", "100", "running"));
    }

    // covers: UC-cockpit-observability-15
    #[test_id(tnigpsg)]
    #[test]
    fn no_gate_run_directory_is_zero_gates() {
        let run = testkit::TempDir::new("gate-scan-none");
        let (rows, n, live) = gate_scan_with(run.path(), 1000, &|_| Some("gate".into()));
        assert_eq!((rows.len(), n, live), (0, 0, 0));
    }
}

#[cfg(test)]
mod waiting_tests {
    use super::*;

    // covers: UC-cockpit-observability-08
    #[test_id(t3dqpda)]
    #[test]
    fn waiting_is_the_ask_record_not_the_decision_beads_and_unknown_when_unreadable() {
        use spira_store::lc_call::fake;
        let read = |out: &str, code: i32| {
            let (c, _) = crate::store::scripted::client(&[("list-asks", code, out)]);
            crate::store::answer_kind(&c, &["list-asks"])
        };
        let three = fake::open_asks_json();
        assert_eq!(waiting_count(read(&three, 0)), fake::OPEN_ASK_IDS.len().to_string());
        let beads = serde_json::json!([fake::decision_without_record()]).to_string();
        assert_eq!(waiting_count(read(&beads, 0)), "?", "a bead list is not an ask list");
        assert_eq!(waiting_count(read("{}", 0)), "?");
        assert_eq!(waiting_count(read("", 1)), "?");
        assert_eq!(waiting_count(read("[]", 0)), "0");
    }
}

// ---------------------------------------------------------------------------------------
// slots_keys
// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod bound_tests {
    use super::bound_records;

    #[test_id(trihsxo)]
    #[test]
    fn bound_records_joins_each_class_and_says_question_when_the_scheduler_is_silent() {
        let two = r#"[{"class":"build-disk","limit":7,"held":9,"waiting":3,"over":true},{"class":"memory","limit":4,"held":2,"waiting":1,"over":false}]"#;
        assert_eq!(bound_records(Ok(two.into())), "build-disk 9 7 3 1;memory 2 4 1 0");
        assert_eq!(bound_records(Ok("[]".into())), "");
        assert_eq!(bound_records(Err("the scheduler is not answering".into())), "?");
        assert_eq!(bound_records(Ok("not json".into())), "?");
        assert_eq!(bound_records(Ok(r#"[{"class":"x"}]"#.into())), "?");
    }
}

fn scheduler_bound() -> Result<String, String> {
    let sock = spira_config::process::cfg("SPIRA_RUNTIME_SOCKET")?;
    spira_config::scheduler_link::call(std::path::Path::new(sock.trim()), &["scheduler", "bound"])
}

/// `class held limit waiting over` records joined by `;`; `?` when the scheduler did not answer.
fn bound_records(reply: Result<String, String>) -> String {
    let Ok(doc) = reply.and_then(|r| serde_json::from_str::<Vec<serde_json::Value>>(&r).map_err(|e| e.to_string())) else { return "?".to_string() };
    let num = |b: &serde_json::Value, k: &str| b[k].as_i64().map(|n| n.to_string());
    let record = |b: &serde_json::Value| Some(format!("{} {} {} {} {}", b["class"].as_str()?, num(b, "held")?, num(b, "limit")?, num(b, "waiting")?, u8::from(b["over"].as_bool()?)));
    doc.iter().map(record).collect::<Option<Vec<_>>>().map_or_else(|| "?".to_string(), |r| r.join(";"))
}

pub fn slots_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let home = io::home_dir();
    let live = io::lib_call(&home, "dwarves_live_total", &[]).unwrap_or_else(|| "?".to_string());
    push(&mut out, "SP_SLOTS_LIVE", live.clone());

    let file_pool = io::max_dwarves();
    let pool_limit = Limit::read("SPIRA_MAX_DWARVES", Some(file_pool.trim()).filter(|p| !p.is_empty()));
    let ceiling_limit = Limit::read("SPIRA_MAX_LIVE_DWARVES", Some(&cfg.max_live_dwarves.to_string()));
    let ceiling = ceiling_limit.shown().unwrap_or("?").to_string();
    push(&mut out, "SP_SLOTS_CEILING", ceiling.clone());
    push(&mut out, "SP_SLOTS_POOL", pool_limit.shown().unwrap_or("?").to_string());
    push(&mut out, "SP_SLOTS_UNCONFIRMED", if ceiling_limit.held().is_some() && pool_limit.held().is_some() { "" } else { "1" }.to_string());
    let pending: Vec<(&str, &spira_config::scheduler_link::Pending)> = [("ceiling", &ceiling_limit), ("pool", &pool_limit)].into_iter().filter_map(|(n, l)| l.pending().map(|p| (n, p))).collect();
    push(&mut out, "SP_SLOTS_PENDING", pending.iter().map(|(n, p)| format!("{n} {}", p.config)).collect::<Vec<_>>().join(", "));
    push(&mut out, "SP_SLOTS_WAITS", pending.first().map(|(_, p)| p.waits.clone()).unwrap_or_default());
    push(&mut out, "SP_SLOTS_DIRECTED", strand::probe::dwarves_live_directed(&strand_cfg()).map(|n| n.to_string()).unwrap_or_else(|_| "?".to_string()));

    // A failed read must never resolve to a reassuring free count.
    let free = match (live.trim().parse::<i64>(), ceiling.trim().parse::<i64>()) {
        (Ok(l), Ok(c)) => (c - l).max(0).to_string(),
        _ => "?".to_string(),
    };
    push(&mut out, "SP_SLOTS_FREE", free);
    push(&mut out, "SP_BOUND", bound_records(scheduler_bound()));

    let run = io::run_dir();
    let cap_now = io::now();
    let capacity_paused = match dwarf::capacity::pause_state(&capacity_pause_file(&run), cap_now) {
        dwarf::capacity::PauseState::Paused { .. } => "1",
        // Fail closed: never "0"/open on an unreadable file (wave4-decomposition.md (c)3).
        dwarf::capacity::PauseState::Unknown => "?",
        _ => "0",
    };
    push(&mut out, "SP_SLOTS_CAPACITY_PAUSED", capacity_paused);
    out
}

// ---------------------------------------------------------------------------------------
// well_keys
// ---------------------------------------------------------------------------------------

pub fn well_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    // A work bead's state is the lifecycle row's (design §3.4); `None` renders `?`.
    match (lc::held_rows("poison"), lc::held_rows("ask")) {
        (Some(poison), Some(ask)) => {
            push(&mut out, "SP_POISON", poison_count(&poison).to_string());
            push(&mut out, "SP_HOLD_ASK", ask_hold_count(&ask).to_string());
        }
        _ => {
            push(&mut out, "SP_POISON", "?");
            push(&mut out, "SP_HOLD_ASK", "?");
        }
    }

    let scope = &cfg.scope_label;
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let rows = io::json_rows(io::live_beads(&label));
    let lc_rows = rows.as_deref().and_then(|r| lc::state_index_of(&ids_of(r)));
    match rows.zip(lc_rows) {
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
            let (open_n, inprog_n, needsop_n) = well_counts(&work, &lc);
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

/// (unmapped, absent) over a full row set: the answer the server's `plan repo-labels` gives.
fn repo_label_counts(rows: &[Value], valid: &std::collections::HashSet<String>) -> (usize, usize) {
    let (mut unmapped, mut absent) = (0, 0);
    for i in rows {
        let labels: Vec<&str> = i.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let repo_labels: Vec<&str> = labels.iter().filter_map(|l| l.strip_prefix("repo:")).collect();
        if repo_labels.is_empty() {
            absent += 1;
        } else if !repo_labels.iter().any(|r| valid.contains(*r)) {
            unmapped += 1;
        }
    }
    (unmapped, absent)
}

fn repo_labels_argv(sorted_names: &[&str]) -> Vec<String> {
    ["plan", "repo-labels", "--valid", &sorted_names.join(",")].map(String::from).to_vec()
}

fn repo_labels_from_server(raw: &str) -> Option<(usize, usize)> {
    let v: Value = serde_json::from_str(raw).ok()?;
    Some((v.get("unmapped")?.as_u64()? as usize, v.get("absent")?.as_u64()? as usize))
}

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

    let counts = if io::fixture_mode() {
        io::json_rows(io::live_beads("")).map(|rows| repo_label_counts(&rows, &valid))
    } else {
        let mut names: Vec<&str> = valid.iter().map(String::as_str).collect();
        names.sort_unstable();
        let argv = repo_labels_argv(&names);
        lc::verb(&argv.iter().map(String::as_str).collect::<Vec<_>>()).and_then(|raw| repo_labels_from_server(&raw))
    };
    match counts {
        None => {
            push(&mut out, "SP_REPO_UNMAPPED", "?");
            push(&mut out, "SP_REPO_ABSENT", "?");
        }
        Some((unmapped, absent)) => {
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
    let rows = io::json_rows(io::live_beads("spira,incident"));
    // An incident bead is a work bead (sp-jgjvh): whether it is over is its lifecycle row's.
    let lc_rows = rows.as_deref().and_then(|r| lc::state_index_of(&ids_of(r)));
    let Some((rows, lc)) = rows.zip(lc_rows) else {
        push(&mut out, "SP_DUP_REFS", "?");
        push(&mut out, "SP_DUP_BEADS", "?");
        push(&mut out, "SP_DUP_N", "0");
        return out;
    };
    let by_ref = incidents_by_ref(&rows, &lc);
    let mut dup: Vec<(String, Vec<String>)> = by_ref.into_iter().filter(|(_, ids)| ids.len() > 1).collect();
    push(&mut out, "SP_DUP_REFS", dup.len().to_string());
    push(&mut out, "SP_DUP_BEADS", dup.iter().map(|(_, ids)| ids.len() - 1).sum::<usize>().to_string());
    dup.sort_by_key(|e| std::cmp::Reverse(e.1.len()));
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
/// `groomer`/`mimir-trigger` resolve their own copy the same independent way.
fn strand_cfg() -> strand::config::Config {
    strand::config::Config::resolve(&strand::config::Live::load()).unwrap_or_else(|e| {
        eprintln!("cockpit-collect: {e}");
        std::process::exit(1)
    })
}

pub fn livelock_keys() -> Kv {
    let cfg = strand_cfg();
    let door = crate::store::door().unwrap_or_else(|e| {
        eprintln!("cockpit-collect: {e}");
        std::process::exit(1)
    });
    let week_ago = strand::timefmt::utc_stamp(strand::timefmt::now() - 7 * 86_400);
    livelock_report(
        &strand::detectors::detect_livelocked(&cfg, &door),
        strand::detectors::invalid_closed_since(&cfg, &door, &week_ago).as_deref(),
        || io::contentjson(&["list", "--limit", "1"]).is_some(),
    )
}

/// An empty LIVELOCK answer is "none" only when the store answers at all; otherwise `?`.
/// The invalid-closed rows follow their own read: `None` is `?`, an answered empty list is `0`.
fn livelock_report(ll_out: &str, ic: Option<&str>, store_answers: impl FnOnce() -> bool) -> Kv {
    let mut out = Kv::new();
    if ll_out.is_empty() && !store_answers() {
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
    if let Some(ic_out) = ic {
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
    } else {
        push(&mut out, "SP_INVALID_CLOSED", "?");
        push(&mut out, "SP_INVCLSD_N", "?");
        push(&mut out, "SP_UNFILED_FOLLOW", "?");
        push(&mut out, "SP_UNFLFLW_N", "?");
    }
    out
}

// ---------------------------------------------------------------------------------------
// strand_keys
// ---------------------------------------------------------------------------------------

pub fn strand_keys() -> Kv {
    strand_keys_of(std::fs::read_to_string(io::run_dir().join("strands.json")).ok().as_deref())
}

/// The strand keys for a ledger's text: `?` throughout when it is unreadable, otherwise the
/// ghost count (the labelled failure of a claimed bead whose holder is gone) split from every other class.
fn strand_keys_of(ledger: Option<&str>) -> Kv {
    let mut out = Kv::new();
    let Some(Ok(Value::Object(map))) = ledger.map(serde_json::from_str::<Value>) else {
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
    let raw = io::history_beads(label, io::ONLINE_WINDOW_SECS);
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
    let newest_ids: Vec<String> = CZAR_CLASSES
        .iter()
        .filter_map(|(r, _)| by_ref.get(*r)?.iter().max_by_key(|b| b.get("created_at").and_then(Value::as_str).and_then(crate::quoting::parse_iso8601).unwrap_or(0)))
        .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let lc_rows = lc::state_index_of(&newest_ids);
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
    let ledger = std::env::var("SPIRA_SOP_LEDGER").unwrap_or_else(|_| io::run_dir().join(SOP_LEDGER_REL).to_string_lossy().into_owned());
    let window_h: f64 = std::env::var("SPIRA_SOP_WINDOW_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24.0);
    let now = std::env::var("SPIRA_NOW").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or_else(io::now);
    sop_keys_from(io::contentjson(&["memories"]), &ledger, window_h, now)
}

/// The SOP shelf (`bd memories` JSON) read against the applied ledger: an unreadable shelf or
/// ledger is `?` for every key, a missing ledger is a valid empty one.
fn sop_keys_from(shelf_raw: Option<String>, ledger: &str, window_h: f64, now: i64) -> Kv {
    let mut out = Kv::new();
    let Some(raw) = shelf_raw.filter(|s| !s.trim().is_empty()) else {
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
    if Path::new(ledger).exists() {
        let Ok(content) = std::fs::read_to_string(ledger) else {
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
    let db_n: Option<usize> = io::contentjson(&["memories"]).filter(|s| !s.trim().is_empty()).and_then(|s| {
        serde_json::from_str::<Value>(s.trim()).ok()
    }).and_then(|v| v.as_object().map(|m| m.iter().filter(|(k, v)| v.is_string() && k.starts_with("law-")).count()));

    let page_n: Option<usize> = io::git_held(Path::new(&wiki), &["show", "HEAD:wiki/notes/common-law.md"])
        .map(|s| s.lines().filter(|l| l.starts_with("### ")).count());

    statute_skew_keys(db_n, page_n)
}

fn statute_skew_keys(db_n: Option<usize>, page_n: Option<usize>) -> Kv {
    let mut out = Kv::new();
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

/// `OK`, `<dirty>:<n>` counting `finding_prefix` lines, or `?` when the check could not run.
pub fn drift_value(code: Option<i32>, out: &str, dirty: &str, finding_prefix: &str) -> String {
    match code {
        Some(0) => "OK".to_string(),
        Some(1) => format!("{dirty}:{}", out.lines().filter(|l| l.starts_with(finding_prefix)).count()),
        _ => "?".to_string(),
    }
}

/// The checkout the drift probe reads: `SPIRA_REPO` when set, else the checkout holding `home`.
fn drift_repo(repo: Option<String>, home: &Path) -> std::path::PathBuf {
    let env = repo.map(|r| ("SPIRA_REPO".to_string(), r)).into_iter().collect();
    spira_config::resolve::derive_home_repo(home, &env)
}

pub fn drift_keys() -> Kv {
    use install::checkout_drift::Probe;
    use install::unit_drift::UnitDrift;
    let repo = io::try_home_dir().ok().map(|home| drift_repo(std::env::var("SPIRA_REPO").ok(), &home));
    let mut out = Kv::new();
    let (rc, text) = match repo.map_or(Probe::Unreadable, |r| install::checkout_drift::checkout(&r.to_string_lossy())) {
        Probe::Clean => (Some(0), String::new()),
        Probe::Drift(t) => (Some(1), t),
        Probe::Unreadable => (None, String::new()),
    };
    push(&mut out, "SP_CHECKOUT_DRIFT", drift_value(rc, &text, "DIRTY", "DIRTY "));
    let (rc, text) = match install::unit_drift::units_drift(&install::unit_drift::default_unit_dir()) {
        UnitDrift::Clean => (Some(0), String::new()),
        UnitDrift::Drift(t) => (Some(1), t),
        UnitDrift::Unreadable => (None, String::new()),
    };
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
    rows.sort_by_key(|e| std::cmp::Reverse(e.0));
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
// hel_keys — the HEL / SELF / dwarf-ledger keys, from crate::metrics.
// ---------------------------------------------------------------------------------------

const HEL_KEYS: &[&str] = &["SP_SENT_FAILED", "SP_SENT_FAILED_AGE_M", "SP_DWARF_BORN", "SP_DWARF_LIVED", "SP_DWARF_STILLBORN", "SP_DWARF_WORKED", "SP_DWARF_THRASH"];

pub fn hel_keys() -> Kv {
    let run = io::run_dir();
    let reap = run.join("reap.log");
    let ledger = run.join("dwarf-ledger.log");
    let keys = crate::metrics::collect(reap.to_str().unwrap_or(""), ledger.to_str().unwrap_or(""), 24.0, crate::timeutil::now_secs());
    let mut out: Kv = keys.into_iter().collect();
    out.retain(|(k, _)| HEL_KEYS.contains(&k.as_str()));
    out
}

/// SP_DUP_*: incident bead ids grouped by `external_ref`, over the incidents still live —
/// read from each bead's lifecycle row, never bd status. A terminal bead is a predecessor
/// of a recurrence, never surplus; a bead with no lifecycle row is not live work; a
/// `duplicate-of:` bead is already accounted for.
pub fn ids_of(rows: &[Value]) -> Vec<String> {
    rows.iter().filter_map(|r| r.get("id").and_then(Value::as_str)).map(str::to_string).collect()
}

pub fn incidents_by_ref(rows: &[Value], lc: &HashMap<String, lc::Row>) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for i in rows {
        let ref_ = i.get("external_ref").and_then(Value::as_str).unwrap_or("");
        if ref_.is_empty() {
            continue;
        }
        let duplicate = i.get("labels").and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).any(|l| l.starts_with("duplicate-of:")));
        if duplicate {
            continue;
        }
        let id = i.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        if lc.get(&id).is_some_and(|r| !r.terminal()) {
            grouped.entry(ref_.to_string()).or_default().push(id);
        }
    }
    grouped
}

/// SP_POISON: work beads the machine holds for poison, not yet over (the `spira-poison`
/// label is retired; holds replace it, design §3.4).
pub fn poison_count(lc: &HashMap<String, lc::Row>) -> usize {
    lc.values().filter(|r| r.held("poison") && !r.terminal()).count()
}

/// SP_HOLD_ASK: work beads held on an ask the operator has yet to answer, not yet over.
/// Its own figure: wait, operator and poison holds are not decisions awaiting the operator.
pub fn ask_hold_count(lc: &HashMap<String, lc::Row>) -> usize {
    lc.values().filter(|r| r.held("ask") && !r.terminal()).count()
}

/// SP_OPEN / SP_INPROG / SP_NEEDSOP over the plan's work items: a work bead's state is its
/// lifecycle row's — open is "not over" (not terminal), in progress is WORKING. An epic has
/// no lifecycle row: it is a coordination bead, whose bd status is its only state
/// (spira_config::nonwork). Any other item with no row is not counted: the machine cannot
/// tell its state, and a rowless bead can never be claimed (CHECK-ROWLESS reports it).
pub fn well_counts(work: &[&Value], lc: &HashMap<String, lc::Row>) -> (usize, usize, usize) {
    let _has = |i: &Value, lab: &str| {
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
        // Needs the operator = the row's ask hold (sp-psztcc), not the label.
        needsop_n += usize::from(open && lc.get(id).is_some_and(|r| r.held("ask")));
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

    #[test_id(t88yth0)]
    #[test]
    fn the_commit_time_is_asked_of_git_again_only_when_head_or_the_base_moved_or_cannot_be_read() {
        let wt = Path::new("memo-fixture/worktree");
        let asked = std::cell::Cell::new(0);
        let ask = |ts: i64| {
            let asked = &asked;
            move || {
                asked.set(asked.get() + 1);
                Some(ts)
            }
        };
        let keys = |h: &str, b: &str| Some((h.repeat(40), b.repeat(40)));
        assert_eq!(commit_ahead_memo(wt, keys("1", "a"), ask(10)), Some(10));
        assert_eq!(commit_ahead_memo(wt, keys("1", "a"), ask(99)), Some(10), "nothing moved");
        assert_eq!(asked.get(), 1);
        assert_eq!(commit_ahead_memo(wt, keys("2", "a"), ask(20)), Some(20), "HEAD moved");
        assert_eq!(commit_ahead_memo(wt, keys("2", "b"), ask(30)), Some(30), "the base moved");
        assert_eq!(commit_ahead_memo(wt, keys("2", "b"), ask(99)), Some(30));
        assert_eq!(asked.get(), 3);
        assert_eq!(commit_ahead_memo(wt, None, ask(40)), Some(40), "unreadable refs ask git");
        assert_eq!(commit_ahead_memo(wt, None, ask(50)), Some(50), "and are never kept");
        assert_eq!(commit_ahead_memo(wt, keys("2", "b"), ask(60)), Some(60), "a kept answer does not outlive an unreadable round");
        assert_eq!(asked.get(), 6);
    }

    fn git_in(dir: &Path, date: &str, args: &[&str]) {
        // batch-job: a fixture repository's own commit, bounded by the test
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test_id(tscf113)]
    #[test]
    fn the_commit_time_follows_head_and_the_base_through_the_real_ref_files() {
        let d = testkit::TempDir::new("commit-ahead");
        let git = |date: &str, args: &[&str]| git_in(d.path(), date, args);
        git("@1000 +0000", &["init", "-q", "-b", "main"]);
        git("@1000 +0000", &["commit", "-q", "--allow-empty", "-m", "c1"]);
        git("@1000 +0000", &["branch", "base"]);
        git("@2000 +0000", &["commit", "-q", "--allow-empty", "-m", "c2"]);
        assert_eq!(commit_ahead_ts(d.path(), "base"), Some(2000));
        git("@3000 +0000", &["commit", "-q", "--allow-empty", "-m", "c3"]);
        assert_eq!(commit_ahead_ts(d.path(), "base"), Some(3000), "HEAD moved");
        git("@3000 +0000", &["branch", "-f", "base", "HEAD"]);
        assert_eq!(commit_ahead_ts(d.path(), "base"), None, "the base moved to HEAD, nothing is ahead");
    }

    #[test_id(tb4inqo)]
    #[test]
    fn firing_alerts_come_from_the_stores_heimdall_conditions() {
        let doc = r#"{"conditions":[{"key":"sentinel-stale","status":"True"},{"key":"mirror-stale","status":"False"},{"key":"db-down","status":"True"}]}"#;
        assert_eq!(heimdall_alerts(Some(doc.into())), ("2".to_string(), "sentinel-stale,db-down".to_string()));
        assert_eq!(heimdall_alerts(Some(r#"{"conditions":[]}"#.into())).0, "0");
        assert_eq!(heimdall_alerts(None).0, "?");
        assert_eq!(heimdall_alerts(Some("garbage".into())).0, "?");
    }

    #[test_id(tltvb0a)]
    #[test]
    fn loop_keys_come_from_the_schedulers_own_record_of_each_task() {
        let doc = r#"{"at":100000,"tasks":[
            {"name":"sentinel-pass","next_due_at":160000,"last_fired_at":95000},
            {"name":"persona.ops","next_due_at":null,"last_fired_at":40000},
            {"name":"reconcile.heimdall","next_due_at":1,"last_fired_at":null}]}"#;
        let get = |reply: Result<String, String>, k: &str| loop_pulse(reply).into_iter().find(|(key, _)| *key == k).unwrap().1;
        let ok = || Ok(doc.to_string());
        assert_eq!((get(ok(), "SP_SENTINEL_TIMER").as_str(), get(ok(), "SP_SENTINEL_AGE").as_str()), ("1", "5"));
        assert_eq!((get(ok(), "SP_OPS_TIMER").as_str(), get(ok(), "SP_OPS_AGE").as_str()), ("0", "60"));
        assert_eq!((get(ok(), "SP_HEIMDALL_TIMER").as_str(), get(ok(), "SP_HEIMDALL_AGE").as_str()), ("1", "?"));
        let lacking = || Ok(r#"{"at":1,"tasks":[]}"#.to_string());
        for reply in [lacking as fn() -> Result<String, String>, || Err("down".into()), || Ok("not json".into())] {
            for (t, a, _) in LOOP_PULSE {
                assert_eq!((get(reply(), t).as_str(), get(reply(), a).as_str()), ("?", "?"));
            }
        }
    }

    #[test_id(t5dj2m3)]
    #[test]
    fn dwarf_static_cache_honours_ttl_and_refuses_partial_rows() {
        let t = testkit::TempDir::new("dwarf-static");
        let d = t.path().to_path_buf();
        let p = dwarf_static_path(&d, "sp-x");
        let row = |at: i64| serde_json::json!({"at": at, "name": "n", "pri": "2", "title": "t", "repo": "r", "partition": "p", "rune_model": "m"});
        std::fs::write(&p, row(1000).to_string()).unwrap();
        let hit = dwarf_static_load(&p, 1000 + DWARF_STATIC_TTL_S).expect("fresh row is served");
        assert_eq!((hit.name.as_str(), hit.partition.as_str()), ("n", "p"));
        assert!(dwarf_static_load(&p, 1001 + DWARF_STATIC_TTL_S).is_none(), "an expired row is re-read");
        std::fs::write(&p, r#"{"at":1000,"name":"n"}"#).unwrap();
        assert!(dwarf_static_load(&p, 1000).is_none(), "a row missing fields is not served");
    }

    #[test_id(tt1wfi2)]
    #[test]
    fn overrides_are_counted_from_the_spec_files_with_no_child_process() {
        let t = testkit::TempDir::new("ov-summary");
        let d = t.path();
        std::fs::create_dir_all(d.join(".state")).unwrap();
        std::fs::create_dir_all(d.join("retired")).unwrap();
        std::fs::write(d.join("alpha.override"), "needed() { true; }\nBEAD=\"sp-aaa\"\n").unwrap();
        std::fs::write(d.join("beta.override"), "BEAD='sp-bbb' # why\n").unwrap();
        std::fs::write(d.join("gamma.override"), "BEAD=sp-ccc  # bare\n").unwrap();
        std::fs::write(d.join(".state/beta.failed"), "apply-failed\n").unwrap();
        std::fs::write(d.join("retired/old.override"), "BEAD=\"sp-old\"\n").unwrap();
        assert_eq!(overrides_summary(d), (3, 1, "alpha:sp-aaa,beta:sp-bbb,gamma:sp-ccc".to_string()));
        assert_eq!(overrides_summary(&d.join("absent")), (0, 0, String::new()));
    }

    #[test_id(t3mshqe)]
    #[test]
    fn hotfix_keys_come_from_the_release_state_dir_in_process() {
        const SHA: &str = "abc1230000000000000000000000000000000000";
        let t = testkit::TempDir::new("hotfix-keys");
        let state = t.path().join("run/release");
        std::fs::create_dir_all(&state).unwrap();
        const RECORD: &str = "hotfix";
        std::fs::write(state.join(RECORD), format!("sha {SHA}\nreason fix it\nat 2020-01-01T00:00:00Z\n")).unwrap();
        let mut env = release::config::Env::new();
        env.insert("HOME".into(), t.path().display().to_string());
        let flags = release::config::Flags { releases: Some(t.path().join("rel")), run: Some(t.path().join("run")), keep: Some(2) };
        let reg = release::config::Registered { releases_keep: "2".into(), ..Default::default() };
        let cfg = release::config::Config::resolve_with(&flags, &env, None, &reg).unwrap();
        let (line, alert) = hotfix_lines_of(&cfg);
        assert!(line.starts_with(&format!("RUNNING UNLANDED {SHA}: fix it")), "{line}");
        assert!(alert.starts_with(&format!("ALERT hotfix {SHA}")), "{alert}");
    }

    fn lcmap(rows: &[(&str, &str, &[&str])]) -> HashMap<String, lc::Row> {
        rows.iter()
            .map(|(id, st, holds)| {
                (id.to_string(), lc::Row { bead_id: id.to_string(), state: st.to_string(), holds: holds.iter().map(|h| h.to_string()).collect(), ..Default::default() })
            })
            .collect()
    }

    // covers: UC-cockpit-observability-13
    /// Duplicate incident refs are counted from the lifecycle row: only non-terminal rows
    /// count; terminal, rowless and duplicate-of: never do.
    #[test_id(t9sqx4x)]
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
        let mut got = incidents_by_ref(&rows, &lc).remove("r1").unwrap();
        got.sort();
        assert_eq!(got, vec!["a", "b"]);
    }

    // covers: UC-cockpit-observability-13
    #[test_id(tg6sfi5)]
    #[test]
    fn a_terminal_predecessor_and_a_recurrence_is_not_surplus_but_two_live_rows_are() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"p","status":"open","external_ref":"r"},{"id":"q","status":"open","external_ref":"r"}]"#,
        )
        .unwrap();
        for dead in ["DONE", "DROPPED", "SUPERSEDED"] {
            let got = incidents_by_ref(&rows, &lcmap(&[("p", dead, &[]), ("q", "READY", &[])]));
            assert_eq!(got["r"].len(), 1, "{dead}");
        }
        let got = incidents_by_ref(&rows, &lcmap(&[("p", "READY", &[]), ("q", "READY", &[])]));
        assert_eq!(got["r"].len(), 2);
    }

    /// sp-mve9i: the counts follow the lifecycle row, whatever bd's status says.
    #[test_id(t9d3jw3)]
    #[test]
    fn well_counts_read_the_lifecycle_state_not_bd_status() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"a","status":"closed","issue_type":"task","labels":["ask"]},
                {"id":"b","status":"open","issue_type":"task"},
                {"id":"c","status":"in_progress","issue_type":"bug"},
                {"id":"d","status":"open","issue_type":"task"},
                {"id":"e","status":"open","issue_type":"epic"}]"#,
        )
        .unwrap();
        let work: Vec<&Value> = rows.iter().collect();
        let lc = lcmap(&[("a", "REWORK", &[]), ("b", "LANDED", &[]), ("c", "SUBMITTED", &["ask"])]);
        // a: open (REWORK), and its ask LABEL is not a hold (sp-psztcc); b: over; c: open, not
        // WORKING, needs-op by its ask hold; d: rowless, not counted; e: an epic, open by bd.
        assert_eq!(well_counts(&work, &lc), (3, 0, 1));
        let lc = lcmap(&[("d", "WORKING", &[])]);
        assert_eq!(well_counts(&work, &lc), (2, 1, 0));
    }

    // covers: UC-cockpit-observability-13
    #[test_id(ttdqp1u)]
    #[test]
    fn poison_counts_live_poison_holds() {
        let lc = lcmap(&[("a", "REWORK", &["poison"]), ("b", "LANDED", &["poison"]), ("c", "READY", &["wait"])]);
        assert_eq!(poison_count(&lc), 1);
    }

    #[test_id(tea23i4)]
    #[test]
    fn ask_holds_are_counted_apart_from_other_hold_kinds() {
        let lc = lcmap(&[
            ("a", "REWORK", &["ask"]),
            ("b", "LANDED", &["ask"]),
            ("c", "READY", &["wait"]),
            ("d", "READY", &["poison"]),
            ("e", "READY", &["operator"]),
        ]);
        assert_eq!(ask_hold_count(&lc), 1);
        assert_eq!(poison_count(&lc), 1);
    }

    #[test_id(tim0rqt)]
    #[test]
    fn czar_outcome_follows_the_lifecycle_row() {
        let bead: Value = serde_json::from_str(r#"{"id":"z","status":"closed"}"#).unwrap();
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[("z", "WORKING", &[])]))), CzarProgress::Pending);
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[("z", "DONE", &[])]))), CzarProgress::Finished);
        assert_eq!(czar_progress(&bead, Some(&lcmap(&[]))), CzarProgress::Unknown);
        assert_eq!(czar_progress(&bead, None), CzarProgress::Unknown);
    }

    // covers: UC-cockpit-observability-13
    #[test_id(tv1eyvg)]
    #[test]
    fn repo_label_meters_count_unmapped_and_absent_and_an_unreadable_map_is_unknown() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"a","labels":["repo:known"]},{"id":"b","labels":["repo:stray"]},{"id":"c","labels":["plan"]},{"id":"d"},{"id":"e","labels":["repo:stray","repo:known"]}]"#,
        )
        .unwrap();
        let valid: std::collections::HashSet<String> = ["known".to_string()].into();
        assert_eq!(repo_label_counts(&rows, &valid), (1, 2));
        let cfg = Cfg { repo_map: "nonexistent/registry".into(), ..Cfg::default() };
        let out = repo_label_keys(&cfg);
        assert_eq!(out, vec![("SP_REPO_UNMAPPED".to_string(), "?".to_string()), ("SP_REPO_ABSENT".to_string(), "?".to_string())]);
    }

    #[test_id(tif88pe)]
    #[test]
    fn the_repo_label_probe_asks_plan_with_the_sorted_names_and_reads_the_reply() {
        assert_eq!(repo_labels_argv(&["a", "b"]), ["plan", "repo-labels", "--valid", "a,b"]);
        assert_eq!(repo_labels_from_server(r#"{"unmapped":1,"absent":2}"#), Some((1, 2)));
        assert_eq!(repo_labels_from_server(r#"{"unmapped":1}"#), None);
    }

    #[test_id(tv4mqrd)]
    #[test]
    fn well_reports_zero_not_question_mark_on_empty() {
        // well_keys hits bd for real; this only checks the pure classification helper
        // it shares with the other probes stays honest about refusal vs empty.
        assert_eq!(io::json_rows(Some("[]".to_string())), Some(vec![]));
    }

    #[test_id(tqb2sk9)]
    #[test]
    fn dwarf_alive_delegates_to_strands_canonical_predicate() {
        // Wave 4.23 (sp-0ffox) retired this crate's own cmdline check; the positive/negative
        // controls for the predicate now live with strand's own suite. This just confirms
        // the probe reaches it rather than a local reimplementation.
        let dir = testkit::TempDir::new("cockpit-collect-dwarf-alive");
        let pf = dir.join("x.pid");
        std::fs::write(&pf, "999999999").unwrap();
        assert!(!dwarf_alive(&pf), "a pid that does not exist is never alive");
    }

    fn skew_of(kv: &Kv) -> (&str, &str, &str) {
        let get = |k: &str| kv.iter().find(|(key, _)| *key == k).unwrap().1.as_str();
        (get("SP_STATUTE_DB_N"), get("SP_STATUTE_PAGE_N"), get("SP_STATUTE_SKEW"))
    }

    // covers: UC-cockpit-observability-18
    #[test_id(tyy787w)]
    #[test]
    fn statute_keys_report_unknown_without_a_wiki() {
        let kv = statute_keys(&Cfg { wiki: String::new(), ..Cfg::default() });
        assert_eq!(skew_of(&kv), ("?", "?", "?"));
    }

    // covers: UC-cockpit-observability-18
    #[test_id(tywr6va)]
    #[test]
    fn statute_skew_is_a_mismatch_when_the_book_is_under_half_the_page_and_ok_otherwise() {
        assert_eq!(skew_of(&statute_skew_keys(Some(3), Some(10))), ("3", "10", "MISMATCH:DB=3,PAGE=10"));
        assert_eq!(skew_of(&statute_skew_keys(Some(0), Some(10))).2, "MISMATCH:DB=0,PAGE=10");
        assert_eq!(skew_of(&statute_skew_keys(Some(5), Some(10))).2, "OK");
        assert_eq!(skew_of(&statute_skew_keys(Some(11), Some(10))), ("11", "10", "OK"));
        assert_eq!(skew_of(&statute_skew_keys(None, Some(10))).2, "?");
        assert_eq!(skew_of(&statute_skew_keys(Some(4), None)), ("4", "?", "?"));
    }
}

#[cfg(test)]
mod drift_tests {
    use super::{drift_repo, drift_value};

    #[test_id(tx0ite6)]
    #[test]
    fn the_checkout_read_is_spira_repo_or_else_the_one_holding_home() {
        let d = testkit::TempDir::new("drift-repo");
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::create_dir_all(d.join("spira")).unwrap();
        let checkout = d.path().canonicalize().unwrap();
        for unset in [None, Some(String::new())] {
            assert_eq!(drift_repo(unset, &d.join("spira")), checkout);
        }
        assert_eq!(drift_repo(Some("elsewhere".into()), &d.join("spira")), std::path::PathBuf::from("elsewhere"));
    }

    #[test_id(tep1yzh)]
    #[test]
    fn a_probe_that_could_not_run_is_never_read_as_clean() {
        assert_eq!(drift_value(Some(0), "", "DIRTY", "DIRTY "), "OK");
        assert_eq!(drift_value(Some(1), "DIRTY a\nDIRTY b\nnote\n", "DIRTY", "DIRTY "), "DIRTY:2");
        assert_eq!(drift_value(Some(3), "", "DIRTY", "DIRTY "), "?");
        assert_eq!(drift_value(None, "", "DIRTY", "DIRTY "), "?");
    }

    fn strand_val(kv: &[(String, String)], k: &str) -> String {
        kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    fn strand(ledger: Option<&str>) -> Vec<(String, String)> {
        super::strand_keys_of(ledger)
    }

    // covers: UC-ops-detection-remediation-26
    #[test_id(t5yvwcs)]
    #[test]
    fn the_strand_ledger_counts_ghosts_apart_from_every_other_class() {
        let kv = strand(Some(r#"{"spira,plan:ghost:sp-a":{},"spira,plan:ghost:sp-b":{},"spira,plan:empty:sp-c":{},"spira,plan:cycle:sp-d":{"escalated":5}}"#));
        assert_eq!(strand_val(&kv, "SP_STRANDS"), "4");
        assert_eq!(strand_val(&kv, "SP_STRAND_GHOST"), "2", "only ghost is a claimed bead with nobody home");
        assert_eq!(strand_val(&kv, "SP_STRAND_OTHER"), "cycle=1,empty=1");
        assert_eq!(strand_val(&kv, "SP_STRANDS_ESCALATED"), "1");
        let only_ghosts = strand(Some(r#"{"spira,plan:ghost:sp-a":{}}"#));
        assert_eq!(strand_val(&only_ghosts, "SP_STRAND_OTHER"), "none");
    }

    // covers: UC-ops-detection-remediation-26
    #[test_id(t33pvy0)]
    #[test]
    fn an_unreadable_or_unclassifiable_ledger_is_a_question_mark_before_the_split_never_a_zero() {
        for ledger in [None, Some("not json"), Some("[]")] {
            let kv = strand(ledger);
            assert_eq!(strand_val(&kv, "SP_STRAND_GHOST"), "?", "{ledger:?}");
            assert_eq!(strand_val(&kv, "SP_STRANDS"), "?", "{ledger:?}");
        }
        let kv = strand(Some(r#"{"spira,plan:ghost:sp-a":{},"no-class-here":{}}"#));
        assert_eq!(strand_val(&kv, "SP_STRAND_GHOST"), "?", "a row whose class cannot be read makes the ghost count unknowable");
        assert_eq!(strand_val(&kv, "SP_STRAND_OTHER"), "unclassified=1");
        assert_eq!(strand_val(&strand(Some("{}")), "SP_STRAND_GHOST"), "0", "an empty ledger is a true zero");
    }
}

#[cfg(test)]
mod livelock_tests {
    use super::livelock_report;

    fn val(kv: &[(String, String)], k: &str) -> String {
        kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(t67kqvj)]
    #[test]
    fn a_store_that_answers_with_nothing_flagged_reports_zero_never_a_question_mark() {
        let kv = livelock_report("", Some(""), || true);
        for k in ["SP_LIVELOCKED", "SP_INVALID_CLOSED", "SP_UNFILED_FOLLOW"] {
            assert_eq!(val(&kv, k), "0", "{k}");
        }
    }

    // covers: UC-ops-detection-remediation-29
    #[test_id(t15kipb)]
    #[test]
    fn a_store_that_does_not_answer_is_a_question_mark_not_a_clean_bill() {
        let kv = livelock_report("", None, || false);
        assert_eq!(val(&kv, "SP_LIVELOCKED"), "?");
        assert_eq!(val(&kv, "SP_INVALID_CLOSED"), "?");
    }

    #[test_id(tyx9qsj)]
    #[test]
    fn the_invalid_closed_keys_follow_the_invalid_closed_read_alone() {
        let keys = ["SP_INVALID_CLOSED", "SP_INVCLSD_N", "SP_UNFILED_FOLLOW", "SP_UNFLFLW_N"];
        let refused = livelock_report("", None, || true);
        let answered = livelock_report("", Some(""), || false);
        for k in keys {
            assert_eq!(val(&refused, k), "?", "{k}");
            assert_eq!(val(&answered, k), "0", "{k}");
        }
    }

    #[test_id(tppl1jz)]
    #[test]
    fn livelock_keys_issues_no_whole_table_read() {
        let src = include_str!("mod.rs");
        let body = &src[src.find("pub fn livelock_keys").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(!body.contains(concat!("\"--", "all\"")), "{body}");
    }

    // covers: UC-ops-detection-remediation-29
    // covers: UC-ops-detection-remediation-30
    #[test_id(tc6slhw)]
    #[test]
    fn each_detector_row_reaches_its_own_count_and_an_allowed_row_counts_in_neither() {
        let ll = "LIVELOCK sp-a ask-no-overseer — x\nLIVELOCK sp-b ci-stuck — y";
        let ic = "INVALID-CLOSED sp-c — r\nUNFILED-FOLLOW sp-d — r\nALLOWED-IC sp-e — ok";
        let kv = livelock_report(ll, Some(ic), || false);
        assert_eq!(val(&kv, "SP_LIVELOCKED"), "2");
        assert_eq!(val(&kv, "SP_INVALID_CLOSED"), "1");
        assert_eq!(val(&kv, "SP_UNFILED_FOLLOW"), "1");
        assert_eq!(val(&kv, "SP_ALLOWED_IC"), "1");
        assert!(val(&kv, "SP_INVCLSD0").starts_with("INVALID-CLOSED sp-c"));
    }
}
