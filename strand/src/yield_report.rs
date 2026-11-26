//! What the landing gate is worth: `spira/yield.sh report`, computed in the caller's process.
//! The script still writes the records this reads; the two must give the same keys and values.

use crate::timefmt::parse_rfc3339;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Where the reading comes from, with the script's own defaults.
pub struct Sources {
    pub dir: PathBuf,
    pub gate_log: PathBuf,
    pub window_s: u64,
}

impl Sources {
    pub fn resolve(run: &Path, get: impl Fn(&str) -> Option<String>) -> Sources {
        let set = |k: &str| get(k).filter(|v| !v.is_empty());
        Sources {
            dir: set("SPIRA_YIELD").map_or_else(|| run.join("gate-yield"), PathBuf::from),
            gate_log: set("SPIRA_GATE_LOG").map_or_else(|| run.join("gate.log"), PathBuf::from),
            window_s: set("SPIRA_YIELD_WINDOW").and_then(|v| v.parse().ok()).unwrap_or(86400),
        }
    }
}

const COUNT_KEYS: [&str; 6] = ["YIELD_REDS", "YIELD_DEFECT", "YIELD_FAULT", "YIELD_UNKNOWN", "YIELD_DEFECT_INFERRED", "YIELD_TOP_FAULT"];

/// The script's `KEY=value` pairs, in its order. A value nobody could read is `?`, never `0`.
pub fn report(dir: &Path, gate_log: &Path, window_s: u64, now_s: u64) -> Vec<(String, String)> {
    let (now, window) = (now_s as i64, window_s as i64);
    let cost = std::fs::read(gate_log).ok().map(|raw| cost(&String::from_utf8_lossy(&raw), now, window));
    let meter_reds = cost.as_ref().map(|c| c.log_reds);
    let tally = dir.is_dir().then(|| tally(dir, now - window));
    // The record's writer can stop silently; the meter writes on every exit, so reds there
    // with none here means the zeros are not a clean sheet.
    let recorder = match (meter_reds, &tally) {
        (None, _) => "?",
        (Some(0), None) => "absent",
        (Some(0), Some(_)) => "ok",
        (Some(_), Some(t)) if t.reds > 0 => "ok",
        (Some(_), _) => "silent",
    };
    let counts = match tally {
        Some(t) if recorder != "silent" => [t.reds.to_string(), t.defect.to_string(), t.fault.to_string(), t.unknown.to_string(), t.inferred.to_string(), t.top_fault()],
        _ => unknown(),
    };

    let mut out = vec![("YIELD_WINDOW".to_string(), window_s.to_string())];
    out.extend(COUNT_KEYS.iter().map(|k| k.to_string()).zip(counts));
    out.push(("YIELD_RECORDER".to_string(), recorder.to_string()));
    for (name, costs) in [("SOLO", cost.as_ref().map(|c| &c.solo)), ("CONC", cost.as_ref().map(|c| &c.contended))] {
        let stat = costs.map_or_else(unknown, |c| stat(c));
        out.extend(["N", "MED", "MAX"].iter().zip(stat).map(|(s, v)| (format!("YIELD_{name}_{s}"), v)));
    }
    out.push(("YIELD_LOG_REDS".to_string(), meter_reds.map_or_else(|| "?".to_string(), |n| n.to_string())));
    out
}

fn unknown<const N: usize>() -> [String; N] {
    std::array::from_fn(|_| "?".to_string())
}

#[derive(Default)]
struct Tally {
    reds: u64,
    defect: u64,
    inferred: u64,
    fault: u64,
    unknown: u64,
    faults: BTreeMap<String, u64>,
}

impl Tally {
    fn top_fault(&self) -> String {
        let mut top: Option<(&String, u64)> = None;
        for (name, n) in &self.faults {
            if top.is_none_or(|(_, most)| *n > most) {
                top = Some((name, *n));
            }
        }
        top.map_or_else(|| "-".to_string(), |(name, n)| format!("{name} x{n}"))
    }
}

fn tally(dir: &Path, cutoff: i64) -> Tally {
    let mut t = Tally::default();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path()) else { continue };
        let text = String::from_utf8_lossy(&raw);
        // The script's `read` loop never sees a last line that has no newline.
        let rec: HashMap<&str, &str> = text.split_inclusive('\n').filter_map(|l| l.strip_suffix('\n')).map(|l| l.split_once('=').unwrap_or((l, ""))).collect();
        let field = |k: &str| rec.get(k).copied().unwrap_or("");
        let at = field("at");
        if !at.bytes().all(|b| b.is_ascii_digit()) || at.parse::<i64>().map_or(true, |at| at < cutoff) {
            continue;
        }
        t.reds += 1;
        match field("verdict") {
            "DEFECT" => {
                t.defect += 1;
                t.inferred += u64::from(field("by").starts_with("auto:"));
            }
            "GATE_FAULT" => {
                t.fault += 1;
                let name = match field("suite") {
                    "" | "-" => field("reason"),
                    suite => suite,
                };
                if !name.is_empty() {
                    *t.faults.entry(name.to_string()).or_default() += 1;
                }
            }
            _ => t.unknown += 1,
        }
    }
    t
}

