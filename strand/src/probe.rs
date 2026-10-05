//! The world outside the classifier: the bead store (through bd), spira-lc, /proc,
//! systemctl, and the harness's own run files. Every payload a subprocess needs travels on
//! its stdin (law-payloads-go-on-stdin); argv carries only verbs, flags, ids and labels.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::classify::Throttle;
use crate::config::Config;
use crate::model::{parse_beads, Bead};

/// A subprocess's outcome: exit success, stdout, stderr.
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(program: &str, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Out { ok: false, stdout: String::new(), stderr: format!("{program}: {e}") },
    };
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A writer thread, so a child that fills its stdout pipe before draining stdin
        // cannot deadlock us.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    match child.wait_with_output() {
        Ok(o) => Out {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Out { ok: false, stdout: String::new(), stderr: format!("{program}: {e}") },
    }
}

// ------------------------------------------------------------------------------------------
// bd
// ------------------------------------------------------------------------------------------

/// `timeout <BD_TIMEOUT> <bd> -C <SPIRA_DB> <args>`, retried once on a pooled connection the
/// server already dropped ("invalid connection" — the query never ran). Refuses outright
/// when SPIRA_DB is unset: `bd -C ""` would walk up to whatever store it finds.
pub fn bd(cfg: &Config, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    let db = cfg
        .db
        .as_deref()
        .ok_or("refusing to run bd: SPIRA_DB is empty/unset (would fall through to bd auto-discovery)")?;
    let mut full: Vec<&str> = vec![cfg.bd_timeout.as_str(), cfg.bd.as_str(), "-C", db];
    full.extend_from_slice(args);
    let mut last = String::new();
    for _ in 0..2 {
        let o = run("timeout", &full, stdin, &[]);
        if o.ok {
            return Ok(o.stdout);
        }
        last = format!("bd {}: {}", args.first().unwrap_or(&""), o.stderr.trim());
        if !o.stderr.contains("invalid connection") {
            break;
        }
    }
    Err(last)
}

/// The whole store, closed beads included (R1): the pass's snapshot when one is readable,
/// else one `bd list --all`.
pub fn load_store(cfg: &Config) -> Result<Vec<Bead>, String> {
    if let Some(p) = cfg.list_snapshot.as_ref() {
        if let Ok(raw) = fs::read_to_string(p) {
            return parse_beads(&raw).map_err(|e| format!("{}: {e}", p.display()));
        }
    }
    let raw = bd(cfg, &["list", "--all", "--limit", "0", "--brief", "--json"], None)?;
    parse_beads(&raw).map_err(|e| format!("bd list: {e}"))
}

/// The claimable set under one partition's predicate: spira-claim's `ready-count --json`,
/// the one ready set the summoner counts and an aeon claims from (the machine's READY/REWORK
/// rows) — never bd's ready query nor the sentinel's bd-ready snapshot, whose status and
/// assignee no claim writes (sp-7g5q6). A refusal is `Err`, never empty.
pub fn ready(cfg: &Config, labels: &str, exclude: &[String]) -> Result<HashSet<String>, String> {
    let excl = exclude.join(",");
    let o = run("timeout", &["60", cfg.claim_bin.as_str(), "ready-count", labels, &excl, "--json"], None, &[]);
    if !o.ok {
        return Err(format!("spira-claim ready-count {labels}: {}", o.stderr.trim()));
    }
    Ok(parse_beads(&o.stdout).map_err(|e| format!("spira-claim ready-count {labels}: {e}"))?.into_iter().map(|b| b.id).collect())
}

pub fn show(cfg: &Config, id: &str) -> Option<Bead> {
    let raw = bd(cfg, &["show", id, "--json"], None).ok()?;
    parse_beads(&raw).ok()?.into_iter().next()
}

/// `bd sql --json <query>` → the first column of the first row, as an integer. bd sql has
/// no stdin form; every query built here carries only validated ids and fixed words.
pub fn sql_count(cfg: &Config, query: &str) -> Option<i64> {
    let raw = bd(cfg, &["sql", "--json", query], None).ok()?;
    let start = raw.find('[')?;
    let v: serde_json::Value = serde_json::from_str(&raw[start..]).ok()?;
    let row = v.as_array()?.first()?.as_object()?;
    let cell = row.values().next()?;
    cell.as_i64().or_else(|| cell.as_str()?.trim().parse().ok())
}

