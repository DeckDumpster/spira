//! What an ejected bead is told: the failing tests, where each failed, the tail of its output.
//! The eject paths record it as an `eject-failure` fact beside the transition; the next dwarf's
//! brief prints it first. A test that ejects several beads on unrelated diffs is a red on the
//! base or in the gate, and the digest says so.

use std::process::Stdio;

pub const KIND: &str = "eject-failure";
const TIP_PREFIX: &str = "tip ";
const FILES_PREFIX: &str = "files ";
const FILES_CUT_PREFIX: &str = "files-cut ";
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
/// How many other beads a test must have failed on, on unrelated diffs, before it is a flip.
pub const FLIP_MIN_BEADS: usize = 2;
const MORE_PREFIX: &str = "... and ";
const MORE_SUFFIX: &str = " more failing tests";
const CUT_SUFFIX: &str = " more lines cut";

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
    nextest_failures(output, &mut out);
    for name in output.lines().filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED")) {
        if !out.iter().any(|f| f.test == name) {
            out.push(Failure { test: name.to_string(), location: None, tail: Vec::new() });
        }
    }
    out
}

fn is_rule_line(l: &str) -> bool {
    let l = l.trim();
    l.chars().count() >= 3 && l.chars().all(|c| c == '─' || c == '-')
}