struct Run<'a> {
    repo: &'a str,
    start: i64,
    end: i64,
    red: bool,
}

/// One `gate.log` row; none for a torn or older-format line, and none for a cached verdict,
/// which ran nothing and so cost nothing.
fn meter_row(line: &str) -> Option<Run<'_>> {
    fn word(s: &str) -> Option<(&str, &str)> {
        let (w, rest) = s.split_once(' ').unwrap_or((s, ""));
        (!w.is_empty() && !w.contains(char::is_whitespace)).then(|| (w, rest.trim_start_matches(' ')))
    }
    let secs = |w: &str, key: &str| {
        let n = w.strip_prefix(key)?.strip_suffix('s')?;
        n.bytes().all(|b| b.is_ascii_digit()).then(|| n.parse::<i64>().ok())?
    };
    let (stamp, rest) = word(line)?;
    let (repo, rest) = word(rest)?;
    let (_branch, rest) = word(rest)?;
    let (waited, rest) = word(rest)?;
    let (ran, rest) = word(rest)?;
    let (rc, note) = word(rest)?;
    if stamp.len() != 20 || !stamp.ends_with('Z') || note.split(' ').next() == Some("cached") {
        return None;
    }
    let (end, cost) = (parse_rfc3339(stamp)?, secs(waited, "waited=")? + secs(ran, "ran=")?);
    Some(Run { repo, start: end - cost, end, red: rc.strip_prefix("rc=")? != "0" })
}

struct Cost {
    solo: Vec<i64>,
    contended: Vec<i64>,
    log_reds: u64,
}

/// Two runs of one repository were concurrent exactly when their `[finish - waited - ran,
/// finish]` intervals overlap.
fn cost(log: &str, now: i64, window: i64) -> Cost {
    let runs: Vec<Run> = log.lines().filter_map(meter_row).collect();
    let mut by_repo: HashMap<&str, Vec<(usize, &Run)>> = HashMap::new();
    for (i, r) in runs.iter().enumerate().filter(|(_, r)| r.end >= now - window - 86400) {
        by_repo.entry(r.repo).or_default().push((i, r));
    }
    let mut c = Cost { solo: Vec::new(), contended: Vec::new(), log_reds: 0 };
    for (i, r) in runs.iter().enumerate().filter(|(_, r)| r.end >= now - window) {
        c.log_reds += u64::from(r.red);
        let overlapped = by_repo.get(r.repo).into_iter().flatten().any(|(j, o)| *j != i && o.start < r.end && r.start < o.end);
        if overlapped { &mut c.contended } else { &mut c.solo }.push(r.end - r.start);
    }
    c
}