/// An id or actor fit to appear inside a SQL literal: bead-id characters only.
pub fn sql_safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

// ------------------------------------------------------------------------------------------
// The roster probe (DESIGN.md §2.5) — the one bash seam.
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSpec {
    pub labels: String,
    pub exclude: String,
    pub fayths: Vec<String>,
}

const ROSTER_SCRIPT: &str = r#"set -uo pipefail
. "$STRAND_LIB" >/dev/null 2>&1 || { echo "cannot source $STRAND_LIB" >&2; exit 3; }
emit() {
    local l="$1" x="$2" fs
    fs="$(fayths_for_labels "$l" | tr '\n' ' ')"
    [ -n "${fs// /}" ] || fs="$(spira_fayths)"
    printf '%s\t%s\t%s\n' "$l" "$x" "$fs"
}
if [ -n "${STRAND_WANT:-}" ]; then
    emit "$STRAND_WANT" ""
else
    fayth_partitions | while IFS=$'\t' read -r l x; do
        [ -n "$l" ] || continue
        emit "$l" "$x"
    done
fi
"#;

pub fn parse_roster(text: &str) -> Vec<PartitionSpec> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.splitn(3, '\t');
            let labels = f.next()?.trim().to_string();
            if labels.is_empty() {
                return None;
            }
            let exclude = f.next().unwrap_or("").trim().to_string();
            let fayths = f.next().unwrap_or("").split_whitespace().map(str::to_string).collect();
            Some(PartitionSpec { labels, exclude, fayths })
        })
        .collect()
}

/// Every partition this run covers, with the personas that work it. `want` narrows to one
/// partition (SPIRA_LABELS). Err when the probe itself failed — never an empty roster.
pub fn roster(cfg: &Config, want: Option<&str>) -> Result<Vec<PartitionSpec>, String> {
    let home = cfg.home.as_ref().ok_or("SPIRA_HOME is unknown (not in the environment, no [spira].prod)")?;
    let lib = home.join("lib.sh");
    if !lib.is_file() {
        return Err(format!("{} is missing — cannot read the persona roster", lib.display()));
    }
    let lib_s = lib.to_string_lossy().into_owned();
    let mut env: Vec<(&str, &str)> = vec![("STRAND_LIB", lib_s.as_str())];
    if let Some(w) = want {
        env.push(("STRAND_WANT", w));
    }
    // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
    // bin/+spira/ on the CHILD's PATH, never only inherited.
    let extra = spira_config::release_env::child_path_env_for_process();
    env.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let o = run("bash", &["-s"], Some(ROSTER_SCRIPT.as_bytes()), &env);
    if !o.ok {
        return Err(format!("roster probe failed: {}", o.stderr.trim()));
    }
    Ok(parse_roster(&o.stdout))
}

// ------------------------------------------------------------------------------------------
// Liveness and the fleet
// ------------------------------------------------------------------------------------------

fn pid_in(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A recorded pid that is still an aeon: alive AND its argv is the aeon runner — aeon.sh, or
/// the Rust binary whose argv[0] is `…/aeon` (a recycled pid must not resurrect a dead
/// aeon's claim).
pub fn aeon_alive(pf: &Path) -> bool {
    let Some(pid) = pid_in(pf) else { return false };
    let Ok(cmd) = fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    is_aeon_cmdline(&cmd)
}

pub fn is_aeon_cmdline(c: &[u8]) -> bool {
    let argv0 = String::from_utf8_lossy(c.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
    String::from_utf8_lossy(c).contains("aeon.sh") || argv0 == "aeon" || argv0.ends_with("/aeon")
}

fn pidfiles(run: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(run) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix) && n.len() > prefix.len() + suffix.len())
        })
        .collect()
}

/// Is a live process working this bead: a hold pidfile (pid alive), or an aeon pidfile
/// (pid alive and an aeon). A thin wrapper onto `sending::reap::holder_alive` — the
/// destruction chokepoint's own canonical implementation (sp-9envm) — rather than this
/// crate's own copy (wave 4.23, sp-0ffox): ONE holder-liveness predicate, not two.
pub fn holder_alive(run: &Path, id: &str) -> bool {
    sending::reap::holder_alive(run, id)
}

