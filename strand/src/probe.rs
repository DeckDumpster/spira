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

fn split_labels(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

/// The claimable set under one partition's predicate.
pub fn ready(cfg: &Config, labels: &str, exclude: &[String]) -> Result<HashSet<String>, String> {
    let need = split_labels(labels);
    if let Some(p) = cfg.ready_snapshot.as_ref() {
        if let Ok(raw) = fs::read_to_string(p) {
            let beads = parse_beads(&raw).map_err(|e| format!("{}: {e}", p.display()))?;
            return Ok(beads
                .into_iter()
                .filter(|b| cfg.scope_label.is_empty() || b.has(&cfg.scope_label))
                .filter(|b| need.iter().all(|l| b.has(l)))
                .filter(|b| !exclude.iter().any(|x| b.has(x)))
                .map(|b| b.id)
                .collect());
        }
    }
    let excl = exclude.join(",");
    let mut args: Vec<&str> = vec!["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"];
    if !cfg.scope_label.is_empty() {
        args.extend(["--label", cfg.scope_label.as_str()]);
    }
    if !cfg.no_loop_label.is_empty() {
        args.extend(["--exclude-label", cfg.no_loop_label.as_str()]);
    }
    args.extend(["--label", labels]);
    if !excl.is_empty() {
        args.extend(["--exclude-label", excl.as_str()]);
    }
    args.push("--json");
    let raw = bd(cfg, &args, None)?;
    Ok(parse_beads(&raw).map_err(|e| format!("bd ready: {e}"))?.into_iter().map(|b| b.id).collect())
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

fn hold_alive(pf: &Path) -> bool {
    pid_in(pf).is_some_and(|pid| Path::new(&format!("/proc/{pid}")).is_dir())
}

/// A recorded pid that is still an aeon: alive AND its argv names aeon.sh (a recycled pid
/// must not resurrect a dead aeon's claim).
pub fn aeon_alive(pf: &Path) -> bool {
    let Some(pid) = pid_in(pf) else { return false };
    let Ok(cmd) = fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    String::from_utf8_lossy(&cmd).contains("aeon.sh")
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
/// (pid alive and an aeon).
pub fn holder_alive(run: &Path, id: &str) -> bool {
    if hold_alive(&run.join(format!("hold-{id}.pid"))) {
        return true;
    }
    pidfiles(run, "aeon-", &format!("-{id}.pid")).iter().any(|p| aeon_alive(p))
}

fn unit_count(cfg: &Config, pattern: &str) -> u32 {
    let o = run(&cfg.systemctl, &["--user", "list-units", pattern, "--no-legend"], None, &[]);
    o.stdout.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

/// Live aeons of one persona: the unit list under systemd-run (the pidfile lags the unit),
/// live pidfiles otherwise. Read-only: a dead pidfile is left for its owner to reap.
pub fn aeon_count(cfg: &Config, fayth: &str) -> u32 {
    if cfg.summon == "systemd-run" {
        return unit_count(cfg, &format!("spira-aeon-{fayth}-*"));
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, &format!("aeon-{fayth}-"), ".pid").iter().filter(|p| aeon_alive(p)).count() as u32
}

pub fn aeons_live_total(cfg: &Config) -> u32 {
    if cfg.summon == "systemd-run" {
        return unit_count(cfg, "spira-aeon-*");
    }
    let Some(run_dir) = cfg.run.as_ref() else { return 0 };
    pidfiles(run_dir, "aeon-", ".pid").iter().filter(|p| aeon_alive(p)).count() as u32
}

/// The legacy wait exemption: the label CHECK 2 put on a bead waiting on the operator before
/// sp-i2m7y moved it onto a spira-lc `wait` hold (`SPIRA_RECLAIM_SKIP_LABEL`'s default; the
/// key itself is retired, so the literal is the contract).
pub const WAIT_LABEL: &str = "spira-waiting-operator"; // literal-ok: retired key's default

/// Beads the ghost check must not reclaim because they are legitimately waiting.
///
/// `lifecycle_enforce` off: the beads carrying [`WAIT_LABEL`], read off the store the pass
/// already loaded — spira-lc is never run. On: the spira-lc `wait` holds, and an absent or
/// non-executable binary, a failed call or an unparseable reply is an `Err` (the caller's
/// "cannot tell"), never an empty set that would let the ghost check reclaim a held bead.
pub fn wait_held(cfg: &Config, beads: &[Bead]) -> Result<HashSet<String>, String> {
    if !cfg.lifecycle_enforce {
        return Ok(beads.iter().filter(|b| b.has(WAIT_LABEL)).map(|b| b.id.clone()).collect());
    }
    let unreachable = |why: String| {
        format!("lifecycle_enforce is on and spira-lc is unreachable ({why}) — cannot read the wait holds")
    };
    let bin = cfg.lc_bin.as_deref().ok_or_else(|| unreachable("SPIRA_LC_BIN unset".into()))?;
    if !is_executable(Path::new(bin)) {
        return Err(unreachable(format!("{bin} is not executable")));
    }
    let o = run("timeout", &["30", bin, "list", "--hold", "wait"], None, &[]);
    if !o.ok {
        return Err(unreachable(format!("list --hold wait: {}", o.stderr.trim())));
    }
    let v: serde_json::Value = serde_json::from_str(&o.stdout).map_err(|e| unreachable(format!("unparseable reply: {e}")))?;
    Ok(v.as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.get("bead_id").and_then(|x| x.as_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .collect())
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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

/// Did the last sentinel pass skip this partition's personas ("pass budget exhausted")?
pub fn pass_truncated(log_text: &str, fayths: &[String]) -> bool {
    let lines: Vec<&str> = log_text.lines().collect();
    let Some(start) = lines.iter().rposition(|l| l.contains(": state: goal=")) else {
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
    use super::*;

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
        let log = "a: state: goal=x\nCHECK7 builder: not evaluated (pass budget exhausted)\nb: state: goal=y\nCHECK7 ops: ok\n";
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
        let dir = std::env::temp_dir().join(format!("strand-throttle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
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