/// Count, median and worst; no rows is not a measurement of zero seconds.
fn stat(costs: &[i64]) -> [String; 3] {
    let mut v = costs.to_vec();
    v.sort_unstable();
    let show = |x: Option<&i64>| x.map_or_else(|| "?".to_string(), i64::to_string);
    [v.len().to_string(), show(v.get(v.len() / 2)), show(v.last())]
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: u64 = 7200;
    const NOW: u64 = 1_700_000_000;

    struct Rig {
        root: testkit::TempDir,
    }

    impl Rig {
        fn new() -> Rig {
            Rig { root: testkit::TempDir::new("yield-report") }
        }
        fn dir(&self) -> PathBuf {
            self.root.join("gate-yield")
        }
        fn log(&self) -> PathBuf {
            self.root.join("gate.log")
        }
        fn record(&self, name: &str, ago: u64, verdict: &str, by: &str, suite: &str, reason: &str) {
            std::fs::create_dir_all(self.dir()).unwrap();
            let at = NOW - ago;
            let body = format!("at={at}\nlast={at}\nseen=1\nrepo=repo\nbranch=spira/{name}\nbead={name}\ntree=t1\noutcome=FAIL\nreason={reason}\nsuite={suite}\nverdict={verdict}\nby={by}\nwhy=\n");
            std::fs::write(self.dir().join(name), body).unwrap();
        }
        /// Rows are (finished seconds ago, repo, waited, ran, rc, note).
        fn meter(&self, rows: &[(u64, &str, u64, u64, &str, &str)]) {
            let text: String = rows
                .iter()
                .map(|(ago, repo, waited, ran, rc, note)| format!("{} {repo} spira/sp-m waited={waited}s ran={ran}s rc={rc} {note}\n", crate::timefmt::utc_stamp((NOW - ago) as i64)))
                .collect();
            std::fs::write(self.log(), text).unwrap();
        }
        fn read(&self, window: u64) -> BTreeMap<String, String> {
            report(&self.dir(), &self.log(), window, NOW).into_iter().collect()
        }
    }

    fn pick<'a>(r: &'a BTreeMap<String, String>, keys: &[&str]) -> Vec<&'a str> {
        keys.iter().map(|k| r.get(*k).map_or("<absent>", String::as_str)).collect()
    }

    const COUNTS: [&str; 6] = ["YIELD_REDS", "YIELD_DEFECT", "YIELD_FAULT", "YIELD_UNKNOWN", "YIELD_DEFECT_INFERRED", "YIELD_TOP_FAULT"];

    #[test_id(tvp5a1y)]
    #[test]
    fn the_keys_come_in_the_scripts_order() {
        let got: Vec<String> = report(&Rig::new().dir(), Path::new("does/not/exist"), WINDOW, NOW).into_iter().map(|(k, _)| k).collect();
        let want = "WINDOW REDS DEFECT FAULT UNKNOWN DEFECT_INFERRED TOP_FAULT RECORDER SOLO_N SOLO_MED SOLO_MAX CONC_N CONC_MED CONC_MAX LOG_REDS";
        assert_eq!(got, want.split(' ').map(|k| format!("YIELD_{k}")).collect::<Vec<_>>());
    }

    #[test_id(tz5ec6f)]
    #[test]
    fn a_red_nobody_classified_counts_as_unknown_and_not_as_either_verdict() {
        let rig = Rig::new();
        rig.record("sp-a", 60, "UNKNOWN", "", "test-a.sh", "branch-red");
        std::fs::write(rig.dir().join("sp-b"), format!("at={}\nreason=branch-red\n", NOW - 60)).unwrap();
        assert_eq!(pick(&rig.read(WINDOW), &COUNTS), ["2", "0", "0", "2", "0", "-"]);
    }

    #[test_id(td5enkm)]
    #[test]
    fn an_inferred_defect_is_counted_apart_from_a_stated_one() {
        let rig = Rig::new();
        rig.record("sp-a", 60, "DEFECT", "auto:changed-then-passed", "-", "branch-red");
        rig.record("sp-b", 60, "DEFECT", "stated", "-", "branch-red");
        assert_eq!(pick(&rig.read(WINDOW), &COUNTS), ["2", "2", "0", "0", "1", "-"]);
    }

    #[test_id(t8suy3l)]
    #[test]
    fn the_worst_gate_fault_is_named_by_its_suite_and_by_its_reason_when_it_has_none() {
        let rig = Rig::new();
        rig.record("sp-a", 60, "GATE_FAULT", "auto:BASE_FAIL", "test-boxreader.sh", "base-red");
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_FAULT", "YIELD_TOP_FAULT"]), ["1", "test-boxreader.sh x1"]);
        rig.record("sp-b", 60, "GATE_FAULT", "auto:NO_VERDICT", "-", "deadline");
        rig.record("sp-c", 60, "GATE_FAULT", "auto:NO_VERDICT", "", "deadline");
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_FAULT", "YIELD_TOP_FAULT"]), ["3", "deadline x2"]);
    }

    #[test_id(tmlzb73)]
    #[test]
    fn an_absent_record_and_an_unreadable_log_are_unknown_never_zero() {
        let r = Rig::new().read(WINDOW);
        assert_eq!(pick(&r, &COUNTS), ["?"; 6]);
        assert_eq!(pick(&r, &["YIELD_RECORDER", "YIELD_SOLO_N", "YIELD_SOLO_MED", "YIELD_CONC_MAX", "YIELD_LOG_REDS"]), ["?"; 5]);
        assert_eq!(r["YIELD_WINDOW"], WINDOW.to_string());
    }

    #[test_id(twyl40q)]
    #[test]
    fn a_record_that_does_not_exist_is_absent_while_the_meter_has_no_reds_and_silent_once_it_has() {
        let rig = Rig::new();
        rig.meter(&[(200, "repo", 0, 90, "0", "pass")]);
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_RECORDER", "YIELD_REDS", "YIELD_LOG_REDS"]), ["absent", "?", "0"]);
        rig.meter(&[(200, "repo", 0, 90, "1", "branch-red")]);
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_RECORDER", "YIELD_REDS", "YIELD_LOG_REDS"]), ["silent", "?", "1"]);
    }

    #[test_id(t5xuhjd)]
    #[test]
    fn an_empty_record_under_a_meter_showing_reds_is_silent_and_withholds_its_counts() {
        let rig = Rig::new();
        std::fs::create_dir(rig.dir()).unwrap();
        rig.meter(&[(200, "repo", 0, 90, "1", "branch-red")]);
        let r = rig.read(WINDOW);
        assert_eq!(r["YIELD_RECORDER"], "silent");
        assert_eq!(pick(&r, &COUNTS), ["?"; 6]);
        rig.record("sp-a", 60, "UNKNOWN", "", "-", "branch-red");
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_RECORDER", "YIELD_REDS"]), ["ok", "1"], "a record holding reds is not withheld by the same log");
    }

    #[test_id(tj09mox)]
    #[test]
    fn counts_with_no_readable_meter_are_given_but_the_recorder_is_unknown() {
        let rig = Rig::new();
        rig.record("sp-a", 60, "UNKNOWN", "", "-", "branch-red");
        assert_eq!(pick(&rig.read(WINDOW), &["YIELD_RECORDER", "YIELD_REDS"]), ["?", "1"]);
    }

    #[test_id(tezbufq)]
    #[test]
    fn a_red_older_than_the_window_is_not_counted_until_the_window_reaches_it() {
        let rig = Rig::new();
        rig.record("sp-new", 60, "UNKNOWN", "", "-", "branch-red");
        rig.record("sp-edge", WINDOW, "UNKNOWN", "", "-", "branch-red");
        rig.record("sp-ancient", WINDOW * 3, "UNKNOWN", "", "-", "branch-red");
        std::fs::write(rig.dir().join("sp-torn"), "at=soon\n").unwrap();
        std::fs::write(rig.dir().join(".hidden"), format!("at={NOW}\n")).unwrap();
        assert_eq!(rig.read(WINDOW)["YIELD_REDS"], "2");
        assert_eq!(rig.read(WINDOW * 4)["YIELD_REDS"], "3");
    }

    #[test_id(t2qkvgl)]
    #[test]
    fn cost_is_split_by_overlap_within_one_repository_and_a_cached_pass_is_no_cost() {
        let rig = Rig::new();
        rig.meter(&[
            (300, "repo", 0, 100, "0", "pass"),
            (310, "repo", 10, 30, "1", "branch-red"),
            (305, "other", 0, 70, "0", "pass"),
            (40, "repo", 0, 60, "0", ""),
            (30, "repo", 0, 0, "1", "cached tree=abc"),
            (WINDOW + 500, "repo", 0, 999, "1", "branch-red"),
        ]);
        let mut text = std::fs::read_to_string(rig.log()).unwrap();
        text.push_str("2023-11-14T22:13:00Z repo spira/sp-torn waited=3s\nnot a row\n");
        std::fs::write(rig.log(), text).unwrap();
        let r = rig.read(WINDOW);
        assert_eq!(pick(&r, &["YIELD_SOLO_N", "YIELD_SOLO_MED", "YIELD_SOLO_MAX"]), ["2", "70", "70"]);
        assert_eq!(pick(&r, &["YIELD_CONC_N", "YIELD_CONC_MED", "YIELD_CONC_MAX"]), ["2", "100", "100"]);
        assert_eq!(r["YIELD_LOG_REDS"], "1");
    }

    #[test_id(t368qey)]
    #[test]
    fn a_log_with_no_rows_in_the_window_is_no_samples_and_not_zero_seconds() {
        let rig = Rig::new();
        rig.meter(&[]);
        let r = rig.read(WINDOW);
        assert_eq!(pick(&r, &["YIELD_SOLO_N", "YIELD_SOLO_MED", "YIELD_SOLO_MAX", "YIELD_CONC_N", "YIELD_CONC_MED", "YIELD_CONC_MAX", "YIELD_LOG_REDS"]), ["0", "?", "?", "0", "?", "?", "0"]);
    }

    #[test_id(t8zurfi)]
    #[test]
    fn the_sources_default_under_the_run_directory_and_the_environment_overrides_them() {
        let d = Sources::resolve(Path::new("r"), |_| Some(String::new()));
        assert_eq!((d.dir, d.gate_log, d.window_s), (PathBuf::from("r/gate-yield"), PathBuf::from("r/gate.log"), 86400));
        let o = Sources::resolve(Path::new("r"), |k| Some(match k {
            "SPIRA_YIELD" => "y",
            "SPIRA_GATE_LOG" => "l",
            "SPIRA_YIELD_WINDOW" => "60",
            _ => return None,
        }.to_string()));
        assert_eq!((o.dir, o.gate_log, o.window_s), (PathBuf::from("y"), PathBuf::from("l"), 60));
    }
}