fn unit_count(cfg: &Config, pattern: &str, exclude: Option<&str>) -> u32 {
    let o = run("timeout", &["5", &cfg.systemctl, "--user", "list-units", pattern, "--no-legend"], None, &[]);
    o.stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| match exclude {
            Some(ex) => l.split_whitespace().next() != Some(ex),
            None => true,
        })
        .count() as u32
}

/// Live aeons of one persona: the unit list under systemd-run (the pidfile lags the unit),
/// live pidfiles otherwise. Read-only: a dead pidfile is left for its owner to reap.
///
/// `exclude` is the caller's OWN transient unit (lib.sh `aeon_count <fayth>
/// [exclude-unit]`, sp-0hnm6): systemd-run's unit exists before the capacity check ever
/// runs, so a sweep counting its own fayth was counting itself. Matched only in the
/// systemd-run branch, exactly as the bash original — the pidfile fallback never sees the
/// race it guards against (aeon.sh writes its own pidfile only after this check runs).
pub fn aeon_count(cfg: &Config, fayth: &str, exclude: Option<&str>) -> u32 {
    if cfg.summon == "systemd-run" {
        return unit_count(cfg, &format!("spira-aeon-{fayth}-*"), exclude);
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, &format!("aeon-{fayth}-"), ".pid").iter().filter(|p| aeon_alive(p)).count() as u32
}

/// How many aeons exist right now, across every persona and every lane.
pub fn aeons_live_total(cfg: &Config) -> u32 {
    if cfg.summon == "systemd-run" {
        return unit_count(cfg, "spira-aeon-*", None);
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, "aeon-", ".pid").iter().filter(|p| aeon_alive(p)).count() as u32
}

/// How many lane aeons exist right now, across all lane fayths. `FAYTH_NAME` is resolved
/// per lane fayth (default: the fayth id itself), exactly as the bash original — a lane
/// fayth is free to declare a different unit/pidfile name than its chamber filename.
pub fn aeons_live_lanes(cfg: &Config) -> u32 {
    let Some(home) = cfg.home.as_ref() else { return 0 };
    let env_override = std::env::var("SPIRA_FAYTHS").ok();
    let lanes = spira_config::chamber::spira_lane_fayths(home, env_override.as_deref());
    lanes
        .split_whitespace()
        .map(|f| {
            let name = spira_config::chamber::fayth_get(home, f, "FAYTH_NAME", f);
            aeon_count(cfg, &name, None)
        })
        .sum()
}

/// `fayth_free <fayth> [pool-remaining] [exclude-unit]` (lib.sh): how many more of this
/// persona may be summoned right now. lib.sh's OWN `fayth_free` stays bash — it calls
/// `aeon_count`/`fayth_get` BY NAME, and several suites (test-summon-fayth.sh,
/// test-aeon-elastic-concurrency.sh, test-fayth.sh, test-builder-qa-proposed.sh,
/// test-czar-partition.sh, test-dependents.sh) redefine those bash functions to control
/// `fayth_free`'s inputs without a real process table. Reducing `fayth_free` itself to a
/// one-line shim would move that arithmetic into a separate process where a bash-level
/// override can no longer reach it (the EXEC-BOUNDARY TRAP) — so this Rust copy exists for
/// any FUTURE in-process Rust caller, and lib.sh's bash body is deliberately left alone.
pub fn fayth_free(cfg: &Config, fayth: &str, pool: Option<i64>, exclude: Option<&str>) -> i64 {
    let Some(home) = cfg.home.as_ref() else { return 0 };
    let mut max: i64 = spira_config::chamber::fayth_get(home, fayth, "FAYTH_MAX_CONCURRENT", "1").trim().parse().unwrap_or(1);
    let elastic = spira_config::chamber::fayth_get(home, fayth, "FAYTH_ELASTIC", "0") == "1";
    let is_remainder = elastic && pool.is_some();
    if is_remainder {
        max = pool.unwrap();
    }
    let have: i64 = aeon_count(cfg, fayth, exclude).into();
    let mut free = if is_remainder { max } else { (max - have).max(0) };
    if let Some(p) = pool {
        if p < free {
            free = p;
        }
    }
    free
}

// ------------------------------------------------------------------------------------------
// Correct refusals the starved rule must recognise
// ------------------------------------------------------------------------------------------

