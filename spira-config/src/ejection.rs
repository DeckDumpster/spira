//! What an ejected bead is told: the failing tests, where each failed, the tail of its output.
//! The eject paths record it as an `eject-failure` fact beside the transition; the next dwarf's
//! brief prints it first. A test that ejects several beads on unrelated diffs is a red on the
//! base or in the gate, and the digest says so.

use std::process::Stdio;

pub const KIND: &str = "eject-failure";
const TIP_PREFIX: &str = "tip ";
const FILES_PREFIX: &str = "files ";
const FAIL_PREFIX: &str = "FAIL ";
const ALSO_PREFIX: &str = "ALSO ";
const MAX_FAILURES: usize = 8;
const TAIL_LINES: usize = 6;
const FALLBACK_LINES: usize = 20;
const LINE_MAX: usize = 200;
const FILES_MAX: usize = 40;
const DIGEST_MAX: usize = 5500;
/// How far back a digest still says something about the base: older ones are another era's red.
pub const LOOKBACK_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub test: String,
    pub location: Option<String>,
    pub tail: Vec<String>,
}

fn clip(l: &str) -> String {
    l.chars().filter(|c| !c.is_control()).take(LINE_MAX).collect()
}

fn location_of(line: &str) -> Option<String> {
    let rest = line.split_once(" panicked at ")?.1.trim().trim_end_matches(':');
    let mut parts = rest.rsplitn(3, ':');
    let (_col, line_no, file) = (parts.next()?, parts.next()?, parts.next()?);
    (line_no.bytes().all(|b| b.is_ascii_digit()) && !file.is_empty()).then(|| format!("{file}:{line_no}"))
}

/// The failing tests of a `cargo test` output: each `---- name stdout ----` block, its panic
/// location, and the last lines of its output. A test listed as FAILED with no block keeps its name.
pub fn failures(output: &str) -> Vec<Failure> {
    let mut out: Vec<Failure> = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    let close = |cur: &mut Option<(String, Vec<&str>)>, out: &mut Vec<Failure>| {
        if let Some((test, body)) = cur.take() {
            let location = body.iter().find_map(|l| location_of(l));
            let kept: Vec<&str> = body.iter().copied().filter(|l| !l.trim().is_empty() && !l.starts_with("note: run with `RUST_BACKTRACE")).collect();
            let tail = kept[kept.len().saturating_sub(TAIL_LINES)..].iter().map(|l| clip(l)).collect();
            out.push(Failure { test, location, tail });
        }
    };
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("---- ").and_then(|r| r.strip_suffix(" stdout ----")) {
            close(&mut cur, &mut out);
            cur = Some((name.to_string(), Vec::new()));
        } else if line == "failures:" || line.starts_with("test result:") {
            close(&mut cur, &mut out);
        } else if let Some((_, body)) = cur.as_mut() {
            body.push(line);
        }
    }
    close(&mut cur, &mut out);
    for name in output.lines().filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED")) {
        if !out.iter().any(|f| f.test == name) {
            out.push(Failure { test: name.to_string(), location: None, tail: Vec::new() });
        }
    }
    out
}

/// The digest: the tip judged, the files its diff changes, then each failing test with its
/// location and output tail. An output that names no failing test keeps its own last lines.
pub fn digest(tip: &str, files: &[String], output: &str) -> String {
    let mut d = format!("{TIP_PREFIX}{tip}\n");
    if !files.is_empty() {
        let shown: Vec<&str> = files.iter().take(FILES_MAX).map(String::as_str).collect();
        d += &format!("{FILES_PREFIX}{}\n", shown.join(" "));
    }
    let fs = failures(output);
    if fs.is_empty() {
        d += "no failing test name in the output; its last lines:\n";
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        for l in &lines[lines.len().saturating_sub(FALLBACK_LINES)..] {
            d += &format!("    {}\n", clip(l));
        }
    }
    for f in fs.iter().take(MAX_FAILURES) {
        d += &format!("{FAIL_PREFIX}{}{}\n", f.test, f.location.as_ref().map(|l| format!(" at {l}")).unwrap_or_default());
        for l in &f.tail {
            d += &format!("    {l}\n");
        }
    }
    if fs.len() > MAX_FAILURES {
        d += &format!("... and {} more failing tests\n", fs.len() - MAX_FAILURES);
    }
    d.chars().take(DIGEST_MAX).collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub tip: String,
    pub files: Vec<String>,
    pub tests: Vec<String>,
}

