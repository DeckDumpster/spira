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
const STEP_PREFIX: &str = "STEP ";
const WHY_PREFIX: &str = "why ";
const STEP_LINE_MAX: usize = 500;
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
    clip_to(l, LINE_MAX)
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

fn clip_to(l: &str, max: usize) -> String {
    l.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The red steps of a `run_all`-composed output: (unit, the step's own text) for each
/// `gate-step RED` marker. An output with no marker is one step, `gate`.
pub fn red_steps(output: &str) -> Vec<(String, String)> {
    let mut steps = Vec::new();
    let mut seen_marker = false;
    let mut buf: Vec<&str> = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.trim().strip_prefix("gate-step ") {
            seen_marker = true;
            if rest.starts_with("RED") {
                let unit = rest.split_once(" :: ").map(|(_, u)| u.trim()).unwrap_or("gate");
                steps.push((unit.to_string(), buf.join("\n")));
            }
            buf.clear();
        } else if !line.starts_with("gate: ") {
            buf.push(line);
        }
    }
    if !seen_marker {
        return vec![("gate".to_string(), output.to_string())];
    }
    steps
}

fn is_positive_control(l: &str) -> bool {
    (l.starts_with("fence:") && l.contains(" checked ")) || l.starts_with("spira-lint: clean")
}

fn is_finding_start(rest: &str) -> bool {
    let mut it = rest.split(' ');
    let (Some(rule), Some(loc)) = (it.next(), it.next()) else { return false };
    let Some(rule) = rule.strip_suffix(':') else { return false };
    let Some(loc) = loc.strip_suffix(':') else { return false };
    let ident = !rule.is_empty() && rule.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let (path, no) = loc.rsplit_once(':').unwrap_or(("", ""));
    ident && !path.is_empty() && !no.is_empty() && no.bytes().all(|b| b.is_ascii_digit())
}

/// A round's `member=<culprit> <finding>` line as spira-lint's own `<finding>`.
fn strip_member(l: &str) -> &str {
    let Some(who) = l.strip_prefix("member=") else { return l };
    if who.starts_with("the base itself (") {
        return who.split_once(") ").map(|(_, f)| f).unwrap_or(who);
    }
    if who.starts_with("commit ") {
        let starts = who.char_indices().filter(|(_, c)| *c == ' ').map(|(i, _)| i + 1);
        return starts.map(|i| &who[i..]).find(|r| is_finding_start(r)).unwrap_or(who);
    }
    who.split_once(' ').map(|(_, f)| f).unwrap_or(who)
}

/// What one red step's text contributes: failing tests, else compiler errors with their
/// locations, else the text's own finding lines.
pub fn step_failure(text: &str) -> Vec<String> {
    let fs = failures(text);
    let mut out = Vec::new();
    if !fs.is_empty() {
        for f in fs.iter().take(MAX_FAILURES) {
            out.push(format!("{FAIL_PREFIX}{}{}", f.test, f.location.as_ref().map(|l| format!(" at {l}")).unwrap_or_default()));
            out.extend(f.tail.iter().map(|l| format!("    {l}")));
        }
        if fs.len() > MAX_FAILURES {
            out.push(format!("... and {} more failing tests", fs.len() - MAX_FAILURES));
        }
        return out;
    }
    let lines: Vec<&str> = text.lines().collect();
    let loc_after = |i: usize| lines[i + 1..].iter().take(3).find(|n| n.trim_start().starts_with("--> "));
    for (i, l) in lines.iter().enumerate() {
        let loc = loc_after(i);
        if l.starts_with("error[") || (l.starts_with("error:") && loc.is_some()) {
            out.push(clip_to(l, STEP_LINE_MAX));
            out.extend(loc.map(|n| clip_to(n, STEP_LINE_MAX)));
        }
    }
    if !out.is_empty() {
        return out;
    }
    for l in lines {
        let l = strip_member(l);
        if l.trim().is_empty() || is_positive_control(l) {
            continue;
        }
        let l = clip_to(l, STEP_LINE_MAX);
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// The digest: the tip judged, the files its diff changes, the attribution lines, then for each
/// red step its failure lines. Over the size limit the first lines are kept and the cut counted.
pub fn digest_steps(tip: &str, files: &[String], why: &[String], steps: &[(String, String)]) -> String {
    let mut lines = vec![format!("{TIP_PREFIX}{tip}")];
    if !files.is_empty() {
        let shown: Vec<&str> = files.iter().take(FILES_MAX).map(String::as_str).collect();
        lines.push(format!("{FILES_PREFIX}{}", shown.join(" ")));
    }
    lines.extend(why.iter().map(|w| format!("{WHY_PREFIX}{}", clip_to(w, STEP_LINE_MAX))));
    for (unit, text) in steps {
        lines.push(format!("{STEP_PREFIX}{unit}"));
        for l in step_failure(text) {
            lines.push(if l.starts_with(FAIL_PREFIX) { l } else { format!("  {l}") });
        }
    }
    let total = lines.len();
    let mut d = String::new();
    let mut kept = 0;
    for l in &lines {
        let more = total - kept - 1;
        let marker = format!("... {} more lines cut\n", more);
        if d.chars().count() + l.chars().count() + 1 + if more > 0 { marker.len() } else { 0 } > DIGEST_MAX {
            break;
        }
        d += l;
        d.push('\n');
        kept += 1;
    }
    if kept < total {
        d += &format!("... {} more lines cut\n", total - kept);
    }
    d
}

pub fn digest(tip: &str, files: &[String], output: &str) -> String {
    digest_steps(tip, files, &[], &red_steps(output))
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

/// Records the red `steps`' failure as the bead's `eject-failure` fact. `lc` runs `spira-lc <args>`
/// and answers its stdout.
pub fn record(lc: &dyn Fn(&[String]) -> Result<String, String>, bead: &str, tip: &str, files: &[String], why: &[String], steps: &[(String, String)], actor: &str) -> Result<(), String> {
    let d = digest_steps(tip, files, why, steps);
    let since = (now() - LOOKBACK_SECS).to_string();
    let prior = lc(&["facts".into(), "--kinds".into(), KIND.into(), "--since".into(), since]).ok().map(|o| prior_digests(&o)).unwrap_or_default();
    let d = with_shared(&d, &shared_reds(bead, &d, &prior));
    lc(&["fact".into(), bead.into(), "--kind".into(), KIND.into(), "--actor".into(), actor.into(), "--cause".into(), d]).map(|_| ())
}

pub const LOGS_KIND: &str = "send-back-logs";

/// Keeps one entry of `files` and records it as a `send-back-logs` fact on each bead in `beads`.
/// Answers the entry id; no bead is told of an entry that was not written.
pub fn keep_and_reference(lc: &dyn Fn(&[String]) -> Result<String, String>, run: &std::path::Path, beads: &[&str], actor: &str, files: &[(String, Vec<u8>)], keep_days: u32) -> Result<String, String> {
    let id = crate::sendback::keep(run, actor, files, keep_days, now())?;
    reference(lc, beads, actor, &id)?;
    Ok(id)
}

/// Keeps the reason sift sends `bead` back with as `sift.txt` and references it from the bead's facts.
pub fn keep_sift(lc: &dyn Fn(&[String]) -> Result<String, String>, run: &std::path::Path, bead: &str, reason: &str) -> Result<String, String> {
    let days = crate::process::cfg_parse::<u32>("SPIRA_SENDBACK_KEEP_DAYS")?;
    keep_and_reference(lc, run, &[bead], "sift", &[("sift.txt".to_string(), reason.as_bytes().to_vec())], days)
}

/// Writes the `send-back-logs` fact naming `id` on each bead.
pub fn reference(lc: &dyn Fn(&[String]) -> Result<String, String>, beads: &[&str], actor: &str, id: &str) -> Result<(), String> {
    for b in beads {
        lc(&["fact".into(), (*b).into(), "--kind".into(), LOGS_KIND.into(), "--actor".into(), actor.into(), "--cause".into(), id.into()])?;
    }
    Ok(())
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
    fn a_failed_test_with_no_block_keeps_its_name_and_unnamed_output_keeps_its_own_lines() {
        assert_eq!(failures("test a::b ... FAILED\n")[0].test, "a::b");
        let d = digest("t1", &[], "one\ntwo\nlint broke\n");
        assert!(d.contains("STEP gate") && d.contains("  lint broke") && d.contains("  one"), "{d}");
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
        record(&lc, "sp-b", "t2", &["y.rs".into()], &[], &red_steps(OUT), "gate").unwrap();
        let calls = calls.borrow();
        let fact = calls.iter().find(|c| c[0] == "fact").unwrap();
        let cause = &fact[fact.iter().position(|a| a == "--cause").unwrap() + 1];
        assert!(cause.contains("FAIL ci_cut::finds_config at spira/sim/tests/ci_cut.rs:70") && cause.contains("ALSO sp-a"), "{cause}");
    }

    const HINT: &str = "config-key-registered: A SPIRA_ name production code reads as config belongs in spira/conf.d and is read through spira_config::process::cfg (the registry lists each key; a key read in code but absent from conf.d fails here, and so does a lookup of an allow-listed name that no file reads any more, which is the case when a script stops reading a key it once did)";
    const FINDING: &str = "config-key-registered: spira-lint/config-key-registered-allow:892: lists SPIRA_TOML for spira/lib-test-install.sh, which no longer reads it";

    fn lint_gate_out() -> String {
        let mut o = String::from("gate-step ok rc=0 :: lifecycle-guard\ngate-step ok rc=0 :: fmt\n");
        o += &format!("{FINDING}\n{HINT}\ngate-step RED rc=1 :: spira-lint\n");
        for i in 0..16 {
            o += &format!("gate-step ok rc=0 :: unit{i}\n");
        }
        o += "build-fence: cargo check ok\ngate: the same command passes against local/main, so this is the branch's own.\n";
        o
    }

    #[test]
    fn a_lint_red_records_its_finding_and_no_passing_step() {
        assert!(HINT.len() > 200);
        let d = digest("d6940c05d", &[], &lint_gate_out());
        assert!(d.contains("config-key-registered: spira-lint/config-key-registered-allow:892:") && d.contains("STEP spira-lint"), "{d}");
        assert!(!d.contains("gate-step ok") && !d.contains("cargo check ok"), "{d}");
        assert!(d.contains(HINT), "hint kept whole under the line bound: {d}");
    }

    #[test]
    fn a_round_red_and_a_gate_red_on_the_same_lint_output_record_the_same_lines() {
        let round = format!("member=the base itself (no member's diff touches spira-lint/config-key-registered-allow or its rule config-key-registered) {FINDING}\n{HINT}\n");
        let f = vec!["a.rs".to_string()];
        let r = digest_steps("t", &f, &[], &[("spira-lint".to_string(), round)]);
        let g = digest("t", &f, &lint_gate_out());
        let body = |d: &str| d.lines().skip(2).map(String::from).collect::<Vec<_>>();
        assert_eq!(body(&r), body(&g), "{r}\n---\n{g}");
    }

    #[test]
    fn a_commit_culprit_prefix_is_stripped_to_the_finding() {
        let round = format!("member=commit abc123def456 fix: tidy the thing {FINDING}\n");
        let d = digest_steps("t", &[], &[], &[("spira-lint".to_string(), round)]);
        assert!(d.contains(&format!("  {FINDING}")), "{d}");
    }

    #[test]
    fn findings_over_the_size_limit_keep_the_first_and_count_the_rest() {
        let text: String = (0..300).map(|i| format!("some-rule: src/f{i}.rs:{i}: finding number {i} with padding to take room\n")).collect();
        let d = digest("t", &[], &format!("{text}gate-step RED rc=1 :: spira-lint\n"));
        assert!(d.contains("src/f0.rs:0:"), "{d}");
        assert!(d.chars().count() <= DIGEST_MAX);
        let last = d.lines().last().unwrap();
        let kept = d.lines().filter(|l| l.contains("some-rule:")).count();
        assert_eq!(last, format!("... {} more lines cut", 300 - kept), "{last}");
    }

    #[test]
    fn a_build_red_records_the_compiler_errors_with_their_locations() {
        let mut o = String::new();
        o += "error[E0425]: cannot find value `x` in this scope\n --> gate/src/x.rs:3:5\n  |\n3 |     x\n";
        for i in 0..30 {
            o += &format!("warning: unused variable `v{i}`\n --> gate/src/y.rs:{i}:1\n");
        }
        o += "error: could not compile `gate` (lib) due to 1 previous error\ngate-step RED rc=101 :: build\n";
        let d = digest("t", &[], &o);
        assert!(d.contains("error[E0425]: cannot find value") && d.contains("--> gate/src/x.rs:3:5"), "{d}");
        assert!(!d.contains("warning:") && !d.contains("could not compile"), "{d}");
    }

    #[test]
    fn a_lint_text_with_an_error_line_keeps_every_finding() {
        let d = digest("t", &[], &format!("error: something the rule quotes\n{FINDING}\ngate-step RED rc=1 :: spira-lint\n"));
        assert!(d.contains("error: something the rule quotes") && d.contains(FINDING), "{d}");
    }

    #[test]
    fn an_indented_marker_still_splits_the_steps_and_attribution_lines_are_recorded_as_why() {
        let steps = red_steps("one\n  gate-step ok rc=0 :: a\ntwo\ngate-step RED rc=1 :: b\n");
        assert_eq!(steps, vec![("b".to_string(), "two".to_string())]);
        let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let lc = |a: &[String]| {
            calls.borrow_mut().push(a.to_vec());
            Ok(String::new())
        };
        record(&lc, "sp-b", "t", &[], &["spira-lint: red".into()], &steps, "batcher").unwrap();
        let calls = calls.borrow();
        let fact = calls.iter().find(|c| c[0] == "fact").unwrap();
        let cause = &fact[fact.iter().position(|a| a == "--cause").unwrap() + 1];
        assert!(cause.contains("why spira-lint: red\nSTEP b\n  two"), "{cause}");
    }

    #[test]
    fn one_kept_entry_is_referenced_by_a_fact_on_every_bead_it_judged() {
        let run = testkit::TempDir::new("kar");
        let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let lc = |a: &[String]| {
            calls.borrow_mut().push(a.to_vec());
            Ok(String::new())
        };
        let id = keep_and_reference(&lc, run.path(), &["sp-a", "sp-b"], "round", &[("pass/x.out".replace('/', "_"), b"o".to_vec())], 7).unwrap();
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        for (c, b) in calls.iter().zip(["sp-a", "sp-b"]) {
            assert_eq!(c[..6], ["fact", b, "--kind", LOGS_KIND, "--actor", "round"].map(String::from));
            assert_eq!(c[7], id);
        }
        assert!(crate::sendback::read(run.path(), &id, "pass_x.out").is_ok());
    }
}