/// Some(detail) while the capacity pause runs. Read-only (DESIGN.md §2.3).
pub fn capacity(cfg: &Config, now: i64) -> Option<String> {
    let path = cfg.capacity_pause_path()?;
    let text = fs::read_to_string(path).ok()?;
    let at: i64 = text.lines().next()?.split_whitespace().next()?.parse().ok()?;
    (at > now).then(|| {
        format!(
            "the account is out for another {}s (until {})",
            at - now,
            crate::timefmt::local_hhmm(at)
        )
    })
}

pub fn throttle(cfg: &Config) -> Throttle {
    let Some(stamp) = cfg.throttle_stamp_path() else {
        return Throttle::Open;
    };
    if fs::symlink_metadata(&stamp).is_err() {
        return Throttle::Open;
    }
    let line = fs::read(&stamp)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).lines().next().unwrap_or("").to_string())
        .filter(|l| !l.is_empty());
    match line {
        Some(l) => {
            let depth = l
                .match_indices("depth=")
                .find_map(|(i, _)| {
                    let digits: String = l[i + 6..].chars().take_while(char::is_ascii_digit).collect();
                    (!digits.is_empty()).then_some(digits)
                })
                .unwrap_or_else(|| "?".into());
            Throttle::Shut(format!("depth {} >= release-at {}", depth, cfg.throttle_release_at))
        }
        None => Throttle::Unreadable(format!("{} exists but could not be read", stamp.display())),
    }
}

pub fn throttle_line(t: &Throttle) -> String {
    match t {
        Throttle::Open => "open\t".into(),
        Throttle::Shut(d) => format!("shut\t{d}"),
        Throttle::Unreadable(d) => format!("unreadable\t{d}"),
    }
}

/// The sentinel's pass-start marker: its `state: open=<n> …` line (sp-k6m1m dropped the
/// `goal=` field that used to lead it).
pub const PASS_MARKER: &str = ": state: open=";

/// Did the last sentinel pass skip this partition's personas ("pass budget exhausted")?
pub fn pass_truncated(log_text: &str, fayths: &[String]) -> bool {
    let lines: Vec<&str> = log_text.lines().collect();
    let Some(start) = lines.iter().rposition(|l| l.contains(PASS_MARKER)) else {
        return false;
    };
    fayths.iter().any(|f| {
        let needle = format!("CHECK7 {f}: not evaluated");
        lines[start..].iter().any(|l| l.contains(&needle))
    })
}

// ------------------------------------------------------------------------------------------
// The harness's pulse (report header)
// ------------------------------------------------------------------------------------------

fn unit_known(cfg: &Config, unit: &str) -> bool {
    run(&cfg.systemctl, &["--user", "is-enabled", unit], None, &[]).ok
        || run(&cfg.systemctl, &["--user", "is-active", unit], None, &[]).ok
}

/// conf.sh spira_unit: the instance-suffixed unit if systemd knows it, else the plain one,
/// else `?`.
pub fn spira_unit(cfg: &Config, base: &str, t: &str) -> String {
    let plain = format!("spira-{base}.{t}");
    let inst = match cfg.instance.as_deref() {
        Some(i) => format!("spira-{base}-{i}.{t}"),
        None => plain.clone(),
    };
    if unit_known(cfg, &inst) {
        inst
    } else if unit_known(cfg, &plain) {
        plain
    } else {
        "?".into()
    }
}

/// (sentinel timer state, seconds since the sentinel log was written or -1).
pub fn harness_state(cfg: &Config, now: i64) -> (String, i64) {
    let unit = spira_unit(cfg, "sentinel", "timer");
    let active = if unit == "?" {
        "unknown".to_string()
    } else {
        let o = run(&cfg.systemctl, &["--user", "is-active", &unit], None, &[]);
        let s = o.stdout.trim().to_string();
        if s.is_empty() { "unknown".into() } else { s }
    };
    let age = cfg
        .sentinel_log()
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| now - d.as_secs() as i64)
        .unwrap_or(-1);
    (active, age)
}

#[cfg(test)]
mod tests {
    #[test]
    fn aeon_cmdline_matches_the_script_and_the_binary_only() {
        assert!(super::is_aeon_cmdline(b"bash\0/h/spira/aeon.sh\0builder\0"));
        assert!(super::is_aeon_cmdline(b"/r/current/bin/aeon\0--home\0/r/current/spira\0builder\0"));
        assert!(!super::is_aeon_cmdline(b"/usr/bin/sleep\0aeon\0"));
        assert!(!super::is_aeon_cmdline(b"/r/bin/aeonic\0"));
    }