pub fn parse(digest: &str) -> Parsed {
    let mut p = Parsed::default();
    for l in digest.lines() {
        if let Some(t) = l.strip_prefix(TIP_PREFIX) {
            p.tip = t.trim().to_string();
        } else if let Some(f) = l.strip_prefix(FILES_PREFIX) {
            p.files = f.split_whitespace().map(String::from).collect();
        } else if let Some(t) = l.strip_prefix(FAIL_PREFIX) {
            p.tests.push(t.split(" at ").next().unwrap_or(t).trim().to_string());
        }
    }
    p
}

/// The other beads a digest's failing tests already ejected, on a different tip and a diff that
/// shares no file with this one, with the tests they share.
pub fn shared_reds(bead: &str, this: &str, prior: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let me = parse(this);
    if me.files.is_empty() {
        return Vec::new();
    }
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for (other, text) in prior {
        let o = parse(text);
        let shared: Vec<String> = me.tests.iter().filter(|t| o.tests.contains(t)).cloned().collect();
        let disjoint = !o.files.is_empty() && !o.files.iter().any(|f| me.files.contains(f));
        if other == bead || o.tip == me.tip || shared.is_empty() || !disjoint {
            continue;
        }
        match seen.iter_mut().find(|(b, _)| b == other) {
            Some((_, ts)) => {
                let new: Vec<String> = shared.into_iter().filter(|t| !ts.contains(t)).collect();
                ts.extend(new);
            }
            None => seen.push((other.clone(), shared)),
        }
    }
    seen
}

/// The digest with the shared-red verdict appended, when there is one.
pub fn with_shared(this: &str, shared: &[(String, Vec<String>)]) -> String {
    if shared.is_empty() {
        return this.to_string();
    }
    let mut d = this.trim_end_matches('\n').to_string();
    for (other, tests) in shared {
        d += &format!("\n{ALSO_PREFIX}{other} was ejected on {} with a different diff", tests.join(", "));
    }
    d += "\nthe same test failing on unrelated diffs is a red on the base or in the gate, not in this branch\n";
    d
}

/// Records `output`'s failure as the bead's `eject-failure` fact. `lc` runs `spira-lc <args>`
/// and answers its stdout.
pub fn record(lc: &dyn Fn(&[String]) -> Result<String, String>, bead: &str, tip: &str, files: &[String], output: &str, actor: &str) -> Result<(), String> {
    let d = digest(tip, files, output);
    let since = (now() - LOOKBACK_SECS).to_string();
    let prior = lc(&["facts".into(), "--kinds".into(), KIND.into(), "--since".into(), since]).ok().map(|o| prior_digests(&o)).unwrap_or_default();
    let d = with_shared(&d, &shared_reds(bead, &d, &prior));
    lc(&["fact".into(), bead.into(), "--kind".into(), KIND.into(), "--actor".into(), actor.into(), "--cause".into(), d]).map(|_| ())
}

