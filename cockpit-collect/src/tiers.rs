//! The in-process tiers: what the scheduler's cockpit tier tasks call. [`observe`] runs a
//! tier's probes as library calls, each on its own thread under its own timeout, and answers
//! each probe's fragment by the supervisor's lifecycle (`ok`, `stale` with the last good
//! values, `error`/`timeout` with none); [`publish`] writes one tier's merged `Kv` and merges
//! every tier into `run/cockpit.env`, the pane's read contract, then appends the history row.
//!
//! Replaces the supervisor's child chain for these probes: no self re-exec, no `timeout`, no
//! `cockpit-collect probe <name>`. A probe still running when asked again is skipped, not
//! stacked: it answers as a fault, exactly as a killed probe did.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::probes::{self, Kv};
use crate::{io, quoting};

/// One probe's answer, in the supervisor's fragment terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    /// The probe's registry name (`now`, `round`, …).
    pub name: String,
    /// `ok`, `stale`, `error` or `timeout`.
    pub status: &'static str,
    /// When the values were read (the last good read for a `stale` fragment); 0 for none.
    pub at: i64,
    /// Whether this answer is a fault (the probe overran or failed this time).
    pub killed: bool,
    pub kv: Kv,
}

/// The tier fragments' directory under the run dir: one file per tier task.
pub const TIER_DIR: &str = "cockpit.tiers";

struct Memory {
    running: HashSet<String>,
    last_good: HashMap<String, (i64, Kv)>,
}

fn memory() -> &'static Mutex<Memory> {
    static M: OnceLock<Mutex<Memory>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Memory { running: HashSet::new(), last_good: HashMap::new() }))
}

fn lock() -> std::sync::MutexGuard<'static, Memory> {
    memory().lock().unwrap_or_else(|p| p.into_inner())
}

/// The fault answer: the last good values as `stale`, or none as `fault` when there are none.
fn fault(name: &str, fault: &'static str) -> Fragment {
    match lock().last_good.get(name) {
        Some((at, kv)) => Fragment { name: name.into(), status: "stale", at: *at, killed: true, kv: kv.clone() },
        None => Fragment { name: name.into(), status: fault, at: 0, killed: true, kv: Kv::new() },
    }
}

/// Every probe in `probes` (registry name, timeout), run at once, each answered within its own
/// timeout. A probe whose previous run is still going is not started again.
pub fn observe(probes: &[(String, Duration)]) -> Vec<Fragment> {
    observe_with(probes, |name| {
        let cfg = probes::Cfg::load().ok()?;
        probes::run(name, &cfg)
    })
}

/// [`observe`] over `run`, the probe library or a test's stand-in.
pub fn observe_with(probes: &[(String, Duration)], run: fn(&str) -> Option<Kv>) -> Vec<Fragment> {
    let start = Instant::now();
    let mut waiting = vec![];
    for (name, timeout) in probes {
        if !lock().running.insert(name.clone()) {
            waiting.push((name.clone(), *timeout, None));
            continue;
        }
        let (tx, rx) = mpsc::channel();
        let n = name.clone();
        let spawned = std::thread::Builder::new().name(format!("cockpit-{n}")).spawn(move || {
            let at = io::now();
            let got = std::panic::catch_unwind(|| run(&n)).ok().flatten();
            let mut m = lock();
            m.running.remove(&n);
            if let Some(kv) = &got {
                m.last_good.insert(n.clone(), (at, kv.clone()));
            }
            drop(m);
            let _ = tx.send(got.map(|kv| (at, kv)));
        });
        match spawned {
            Ok(_) => waiting.push((name.clone(), *timeout, Some(rx))),
            Err(_) => {
                lock().running.remove(name);
                waiting.push((name.clone(), *timeout, None));
            }
        }
    }
    waiting
        .into_iter()
        .map(|(name, timeout, rx)| {
            let Some(rx) = rx else { return fault(&name, "timeout") };
            match rx.recv_timeout(timeout.saturating_sub(start.elapsed())) {
                Ok(Some((at, kv))) => Fragment { name, status: "ok", at, killed: false, kv },
                Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => fault(&name, "error"),
                Err(mpsc::RecvTimeoutError::Timeout) => fault(&name, "timeout"),
            }
        })
        .collect()
}

fn parse_lines(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once('=')).filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    io::replace_if_changed(path, content).map(|_| ()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every tier's merged `Kv`, tiers in name order, the first value of a key winning.
fn merged_tiers(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "env")).collect();
    files.sort();
    let mut seen = HashSet::new();
    files.iter().flat_map(|f| parse_lines(&std::fs::read_to_string(f).unwrap_or_default())).filter(|(k, _)| seen.insert(k.clone())).collect()
}