/// Each red nextest status line opens a block ended by the next status line, a rule line or
/// `Summary [`; a name already listed, as at the end-of-run repeat, adds nothing.
fn nextest_failures(output: &str, out: &mut Vec<Failure>) {
    let mut cur: Option<(&str, Vec<&str>)> = None;
    let mut close = |cur: &mut Option<(&str, Vec<&str>)>| {
        let Some((test, body)) = cur.take() else { return };
        if out.iter().any(|f| f.test == test) {
            return;
        }
        let kept: Vec<&str> = body.iter().map(|l| l.trim_start()).filter(|l| {
            !l.is_empty() && !l.starts_with("running ") && !l.starts_with("note: run with `RUST_BACKTRACE") && !l.ends_with(" ───")
        }).collect();
        let from = kept.iter().position(|l| l.contains(" panicked at ")).unwrap_or(kept.len().saturating_sub(TAIL_LINES));
        let tail = &kept[from..(from + TAIL_LINES).min(kept.len())];
        let location = body.iter().find_map(|l| location_of(l));
        out.push(Failure { test: test.to_string(), location, tail: tail.iter().map(|l| clip(l)).collect() });
    };
    for line in output.lines() {
        if crate::unit_tier::nextest_status(line) || is_rule_line(line) || line.trim_start().starts_with("Summary [") {
            close(&mut cur);
            cur = crate::unit_tier::nextest_red(line.trim()).map(|n| (n, Vec::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            body.push(line);
        }
    }
    close(&mut cur);
}

fn clip_to(l: &str, max: usize) -> String {
    l.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The red steps of a `run_all`-composed output: (unit, the step's own text) for each
/// `gate-step RED` marker. An output with no marker is one step, `gate`; markers with no RED
/// leave the text after the last one as the step, named for the phase that failed.
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
    if steps.is_empty() {
        let unit = output.lines().find_map(|l| l.trim().strip_prefix("gate: phase '")?.split_once("' failed").map(|(n, _)| format!("phase:{n}")));
        steps.push((unit.unwrap_or_else(|| "gate".to_string()), buf.join("\n")));
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
pub fn step_failure(unit: &str, text: &str) -> Vec<String> {
    let fs = failures(text);
    let mut out = Vec::new();
    if !fs.is_empty() {
        for f in fs.iter().take(MAX_FAILURES) {
            out.push(format!("{FAIL_PREFIX}{}{}", f.test, f.location.as_ref().map(|l| format!(" at {l}")).unwrap_or_default()));
            out.extend(f.tail.iter().map(|l| format!("    {l}")));
        }
        if fs.len() > MAX_FAILURES {
            out.push(format!("{MORE_PREFIX}{}{MORE_SUFFIX}", fs.len() - MAX_FAILURES));
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
    let lines = without_warnings(&lines);
    if unit == "gate" || unit.starts_with("phase:") {
        let body: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).collect();
        out.push(format!("no recognisable failure in {} lines; the last follow:", body.len()));
        let mut budget = DIGEST_MAX * 3 / 4;
        let mut tail: Vec<String> = Vec::new();
        for l in body.iter().rev() {
            let l = clip_to(l, STEP_LINE_MAX);
            if l.len() + 3 > budget {
                break;
            }
            budget -= l.len() + 3;
            tail.push(l);
        }
        out.extend(tail.into_iter().rev());
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

/// `lines` without each cargo warning block: a `warning` line and the indented lines after it.
fn without_warnings<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut kept = Vec::new();
    let mut in_warning = false;
    for l in lines {
        if l.starts_with("warning") {
            in_warning = true;
        } else if !(in_warning && l.starts_with(char::is_whitespace)) {
            in_warning = false;
            kept.push(*l);
        }
    }
    kept
}

/// The digest: the tip judged, the files its diff changes, the attribution lines, then for each
/// red step its failure lines. Over the size limit the first lines are kept and the cut counted.
pub fn digest_steps(tip: &str, files: &[String], why: &[String], steps: &[(String, String)]) -> String {
    let mut lines = vec![format!("{TIP_PREFIX}{tip}")];
    lines.extend(why.iter().map(|w| format!("{WHY_PREFIX}{}", clip_to(w, STEP_LINE_MAX))));
    for (unit, text) in steps {
        lines.push(format!("{STEP_PREFIX}{unit}"));
        for l in step_failure(unit, text) {
            lines.push(if l.starts_with(FAIL_PREFIX) { l } else { format!("  {l}") });
        }
    }
    let total = lines.len();
    let mut d = String::new();
    let mut kept = 0;
    for l in &lines {
        let more = total - kept - 1;
        let marker = format!("... {more}{CUT_SUFFIX}\n");
        if d.chars().count() + l.chars().count() + 1 + if more > 0 { marker.len() } else { 0 } > DIGEST_MAX {
            break;
        }
        d += l;
        d.push('\n');
        kept += 1;
    }
    if kept < total {
        d += &format!("... {}{CUT_SUFFIX}\n", total - kept);
    }
    let room = DIGEST_MAX.saturating_sub(d.chars().count());
    let at = d.find('\n').map_or(d.len(), |i| i + 1);
    d.insert_str(at, &files_lines(files, room));
    d
}

/// The `files` line with as many leading paths as fit in `room` characters, then a `files-cut` line
/// counting the rest.
fn files_lines(files: &[String], room: usize) -> String {
    let fits = |n: usize| {
        let shown = format!("{FILES_PREFIX}{}\n", files[..n].join(" "));
        let cut = if n < files.len() { format!("{FILES_CUT_PREFIX}{} more\n", files.len() - n) } else { String::new() };
        let s = if n == 0 { cut } else { shown + &cut };
        (s.chars().count() <= room).then_some(s)
    };
    let n = files.len().min(FILES_MAX);
    (0..=n).rev().find_map(fits).unwrap_or_default()
}

pub fn digest(tip: &str, files: &[String], output: &str) -> String {
    digest_steps(tip, files, &[], &red_steps(output))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub tip: String,
    pub files: Vec<String>,
    pub files_cut: bool,
    pub tests: Vec<String>,
}

pub fn parse(digest: &str) -> Parsed {
    let mut p = Parsed::default();
    for l in digest.lines() {
        if let Some(t) = l.strip_prefix(TIP_PREFIX) {
            p.tip = t.trim().to_string();
        } else if l.starts_with(FILES_CUT_PREFIX) {
            p.files_cut = true;
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
    if me.files.is_empty() || me.files_cut {
        return Vec::new();
    }
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for (other, text) in prior {
        let o = parse(text);
        let shared: Vec<String> = me.tests.iter().filter(|t| o.tests.contains(t)).cloned().collect();
        let disjoint = !o.files.is_empty() && !o.files.iter().any(|f| me.files.contains(f));
        if other == bead || o.files_cut || o.tip == me.tip || shared.is_empty() || !disjoint {
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

/// The failing tests of `this` when every one of them also failed on at least [`FLIP_MIN_BEADS`]
/// other beads with unrelated diffs, each with those beads. `None` for a digest with no named test or a
/// truncated list, and for any test the evidence does not cover.
pub fn flipped(bead: &str, this: &str, prior: &[(String, String)]) -> Option<Vec<(String, Vec<String>)>> {
    let tests = parse(this).tests;
    if tests.is_empty() || this.lines().map(str::trim_start).any(|l| l.starts_with(MORE_PREFIX) && l.ends_with(MORE_SUFFIX) || l.starts_with("... ") && l.ends_with(CUT_SUFFIX)) {
        return None;
    }
    let shared = shared_reds(bead, this, prior);
    tests
        .into_iter()
        .map(|t| {
            let beads: Vec<String> = shared.iter().filter(|(_, ts)| ts.contains(&t)).map(|(b, _)| b.clone()).collect();
            (beads.len() >= FLIP_MIN_BEADS).then_some((t, beads))
        })
        .collect()
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
    let prior = prior(lc, now() - LOOKBACK_SECS).unwrap_or_default();
    let d = with_shared(&d, &shared_reds(bead, &d, &prior));
    lc(&["fact".into(), "add".into(), bead.into(), "--kind".into(), KIND.into(), "--actor".into(), actor.into(), "--cause".into(), d]).map(|_| ())
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
        lc(&["fact".into(), "add".into(), (*b).into(), "--kind".into(), LOGS_KIND.into(), "--actor".into(), actor.into(), "--cause".into(), id.into()])?;
    }
    Ok(())
}

/// The `eject-failure` digests recorded since `since`, as (bead, digest).
pub fn prior(lc: &dyn Fn(&[String]) -> Result<String, String>, since: i64) -> Result<Vec<(String, String)>, String> {
    lc(&["fact".into(), "list".into(), "--kinds".into(), KIND.into(), "--since".into(), since.to_string()]).map(|o| prior_digests(&o))
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

    #[test_id(t59they)]
    #[test]
    fn a_failing_test_is_named_with_its_panic_location_and_output_tail() {
        let f = failures(OUT);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].test, "ci_cut::finds_config");
        assert_eq!(f[0].location.as_deref(), Some("spira/sim/tests/ci_cut.rs:70"));
        assert!(f[0].tail.last().unwrap().contains("no built spira-config"), "{:?}", f[0].tail);
        assert!(f[0].tail.iter().all(|l| !l.contains("RUST_BACKTRACE")));
    }

    #[test_id(tbb10aq)]
    #[test]
    fn a_failed_test_with_no_block_keeps_its_name_and_unnamed_output_keeps_its_own_lines() {
        assert_eq!(failures("test a::b ... FAILED\n")[0].test, "a::b");
        let d = digest("t1", &[], "one\ntwo\nlint broke\n");
        assert!(d.contains("STEP gate") && d.contains("  lint broke") && d.contains("  one"), "{d}");
    }

    fn nextest_fail(name: &str, body: &str) -> String {
        format!("        FAIL [   0.300s] (3533/6077) bin/x {name}\n  stderr ───\n\n{body}\n")
    }

    const SUMMARY: &str = "------------\n     Summary [  81.070s] 6077 tests run: 6075 passed, 2 failed\n";

    #[test_id(t9vnxdg)]
    #[test]
    fn a_nextest_fail_after_cargo_warnings_is_named_first_with_its_panic_and_no_warning() {
        let warnings: String = (0..30).map(|i| format!("warning: profile package spec `p{i}` did not match any packages\n")).collect();
        let out = format!("{warnings}{}        PASS [   0.1s] (1/2) bin/x a::ok\n{SUMMARY}        FAIL [   0.300s] (3533/6077) bin/x t::boom\nerror: test run failed\n", nextest_fail("t::boom", "    thread 't::boom' panicked at src/x.rs:12:9:\n    left != right\n    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"));
        let d = digest("t1", &[], &out);
        let l: Vec<&str> = d.lines().collect();
        let at = l.iter().position(|l| *l == "STEP phase:test" || *l == "STEP gate").unwrap();
        assert_eq!(l[at + 1], "FAIL t::boom at src/x.rs:12", "{d}");
        assert!(l[at + 2].contains("panicked at") || l[at + 2].contains("left != right"), "{d}");
        assert!(!d.contains("warning:"), "{d}");
        assert_eq!(d.matches("FAIL t::boom").count(), 1, "{d}");
    }

    #[test_id(txntiku)]
    #[test]
    fn the_phase_test_step_of_a_nextest_red_names_the_failure_under_it() {
        let out = format!("{}{SUMMARY}gate-step RED :: phase:test\n", nextest_fail("t::boom", "    thread 't::boom' panicked at src/x.rs:12:9:\n    bad"));
        let d = digest("t1", &[], &out);
        assert!(d.contains("STEP phase:test\nFAIL t::boom at src/x.rs:12\n      thread 't::boom' panicked at src/x.rs:12:9:\n      bad\n"), "{d}");
    }

    #[test_id(tblo5ar)]
    #[test]
    fn a_nextest_fail_without_a_panic_keeps_its_stderr() {
        let out = nextest_fail("t::sandbox", "    bwrap: No permissions to create a new namespace\n    (test failed with exit code 1)");
        let f = failures(&out);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].location, None);
        assert_eq!(f[0].tail, ["bwrap: No permissions to create a new namespace", "(test failed with exit code 1)"]);
    }

    #[test_id(t83kuig)]
    #[test]
    fn a_timeout_and_a_libtest_red_in_one_output_are_each_named_once() {
        let out = format!("{}{}{SUMMARY}        TIMEOUT [  60.0s] (9/9) bin/x t::slow\n        FAIL [   0.3s] (3/9) bin/x t::boom\n{OUT}", nextest_fail("t::boom", "    thread 't::boom' panicked at a.rs:1:1:\n    x"), "        TIMEOUT [  60.0s] (9/9) bin/x t::slow\n  stdout ───\n    slow\n");
        let names: Vec<String> = failures(&out).into_iter().map(|f| f.test).collect();
        assert_eq!(names, ["ci_cut::finds_config", "t::boom", "t::slow"], "{names:?}");
    }

    #[test_id(tpulfnf)]
    #[test]
    fn unrecognised_gate_output_says_so_and_keeps_the_last_lines_within_the_digest_limit() {
        let out: String = (0..300).map(|i| format!("noise line {i} {}\n", "x".repeat(60))).collect();
        let d = digest("t1", &["a.rs".into()], &out);
        assert!(d.contains("no recognisable failure in 300 lines; the last follow:"), "{d}");
        assert!(d.contains("noise line 299 "), "{d}");
        assert!(!d.contains("noise line 0 "), "{d}");
        assert!(d.chars().count() <= DIGEST_MAX, "{}", d.chars().count());
    }

    #[test_id(trk3owz)]
    #[test]
    fn other_units_keep_the_head_of_unrecognised_output() {
        let d = digest_steps("t", &[], &[], &[("spira-lint".to_string(), "first\nwarning: w\n  indented\nlast\n".to_string())]);
        assert!(d.contains("  first") && d.contains("  last") && !d.contains("indented") && !d.contains("no recognisable"), "{d}");
    }

    #[test_id(t2rnhms)]
    #[test]
    fn a_digest_round_trips_through_parse() {
        let d = digest("abc", &["a.rs".into(), "b.rs".into()], OUT);
        let p = parse(&d);
        assert_eq!((p.tip.as_str(), p.files.len(), p.tests.as_slice()), ("abc", 2, &["ci_cut::finds_config".to_string()][..]), "{d}");
    }

    #[test_id(tp6u4ek)]
    #[test]
    fn the_same_test_on_a_disjoint_diff_names_the_first_bead_and_blames_the_base() {
        let first = digest("t1", &["x.rs".into()], OUT);
        let second = digest("t2", &["y.rs".into()], OUT);
        let shared = shared_reds("sp-b", &second, &[("sp-a".into(), first)]);
        assert_eq!(shared, vec![("sp-a".to_string(), vec!["ci_cut::finds_config".to_string()])]);
        let d = with_shared(&second, &shared);
        assert!(d.contains("sp-a") && d.contains("base or in the gate"), "{d}");
    }

    #[test_id(tjur4pj)]
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

    fn seen(bead: &str, tip: &str, file: &str) -> (String, String) {
        (bead.into(), digest(tip, &[file.into()], OUT))
    }

    #[test_id(tgpo9rl)]
    #[test]
    fn a_test_shared_by_two_beads_on_disjoint_diffs_is_a_flip() {
        let mine = digest("t3", &["m.rs".into()], OUT);
        let f = flipped("sp-z", &mine, &[seen("sp-x", "t1", "a.rs"), seen("sp-y", "t2", "b.rs")]).unwrap();
        assert_eq!(f, vec![("ci_cut::finds_config".to_string(), vec!["sp-x".to_string(), "sp-y".to_string()])]);
    }

    #[test_id(t1sxors)]
    #[test]
    fn one_bead_overlapping_diffs_or_a_partly_shared_digest_is_not_a_flip() {
        let mine = digest("t3", &["m.rs".into()], OUT);
        assert!(flipped("sp-z", &mine, &[seen("sp-x", "t1", "a.rs")]).is_none());
        assert!(flipped("sp-z", &mine, &[seen("sp-x", "t1", "a.rs"), seen("sp-y", "t2", "m.rs")]).is_none());
        assert!(flipped("sp-z", &mine, &[seen("sp-x", "t1", "a.rs"), seen("sp-x", "t2", "b.rs")]).is_none());
        let two = format!("{OUT}test other::t ... FAILED\n");
        let mine2 = digest("t3", &["m.rs".into()], &two);
        assert!(flipped("sp-z", &mine2, &[seen("sp-x", "t1", "a.rs"), seen("sp-y", "t2", "b.rs")]).is_none());
        assert!(flipped("sp-z", &digest("t3", &["m.rs".into()], "lint broke\n"), &[]).is_none());
    }

    #[test_id(tx8jx0g)]
    #[test]
    fn a_truncated_digest_is_not_a_flip() {
        let many: String = (0..9).map(|i| format!("test t::n{i} ... FAILED\n")).collect();
        let mine = digest("t3", &["m.rs".into()], &many);
        let prior: Vec<(String, String)> = ["sp-x", "sp-y"].iter().map(|b| (b.to_string(), digest(&format!("t{b}"), &[format!("{b}.rs")], &many))).collect();
        assert!(flipped("sp-z", &mine, &prior).is_none());
    }

    #[test_id(tbxhb2l)]
    #[test]
    fn a_digest_cut_for_size_is_not_a_flip() {
        let lint: String = (0..300).map(|i| format!("some-rule: src/f{i}.rs:{i}: finding number {i} with padding to take room\n")).collect();
        let make = |tip: &str, file: &str| digest_steps(tip, &[file.to_string()], &[], &[("phase:test".to_string(), "test t::a ... FAILED\n".to_string()), ("spira-lint".to_string(), lint.clone())]);
        let mine = make("t3", "m.rs");
        assert!(mine.lines().any(|l| l.ends_with(CUT_SUFFIX)), "{mine}");
        let prior: Vec<(String, String)> = ["sp-x", "sp-y"].iter().map(|b| (b.to_string(), make(&format!("t{b}"), &format!("{b}.rs")))).collect();
        assert!(flipped("sp-z", &mine, &prior).is_none());
    }

    #[test_id(tmgshut)]
    #[test]
    fn a_failed_prior_read_is_an_error_and_record_still_writes_the_fact() {
        let calls: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let lc = |a: &[String]| {
            calls.borrow_mut().push(a[..2].join(" "));
            if a[..2] == ["fact", "list"] { Err("down".to_string()) } else { Ok(String::new()) }
        };
        assert!(prior(&lc, 0).is_err());
        record(&lc, "sp-b", "t2", &["y.rs".into()], &[], &red_steps(OUT), "gate").unwrap();
        assert!(calls.borrow().iter().any(|c| c == "fact add"));
    }

    #[test_id(tpzhtjo)]
    #[test]
    fn record_writes_the_fact_after_reading_prior_reds() {
        let first = digest("t1", &["x.rs".into()], OUT);
        let facts = serde_json::json!([{"issue_id": "sp-a", "event_type": KIND, "new_value": first}]).to_string();
        let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let lc = |a: &[String]| {
            calls.borrow_mut().push(a.to_vec());
            Ok(if a[..2] == ["fact", "list"] { facts.clone() } else { String::new() })
        };
        record(&lc, "sp-b", "t2", &["y.rs".into()], &[], &red_steps(OUT), "gate").unwrap();
        let calls = calls.borrow();
        let fact = calls.iter().find(|c| c[..2] == ["fact", "add"]).unwrap();
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

    fn phase_red_out() -> String {
        let mut o: String = ["spira-lint", "lifecycle-guard", "fixture-config", "round-image-preflight", "ci-spill-guard", "boundary"].iter().map(|u| format!("gate-step ok rc=0 :: {u}\n")).collect();
        o += "running 66 tests\n---- t::x stdout ----\nthread 't::x' panicked at state-store-sqlite/tests/conformance_sqlite.rs:626:5:\nassertion failed\n\nfailures:\n    t::x\n\ntest result: FAILED. 65 passed; 1 failed\ngate: phase 'test' failed (exit 101): cargo test\n";
        o
    }

    #[test_id(teaidit)]
    #[test]
    fn a_phase_red_after_passing_markers_records_its_failing_lines() {
        let files: Vec<String> = (0..40).map(|i| format!("crate/src/file{i}.rs")).collect();
        let d = digest("abc", &files, &phase_red_out());
        assert!(d.contains("STEP phase:test"), "{d}");
        assert!(d.contains("FAIL t::x at state-store-sqlite/tests/conformance_sqlite.rs:626") && d.contains("assertion failed"), "{d}");
    }

    #[test_id(tthurql)]
    #[test]
    fn an_oversized_file_list_is_shortened_and_the_failure_kept() {
        let files: Vec<String> = (0..40).map(|i| format!("{i:03}{}", "p".repeat(147))).collect();
        let steps = vec![("test".to_string(), "---- t::x stdout ----\nthread 't::x' panicked at a/b.rs:1:2:\nboom\n\nfailures:\n".to_string())];
        let why = vec!["why kept".to_string()];
        let d = digest_steps("abc", &files, &why, &steps);
        assert!(d.lines().position(|l| l == "why why kept") > d.lines().position(|l| l.starts_with("files-cut ")), "{d}");
        assert!(d.contains("FAIL t::x at a/b.rs:1") && d.contains("boom"), "{d}");
        assert!(d.lines().any(|l| l.starts_with("files-cut ") && l.ends_with(" more")), "{d}");
        assert!(d.chars().count() <= DIGEST_MAX, "{}", d.chars().count());
        assert!(!d.contains(CUT_SUFFIX), "{d}");
    }

    #[test_id(tcke1eh)]
    #[test]
    fn a_cut_file_list_is_never_a_shared_red() {
        let cut = format!("tip t1\nfiles x.rs\n{FILES_CUT_PREFIX}3 more\nFAIL t::x\n");
        let whole = "tip t2\nfiles y.rs\nFAIL t::x\n".to_string();
        assert!(parse(&cut).files_cut);
        assert!(shared_reds("sp-b", &whole, &[("sp-a".into(), cut.clone())]).is_empty());
        assert!(shared_reds("sp-b", &cut, &[("sp-a".into(), whole.clone())]).is_empty());
        assert_eq!(shared_reds("sp-b", &whole, &[("sp-a".into(), "tip t1\nfiles x.rs\nFAIL t::x\n".into())]).len(), 1);
    }

    #[test_id(tvnucfa)]
    #[test]
    fn a_lint_red_records_its_finding_and_no_passing_step() {
        assert!(HINT.len() > 200);
        let d = digest("d6940c05d", &[], &lint_gate_out());
        assert!(d.contains("config-key-registered: spira-lint/config-key-registered-allow:892:") && d.contains("STEP spira-lint"), "{d}");
        assert!(!d.contains("gate-step ok") && !d.contains("cargo check ok"), "{d}");
        assert!(d.contains(HINT), "hint kept whole under the line bound: {d}");
    }

    #[test_id(tqojax0)]
    #[test]
    fn a_round_red_and_a_gate_red_on_the_same_lint_output_record_the_same_lines() {
        let round = format!("member=the base itself (no member's diff touches spira-lint/config-key-registered-allow or its rule config-key-registered) {FINDING}\n{HINT}\n");
        let f = vec!["a.rs".to_string()];
        let r = digest_steps("t", &f, &[], &[("spira-lint".to_string(), round)]);
        let g = digest("t", &f, &lint_gate_out());
        let body = |d: &str| d.lines().skip(2).map(String::from).collect::<Vec<_>>();
        assert_eq!(body(&r), body(&g), "{r}\n---\n{g}");
    }

    #[test_id(tu19q7q)]
    #[test]
    fn a_commit_culprit_prefix_is_stripped_to_the_finding() {
        let round = format!("member=commit abc123def456 fix: tidy the thing {FINDING}\n");
        let d = digest_steps("t", &[], &[], &[("spira-lint".to_string(), round)]);
        assert!(d.contains(&format!("  {FINDING}")), "{d}");
    }

    #[test_id(tca7x9x)]
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

    #[test_id(t8yuoyp)]
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

    #[test_id(tdxja4d)]
    #[test]
    fn a_lint_text_with_an_error_line_keeps_every_finding() {
        let d = digest("t", &[], &format!("error: something the rule quotes\n{FINDING}\ngate-step RED rc=1 :: spira-lint\n"));
        assert!(d.contains("error: something the rule quotes") && d.contains(FINDING), "{d}");
    }

    #[test_id(tpel3j6)]
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
        let fact = calls.iter().find(|c| c[..2] == ["fact", "add"]).unwrap();
        let cause = &fact[fact.iter().position(|a| a == "--cause").unwrap() + 1];
        assert!(cause.contains("why spira-lint: red\nSTEP b\n  two"), "{cause}");
    }

    #[test_id(te7zkj3)]
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
            assert_eq!(c[..7], ["fact", "add", b, "--kind", LOGS_KIND, "--actor", "round"].map(String::from));
            assert_eq!(c[8], id);
        }
        assert!(crate::sendback::read(run.path(), &id, "pass_x.out").is_ok());
    }
}