fn prior_digests(facts_json: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(facts_json.trim()) else { return Vec::new() };
    let s = |r: &serde_json::Value, k: &str| r.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    rows.iter().map(|r| (s(r, "issue_id"), s(r, "new_value"))).collect()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `lc` for [`record`]: the installed `spira-lc`, bounded.
pub fn cli(args: &[String]) -> Result<String, String> {
    let o = crate::bounded::bounded("spira-lc").args(args).stdin(Stdio::null()).output().map_err(|e| format!("spira-lc: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(format!("spira-lc exited {:?}: {}", o.status.code(), String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("")))
    }
}

/// The bead's newest recorded failure out of `spira-lc facts --kinds eject-failure` output.
pub fn latest(facts_json: &str) -> String {
    prior_digests(facts_json).pop().map(|(_, d)| d).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const OUT: &str = "running 2 tests\ntest ci_cut::ok ... ok\ntest ci_cut::finds_config ... FAILED\n\nfailures:\n\n---- ci_cut::finds_config stdout ----\nthread 'ci_cut::finds_config' panicked at spira/sim/tests/ci_cut.rs:70:9:\nno built spira-config beside <target>/dwarf or on PATH\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    ci_cut::finds_config\n\ntest result: FAILED. 1 passed; 1 failed\n";

    #[test]
    fn a_failing_test_is_named_with_its_panic_location_and_output_tail() {
        let f = failures(OUT);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].test, "ci_cut::finds_config");
        assert_eq!(f[0].location.as_deref(), Some("spira/sim/tests/ci_cut.rs:70"));
        assert!(f[0].tail.last().unwrap().contains("no built spira-config"), "{:?}", f[0].tail);
        assert!(f[0].tail.iter().all(|l| !l.contains("RUST_BACKTRACE")));
    }

    #[test]
    fn a_failed_test_with_no_block_keeps_its_name_and_unnamed_output_keeps_its_tail() {
        assert_eq!(failures("test a::b ... FAILED\n")[0].test, "a::b");
        let d = digest("t1", &[], "one\ntwo\nlint broke\n");
        assert!(d.contains("no failing test name") && d.contains("    lint broke"), "{d}");
    }

    #[test]
    fn a_digest_round_trips_through_parse() {
        let d = digest("abc", &["a.rs".into(), "b.rs".into()], OUT);
        let p = parse(&d);
        assert_eq!((p.tip.as_str(), p.files.len(), p.tests.as_slice()), ("abc", 2, &["ci_cut::finds_config".to_string()][..]), "{d}");
    }

    #[test]
    fn the_same_test_on_a_disjoint_diff_names_the_first_bead_and_blames_the_base() {
        let first = digest("t1", &["x.rs".into()], OUT);
        let second = digest("t2", &["y.rs".into()], OUT);
        let shared = shared_reds("sp-b", &second, &[("sp-a".into(), first)]);
        assert_eq!(shared, vec![("sp-a".to_string(), vec!["ci_cut::finds_config".to_string()])]);
        let d = with_shared(&second, &shared);
        assert!(d.contains("sp-a") && d.contains("base or in the gate"), "{d}");
    }

    #[test]
    fn overlapping_diffs_the_same_bead_or_the_same_tip_are_not_a_shared_red() {
        let mine = digest("t2", &["x.rs".into()], OUT);
        let overlap = digest("t1", &["x.rs".into(), "z.rs".into()], OUT);
        let same_tip = digest("t2", &["q.rs".into()], OUT);
        let other_test = digest("t1", &["q.rs".into()], "test other ... FAILED\n");
        for (id, theirs) in [("sp-a", overlap), ("sp-a", same_tip), ("sp-b", digest("t1", &["q.rs".into()], OUT)), ("sp-a", other_test)] {
            assert!(shared_reds("sp-b", &mine, &[(id.into(), theirs)]).is_empty());
        }
    }

    #[test]
    fn record_writes_the_fact_after_reading_prior_reds() {
        let first = digest("t1", &["x.rs".into()], OUT);
        let facts = serde_json::json!([{"issue_id": "sp-a", "event_type": KIND, "new_value": first}]).to_string();
        let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let lc = |a: &[String]| {
            calls.borrow_mut().push(a.to_vec());
            Ok(if a[0] == "facts" { facts.clone() } else { String::new() })
        };
        record(&lc, "sp-b", "t2", &["y.rs".into()], OUT, "gate").unwrap();
        let calls = calls.borrow();
        let fact = calls.iter().find(|c| c[0] == "fact").unwrap();
        let cause = &fact[fact.iter().position(|a| a == "--cause").unwrap() + 1];
        assert!(cause.contains("FAIL ci_cut::finds_config at spira/sim/tests/ci_cut.rs:70") && cause.contains("ALSO sp-a"), "{cause}");
        assert_eq!(latest(&serde_json::json!([{"issue_id": "sp-b", "event_type": KIND, "new_value": cause}]).to_string()), *cause);
    }
}