/// Writes tier `tier`'s merged `Kv` (raw `KEY=value` lines) and merges every tier's into
/// `run/cockpit.env`, quoted as the supervisor's merge quoted it, plus `SP_WRITER` and
/// `SP_COLLECTOR_REV`; then appends the history row. Answers the snapshot's path.
pub fn publish(run_dir: &Path, tier: &str, kv_text: &str) -> Result<String, String> {
    if tier.is_empty() || tier.contains('/') {
        return Err(format!("not a tier name: {tier:?}"));
    }
    let dir = run_dir.join(TIER_DIR);
    write_atomic(&dir.join(format!("{tier}.env")), kv_text)?;
    let mut out = String::new();
    for (k, v) in merged_tiers(&dir) {
        out.push_str(&format!("{k}={}\n", quoting::self_quote(&v)));
    }
    out.push_str(&format!("SP_WRITER={}\n", quoting::self_quote(&format!("{}:spira-runtime", std::process::id()))));
    // FORK-EDGE: git rev-parse of the home checkout, as the supervisor's merge read it.
    let rev = io::try_home_dir().ok().and_then(|h| io::git(&h, &["rev-parse", "--short", "HEAD"])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    out.push_str(&format!("SP_COLLECTOR_REV={}\n", quoting::self_quote(&rev)));
    let snap = run_dir.join("cockpit.env");
    write_atomic(&snap, &out)?;
    append_history(run_dir);
    Ok(snap.display().to_string())
}

/// The `slots` tsd row, from `field=value` lines (`live`, `ceiling`, `lanes_live`, `ready`,
/// `capacity_paused`); a missing field is `?`.
pub fn slots_sample(run_dir: &Path, fields_text: &str) -> Result<String, String> {
    let given: BTreeMap<String, String> = parse_lines(fields_text).into_iter().collect();
    let fields: Vec<(&str, String)> = ["live", "ceiling", "lanes_live", "ready", "capacity_paused"].iter().map(|k| (*k, given.get(*k).cloned().unwrap_or_else(|| "?".into()))).collect();
    io::slots_row(run_dir, &fields, &tsd::iso_utc(io::now() as u64), &tsd::default_host())?;
    Ok(String::new())
}

/// `append_history`: read the snapshot back (never the probe's own in-memory output — the
/// snapshot is what every other reader sees) and append one row, rotating the file on a
/// column-set change rather than overwriting it.
pub fn append_history(run_dir: &Path) {
    let snap_path = run_dir.join("cockpit.env");
    let snap: HashMap<String, String> = std::fs::read_to_string(&snap_path).unwrap_or_default().lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.trim_matches('\'').to_string())).collect();
    let get = |k: &str| snap.get(k).cloned().unwrap_or_else(|| "?".to_string());
    let at = snap.get("SP_AT").cloned().unwrap_or_else(|| io::now().to_string());
    let row = format!(
        "{at},{},{},{},{},{},{},{},{}",
        get("SP_TOK_WIN"),
        get("SP_TOK_DWARF_WIN"),
        get("SP_TOK_SESS_WIN"),
        get("SP_TOK_DWARF_TURNS"),
        get("SP_TOK_SESS_TURNS"),
        get("SP_CTX_NOW"),
        get("SP_RATELIM_5H"),
        get("SP_RATELIM_7D"),
    );
    const HIST_COLS: &str = "ts,tok_win,tok_dwarf_win,tok_sess_win,tok_dwarf_turns,tok_sess_turns,ctx_now,ratelim_5h,ratelim_7d";
    let hist_path = run_dir.join("cockpit-history.csv");
    let existing = std::fs::read_to_string(&hist_path).ok();
    let needs_header = match &existing {
        None => true,
        Some(c) => c.lines().next() != Some(HIST_COLS),
    };
    if needs_header {
        if let Some(c) = &existing {
            if !c.is_empty() {
                let _ = std::fs::rename(&hist_path, run_dir.join(format!("cockpit-history.csv.{}", io::now())));
            }
        }
        let _ = std::fs::write(&hist_path, format!("{HIST_COLS}\n"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&hist_path) {
        use std::io::Write;
        let _ = writeln!(f, "{row}");
    }

    let max: usize = std::env::var("SPIRA_COCKPIT_HISTORY_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(20160);
    if let Ok(content) = std::fs::read_to_string(&hist_path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > max + 1 + max / 10 {
            let mut kept = String::new();
            kept.push_str(lines[0]);
            kept.push('\n');
            for l in &lines[lines.len() - max..] {
                kept.push_str(l);
                kept.push('\n');
            }
            let _ = std::fs::write(&hist_path, kept);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Holds the `slow` probe until the test opens it, so an overrun is a fact of the test and
    /// not of the box's load.
    fn gate() -> &'static (Mutex<bool>, std::sync::Condvar) {
        static G: OnceLock<(Mutex<bool>, std::sync::Condvar)> = OnceLock::new();
        G.get_or_init(|| (Mutex::new(false), std::sync::Condvar::new()))
    }

    fn open_gate() {
        let (m, cv) = gate();
        *m.lock().unwrap() = true;
        cv.notify_all();
    }

    fn quick(name: &str) -> Option<Kv> {
        match name {
            "fast" => Some(vec![("SP_FAST".into(), "1".into())]),
            "slow" => {
                let (m, cv) = gate();
                let _open = cv.wait_while(m.lock().unwrap(), |open| !*open).unwrap();
                Some(vec![("SP_SLOW".into(), "1".into())])
            }
            "boom" => panic!("probe failed"),
            _ => None,
        }
    }

    #[test]
    fn republishing_an_unchanged_tier_writes_no_snapshot_data() {
        let run = testkit::TempDir::new("tier-nowrite");
        publish(run.path(), "5s", "SP_A=1\n").unwrap();
        let before = io::data_writes();
        publish(run.path(), "5s", "SP_A=1\n").unwrap();
        assert_eq!(io::data_writes(), before);
        publish(run.path(), "5s", "SP_A=2\n").unwrap();
        assert_eq!(io::data_writes(), before + 2, "tier file and merged snapshot both changed");
    }

    #[test]
    fn history_is_trimmed_only_past_its_slack_and_back_to_the_cap() {
        let run = testkit::TempDir::new("hist-trim");
        let p = run.path().join("cockpit-history.csv");
        let rows = |_: usize| std::fs::read_to_string(&p).unwrap().lines().count() - 1;
        for _ in 0..25 {
            append_history(run.path());
        }
        assert_eq!(rows(0), 25, "below cap + slack nothing is trimmed");
        {
            let _env = testkit::env(&[("SPIRA_COCKPIT_HISTORY_MAX", Some("20"))]);
            append_history(run.path());
        }
        assert_eq!(rows(0), 20, "past cap + slack it is cut back to the cap");
    }

    #[test]
    fn each_probe_answers_ok_timeout_or_error_and_an_overrun_is_skipped_then_stale() {
        let probes = |slow_ms| vec![("fast".to_string(), Duration::from_secs(5)), ("slow".to_string(), Duration::from_millis(slow_ms)), ("boom".to_string(), Duration::from_secs(5))];
        let got = observe_with(&probes(50), quick);
        let status: Vec<(&str, &str)> = got.iter().map(|f| (f.name.as_str(), f.status)).collect();
        assert_eq!(status, [("fast", "ok"), ("slow", "timeout"), ("boom", "error")]);
        assert_eq!(got[0].kv, vec![("SP_FAST".to_string(), "1".to_string())]);
        let again = observe_with(&probes(50), quick);
        assert_eq!(again[1].status, "timeout", "a probe still running is skipped, not stacked");
        open_gate();
        let mut done = false;
        for _ in 0..1000 {
            if !lock().running.contains("slow") {
                done = true;
                break;
            }
            // clock-safe: a cap on the wait for the named signal, the slow probe leaving `running`
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(done, "the released slow probe finished");
        let third = observe_with(&probes(5_000), quick);
        assert_eq!((third[1].status, third[1].killed), ("ok", false));
        lock().running.insert("slow".into());
        let fourth = observe_with(&probes(5_000), quick);
        assert_eq!((fourth[1].status, fourth[1].kv.len()), ("stale", 1), "a fault after a good read keeps the last values");
        lock().running.remove("slow");
    }

    #[test]
    fn publish_merges_every_tier_into_the_snapshot_and_appends_history() {
        let dir = testkit::TempDir::new("cc-tiers");
        publish(dir.path(), "cockpit-tier-60s", "SP_SLOTS_LIVE=2\nSP_AT=9\n").unwrap();
        let snap = publish(dir.path(), "cockpit-tier-5s", "SP_AT=100\nSP_ROUND=it's\n").unwrap();
        let text = std::fs::read_to_string(&snap).unwrap();
        assert!(text.contains("SP_AT='100'\n") && !text.contains("SP_AT='9'"), "the first tier by name wins a shared key: {text}");
        assert!(text.contains("SP_SLOTS_LIVE='2'\n") && text.contains("SP_ROUND='it'\\''s'\n") && text.contains("SP_WRITER="), "{text}");
        let hist = std::fs::read_to_string(dir.path().join("cockpit-history.csv")).unwrap();
        assert_eq!(hist.lines().count(), 3, "{hist}");
        assert!(publish(dir.path(), "../x", "").is_err());
    }
}