    use super::*;
    use crate::config::Source;
    use std::collections::HashMap;

    // ---- holder_alive: delegates to sending::reap (wave 4.23, sp-0ffox) -------------------

    #[test]
    fn holder_alive_is_sendings_own_predicate() {
        let run = testkit::TempDir::new("strand-holder-alive");
        std::fs::write(run.join("hold-sp-h1.pid"), std::process::id().to_string()).unwrap();
        assert!(holder_alive(&run, "sp-h1"));
        assert!(!holder_alive(&run, "sp-h2"), "nothing holds sp-h2");
        // A live pid whose argv is not an aeon must not resurrect a recycled pid's claim —
        // the same guarantee sending::reap's own suite proves; this just confirms the
        // delegation reaches it rather than a local reimplementation.
        std::fs::write(run.join("aeon-builder-sp-a1.pid"), std::process::id().to_string()).unwrap();
        assert!(!holder_alive(&run, "sp-a1"));
    }

    // ---- aeon_count / aeons_live_lanes / fayth_free (wave 4.23, sp-0ffox) ------------------

    struct Env(HashMap<&'static str, String>);
    impl Source for Env {
        fn env(&self, k: &str) -> Option<String> {
            self.0.get(k).cloned()
        }
        fn toml(&self, _: &str) -> Option<String> {
            None
        }
    }

    /// `systemctl --user list-units <pattern> --no-legend`, stubbed: echoes `lines`
    /// regardless of its argv, so the test controls the fleet without a real systemd
    /// session (SPIRA_SYSTEMCTL).
    fn mock_systemctl(dir: &std::path::Path, lines: &[&str]) -> String {
        let p = dir.join("mock-systemctl");
        let body = format!("#!/bin/sh\n{}\n", lines.iter().map(|l| format!("echo '{l}'")).collect::<Vec<_>>().join("\n"));
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(&p, &body);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn aeon_count_excludes_only_the_named_unit_in_the_systemd_run_branch() {
        let t = testkit::TempDir::new("strand-aeon-count-excl");
        let sc = mock_systemctl(
            &t,
            &["spira-aeon-builder-1.service loaded active running x", "spira-aeon-builder-2.service loaded active running x"],
        );
        let cfg = Config::resolve(&Env(HashMap::from([("SPIRA_SYSTEMCTL", sc)])));
        assert_eq!(cfg.summon, "systemd-run", "the default — exercises the unit-list branch");
        assert_eq!(aeon_count(&cfg, "builder", None), 2);
        assert_eq!(aeon_count(&cfg, "builder", Some("spira-aeon-builder-1.service")), 1, "the caller's own unit is excluded");
        assert_eq!(aeon_count(&cfg, "builder", Some("no-such-unit")), 2, "excluding a unit that isn't there changes nothing");
    }

    #[test]
    fn aeon_count_pid_fallback_ignores_exclude_exactly_as_the_bash_original() {
        let t = testkit::TempDir::new("strand-aeon-count-fallback");
        let run = t.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let cfg = Config::resolve(&Env(HashMap::from([
            ("SPIRA_SUMMON", "mock".to_string()),
            ("SPIRA_RUN", run.to_string_lossy().into_owned()),
        ])));
        assert_eq!(aeon_count(&cfg, "builder", None), 0, "no pidfiles at all");
        assert_eq!(aeon_count(&cfg, "builder", Some("anything")), 0, "exclude is a no-op off the systemd-run branch");
    }

    #[test]
    fn aeons_live_lanes_resolves_fayth_name_and_sums_lane_units() {
        let t = testkit::TempDir::new("strand-live-lanes");
        std::fs::create_dir_all(t.join("chamber")).unwrap();
        // A lane fayth declaring a different unit name than its chamber filename —
        // aeons_live_lanes must resolve FAYTH_NAME, not use the fayth id verbatim.
        std::fs::write(t.join("chamber/laner.fayth"), "FAYTH_LANE=incident\nFAYTH_NAME=siren\n").unwrap();
        std::fs::write(t.join("chamber/builder.fayth"), "").unwrap(); // not a lane — must not be counted
        let sc = mock_systemctl(&t, &["spira-aeon-siren-1.service loaded active running x"]);
        let cfg = Config::resolve(&Env(HashMap::from([
            ("SPIRA_HOME", t.to_string_lossy().into_owned()),
            ("SPIRA_SYSTEMCTL", sc),
        ])));
        assert_eq!(aeons_live_lanes(&cfg), 1);
    }

    #[test]
    fn fayth_free_matches_the_bash_originals_arithmetic() {
        let t = testkit::TempDir::new("strand-fayth-free");
        std::fs::create_dir_all(t.join("chamber")).unwrap();
        std::fs::write(t.join("chamber/stretchy.fayth"), "FAYTH_MAX_CONCURRENT=4\nFAYTH_ELASTIC=1\n").unwrap();
        std::fs::write(t.join("chamber/anchor.fayth"), "FAYTH_MAX_CONCURRENT=10\n").unwrap();
        std::fs::write(t.join("chamber/laner.fayth"), "FAYTH_MAX_CONCURRENT=1\n").unwrap();
        let run = t.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let cfg = Config::resolve(&Env(HashMap::from([
            ("SPIRA_HOME", t.to_string_lossy().into_owned()),
            ("SPIRA_SUMMON", "mock".to_string()), // pid fallback, no real aeons: have=0 throughout
            ("SPIRA_RUN", run.to_string_lossy().into_owned()),
        ])));
        // elastic: pool remainder whole, never subtracting running twice (have is ignored
        // here since the stub always reports 0 — the G18 regression this guards against is
        // "have" double-counted against a pool that already nets it out).
        assert_eq!(fayth_free(&cfg, "stretchy", Some(5), None), 5);
        // no pool: falls back to FAYTH_MAX_CONCURRENT.
        assert_eq!(fayth_free(&cfg, "stretchy", None, None), 4);
        // non-elastic: cap minus running (0), clamped at 0 from below.
        assert_eq!(fayth_free(&cfg, "anchor", Some(10), None), 10);
        assert_eq!(fayth_free(&cfg, "laner", None, None), 1);
    }

    #[test]
    fn roster_lines() {
        let r = parse_roster("spira,plan\tspira-poison,ask-x\tbuilder \nspira,incident\t\tops\n\n");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].labels, "spira,plan");
        assert_eq!(r[0].exclude, "spira-poison,ask-x");
        assert_eq!(r[0].fayths, vec!["builder"]);
        assert_eq!(r[1].exclude, "");
    }

    #[test]
    fn truncation_reads_only_the_last_pass() {
        let log = "a: state: open=1\nCHECK7 builder: not evaluated (pass budget exhausted)\nb: state: open=2\nCHECK7 ops: ok\n";
        assert!(!pass_truncated(log, &["builder".into()]));
        let log2 = format!("{log}CHECK7 builder: not evaluated (pass budget exhausted)\n");
        assert!(pass_truncated(&log2, &["builder".into()]));
        assert!(!pass_truncated("no passes", &["builder".into()]));
    }

    #[test]
    fn sql_safety() {
        assert!(sql_safe("sp-zs04v.5"));
        assert!(!sql_safe("x'; drop table events; --"));
        assert!(!sql_safe(""));
    }

    #[test]
    fn throttle_states_from_the_stamp() {
        use crate::config::{Config, Source};
        struct S(PathBuf);
        impl Source for S {
            fn env(&self, k: &str) -> Option<String> {
                (k == "SPIRA_THROTTLE_STAMP").then(|| self.0.to_string_lossy().into_owned())
            }
            fn toml(&self, _: &str) -> Option<String> {
                None
            }
        }
        let dir = testkit::TempDir::new("strand-throttle");
        let stamp = dir.join("queue-throttled");
        let cfg = Config::resolve(&S(stamp.clone()));
        assert_eq!(throttle_line(&throttle(&cfg)), "open\t");
        fs::write(&stamp, "2026-09-29T00:00:00Z depth=14 x\n").unwrap();
        assert_eq!(throttle_line(&throttle(&cfg)), "shut\tdepth 14 >= release-at 8");
        fs::write(&stamp, "").unwrap();
        assert!(throttle_line(&throttle(&cfg)).starts_with("unreadable\t"));
        fs::write(&stamp, "no depth here\n").unwrap();
        assert_eq!(throttle_line(&throttle(&cfg)), "shut\tdepth ? >= release-at 8");
        fs::remove_dir_all(&dir).unwrap();
    }
}
