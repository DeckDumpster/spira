//! The mender's verbs. Every outside fact comes through [`World`], so each verb is a pure
//! function of what the world answers and a test hands in a recorder.

use crate::outcome::{self, Answer, Kind};
use crate::pack::{Member, Pack, Spec};
use crate::signature;

pub const ACTOR: &str = "mender";
const DIFF_CAP: usize = 24_000;
const DESIGN_LINES: usize = 200;

pub trait World {
    /// `spira-lc <args>`: the exit status and the answer.
    fn lc(&self, args: &[&str]) -> (i32, String);
    fn git(&self, repo: &str, args: &[&str]) -> Result<String, String>;
    fn read(&self, path: &str) -> Result<String, String>;
    /// `SPIRA_MENDER_DEADLINE_SECS`, as declared; there is no fallback.
    fn deadline_secs(&self) -> Result<u64, String>;
    /// `SPIRA_WIKI`: the root a bead's `wiki/...` design path is read under.
    fn wiki(&self) -> Result<String, String>;
}

type Reply = Result<String, String>;

fn lc_ok(w: &dyn World, args: &[&str]) -> Reply {
    match w.lc(args) {
        (0, out) => Ok(out),
        (code, out) => Err(format!("spira-lc {} refused ({code}): {out}", args.first().copied().unwrap_or(""))),
    }
}

pub fn load_spec(w: &dyn World, path: &str) -> Result<Spec, String> {
    let text = w.read(path)?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not a failure spec: {e}"))
}

fn member(w: &dyn World, spec: &Spec, bead: &str, tip: &str) -> Result<Member, String> {
    let range = format!("{}...{tip}", spec.base);
    let files = w.git(&spec.repo, &["diff", "--name-only", &range])?;
    let diff = w.git(&spec.repo, &["diff", &range])?;
    Ok(Member { bead: bead.into(), tip: tip.into(), files: files.lines().map(str::to_string).collect(), diff })
}

fn members(w: &dyn World, spec: &Spec) -> Result<Vec<Member>, String> {
    spec.members.iter().map(|m| member(w, spec, &m.bead, &m.tip)).collect()
}

fn suite_covers(w: &dyn World, spec: &Spec) -> Option<Vec<String>> {
    let text = w.read(&format!("{}/spira/{}", spec.repo, spec.suite)).ok()?;
    suite_select::header::covers_of(&text)
}

pub fn design_paths(description: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in description.split_whitespace() {
        let tok = tok.trim_matches(|c: char| matches!(c, '`' | '(' | ')' | ',' | ';' | '"' | '\'' | '.'));
        if let Some(i) = tok.find("wiki/") {
            let rest = &tok[i + "wiki/".len()..];
            if rest.ends_with(".md") && !out.iter().any(|p| p == rest) {
                out.push(rest.to_string());
            }
        }
    }
    out
}

pub fn pack(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let output = spec.output.clone();
    let ms = members(w, &spec)?;
    let covers = suite_covers(w, &spec);
    let hits = signature::scan(&output);
    let mut text = Pack { spec: &spec, output: &output, hits: &hits, covers: covers.as_deref(), members: &ms, diff_cap: DIFF_CAP }.render();
    text.push_str("\n## Whole bead of each member the evidence points at\n");
    let pointed = crate::pack::touching(&ms, covers.as_deref());
    for m in pointed {
        text.push_str(&format!("\n{}\n", bead(w, &m.bead)?));
    }
    Ok(text)
}

pub fn diff(w: &dyn World, spec_path: &str, bead_id: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let m = spec.members.iter().find(|m| m.bead == bead_id).ok_or_else(|| format!("{bead_id} is not a member of {}", spec.batch))?;
    Ok(member(w, &spec, &m.bead, &m.tip)?.diff)
}

/// Description, acceptance and notes (`content show`), comments, lifecycle history, and the
/// design the description names.
pub fn bead(w: &dyn World, id: &str) -> Reply {
    let shown = lc_ok(w, &["content", "show", id])?;
    let mut out = format!("## Bead {id}\n\n{shown}\n\n### Comments\n\n{}\n\n### History\n\n{}\n", lc_ok(w, &["content", "comments", id])?, lc_ok(w, &["history", id])?);
    for p in design_paths(&shown) {
        out.push_str(&format!("\n### Source design: {p}\n\n"));
        let full = w.wiki().map(|root| format!("{}/{p}", root.trim_end_matches('/')));
        match full.and_then(|f| w.read(&f)) {
            Ok(t) => out.push_str(&t.lines().take(DESIGN_LINES).collect::<Vec<_>>().join("\n")),
            Err(e) => out.push_str(&format!("(unreadable: {e})")),
        }
        out.push('\n');
    }
    Ok(out)
}

pub fn round_members(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let mut out = format!("round {} (pass {}), base {}\n", spec.batch, spec.pass, spec.base);
    for m in &spec.members {
        let state = lc_ok(w, &["show", &m.bead]).unwrap_or_else(|e| e);
        out.push_str(&format!("\n{} @ {}\n{state}\n", m.bead, m.tip));
    }
    Ok(out)
}

pub fn failures(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    lc_ok(w, &["mending", "list", "--batch", &spec.batch])
}

pub fn passes(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    lc_ok(w, &["history", &spec.batch, "--machine", "batch"])
}

pub fn verdicts(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let ids: Vec<&str> = spec.members.iter().map(|m| m.bead.as_str()).collect();
    lc_ok(w, &["facts", "--ids", &ids.join(",")])
}

pub fn landing(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    w.git(&spec.repo, &["log", "--format=%h %an %s", "-n", "20", &spec.base])
}

fn target(spec: &Spec) -> [String; 5] {
    [spec.batch.clone(), "--pass".into(), spec.pass.to_string(), "--suite".into(), spec.suite.clone()]
}

fn event(w: &dyn World, spec: &Spec, kind: serde_json::Value) -> Reply {
    let t = target(spec);
    let kind = kind.to_string();
    let mut args = vec!["mending", "event"];
    args.extend(t.iter().map(String::as_str));
    args.extend(["--kind", &kind, "--actor", ACTOR]);
    lc_ok(w, &args)
}

/// Takes the failure: the machine stamps the pickup and fixes the deadline from the declared
/// config, never from anything the mender passes.
pub fn start(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let secs = w.deadline_secs()?.to_string();
    let t = target(&spec);
    let mut args = vec!["mending", "pickup"];
    args.extend(t.iter().map(String::as_str));
    args.extend(["--deadline-secs", &secs, "--actor", ACTOR]);
    lc_ok(w, &args)
}

/// Stamps the end of triage; the machine refuses a second stamp, which is not an error here.
pub fn triaged(w: &dyn World, spec_path: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    event(w, &spec, serde_json::json!({"TriageEnded": {"at": 0}}))
}

/// Writes the diagnosis so far, so a deadline return carries it.
pub fn diagnose(w: &dyn World, spec_path: &str, text: &str) -> Reply {
    let spec = load_spec(w, spec_path)?;
    if text.trim().is_empty() {
        return Err("diagnose: the text goes on stdin".into());
    }
    event(w, &spec, serde_json::json!({"Diagnosis": {"text": text.trim()}}))
}

fn target_row(w: &dyn World, spec: &Spec) -> Option<serde_json::Value> {
    let (code, out) = w.lc(&["mending", "list", "--batch", &spec.batch]);
    let rows: Vec<serde_json::Value> = if code == 0 { serde_json::from_str(&out).unwrap_or_default() } else { vec![] };
    rows.into_iter().find(|r| r["pass"] == spec.pass && r["suite"] == spec.suite.as_str())
}

fn settled_state(row: &serde_json::Value) -> Option<&str> {
    row["state"].as_str().filter(|s| !matches!(*s, "WAITING" | "MENDING"))
}

pub fn finish(w: &dyn World, spec_path: &str, kind: &str, diagnosis: &str, checks: Option<&str>) -> Reply {
    let spec = load_spec(w, spec_path)?;
    let kind = Kind::parse(kind).ok_or_else(|| format!("{kind:?} is not an outcome: mended | environment | flaky | return | unsure"))?;
    let text = outcome::validate(&Answer { kind, diagnosis, checks })?;
    let row = target_row(w, &spec);
    if let Some(state) = row.as_ref().and_then(settled_state) {
        return Ok(format!("no-op: {} pass {} {} is already {state}; no event sent", spec.batch, spec.pass, spec.suite));
    }
    if !row.is_some_and(|r| !r["triage_ended_at"].is_null()) {
        event(w, &spec, serde_json::json!({"TriageEnded": {"at": 0}}))?;
    }
    event(w, &spec, serde_json::json!({"Diagnosis": {"text": text}}))?;
    match kind {
        Kind::Mended => mend(w, &spec, checks.unwrap_or("")),
        _ => event(w, &spec, outcome::terminal_event(kind, &text)),
    }
}

const FENCE_LIST: &str = "spira/mend-fences";
const SUITE_GLOB: &str = "**/test-*.sh";

fn numstat(w: &dyn World, repo: &str, from: &str, to: &str) -> Result<Vec<(String, u32, u32)>, String> {
    let out = w.git(repo, &["diff", "--numstat", "--no-renames", from, to])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (add, del, path) = (f.next()?, f.next()?, f.next()?);
            Some((path.to_string(), add.parse().unwrap_or(0), del.parse().unwrap_or(0)))
        })
        .collect())
}

fn crate_dir(w: &dyn World, repo: &str, rev: &str, file: &str) -> Option<String> {
    let mut dir = file.rsplit_once('/')?.0;
    loop {
        if w.git(repo, &["cat-file", "-e", &format!("{rev}:{dir}/Cargo.toml")]).is_ok() {
            return Some(dir.to_string());
        }
        dir = dir.rsplit_once('/')?.0;
    }
}

fn suites_naming(w: &dyn World, repo: &str, rev: &str, file: &str) -> Vec<String> {
    let name = file.rsplit('/').next().unwrap_or(file);
    let glob = format!(":(glob){SUITE_GLOB}");
    let prefix = format!("{rev}:");
    w.git(repo, &["grep", "-l", "-F", "-e", name, rev, "--", &glob])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix(&prefix).map(String::from))
        .collect()
}

/// Takes each member's branch head as its mended tip and hands the machine the facts to
/// assess; the machine, not the mender, decides whether the mend is taken.
fn mend(w: &dyn World, spec: &Spec, checks: &str) -> Reply {
    let beads: Vec<&str> = spec.members.iter().map(|m| m.bead.as_str()).collect();
    let recorded: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&lc_ok(w, &["mending", "tips", "--beads", &beads.join(",")])?).map_err(|e| format!("spira-lc mending tips: {e}"))?;
    let mut moves = Vec::new();
    let mut changes: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
    let mut required: Vec<String> = Vec::new();
    for m in &spec.members {
        let current = recorded.get(&m.bead).and_then(|t| t.as_str()).unwrap_or(&m.tip).to_string();
        let branch = format!("refs/heads/spira/{}", m.bead);
        let tip = w.git(&spec.repo, &["rev-parse", "--verify", &format!("{branch}^{{commit}}")])?.trim().to_string();
        if tip == current {
            continue;
        }
        if w.git(&spec.repo, &["merge-base", "--is-ancestor", &current, &tip]).is_err() {
            return Err(format!("mended: {branch} at {tip} is not built on {current}, the tip this round last saw; rebuild the fix on it"));
        }
        let stat = numstat(w, &spec.repo, &current, &tip)?;
        let paths: Vec<String> = stat.iter().map(|(p, _, _)| p.clone()).collect();
        for (p, a, d) in stat {
            let e = changes.entry(p).or_default();
            e.0 += a;
            e.1 += d;
        }
        required.extend(lifecycle::mending::required_checks(&paths, |p| crate_dir(w, &spec.repo, &tip, p), |p| suites_naming(w, &spec.repo, &tip, p)));
        moves.push(format!("{}:{current}:{tip}", m.bead));
    }
    if moves.is_empty() {
        return Err("mended: no member branch has moved since the round last saw it; push the fix to the member's branch first".into());
    }
    required.sort();
    required.dedup();
    let mut member_files = Vec::new();
    for m in &spec.members {
        member_files.extend(numstat(w, &spec.repo, &spec.base, &m.tip)?.into_iter().map(|(p, _, _)| p));
    }
    let fences: Vec<String> = w
        .git(&spec.repo, &["show", &format!("{}:{FENCE_LIST}", spec.base)])
        .map_err(|e| format!("{FENCE_LIST} is not at the round's base, so a mend cannot be assessed: {e}"))?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let facts = lifecycle::mending::MendFacts {
        changes: changes.into_iter().map(|(path, (added, deleted))| lifecycle::mending::FileChange { path, added, deleted }).collect(),
        member_files,
        required_checks: required,
        checked: checks.split(|c: char| c == ',' || c.is_whitespace()).filter(|c| c.starts_with("crate:") || c.starts_with("suite:")).map(String::from).collect(),
        ask: None,
    };
    let (members, facts, fences) = (moves.join(","), serde_json::to_string(&facts).map_err(|e| e.to_string())?, serde_json::to_string(&fences).map_err(|e| e.to_string())?);
    let t = target(spec);
    let mut args = vec!["mending", "mend"];
    args.extend(t.iter().map(String::as_str));
    args.extend(["--members", &members, "--facts", &facts, "--fences", &fences, "--actor", ACTOR]);
    lc_ok(w, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Fake {
        files: HashMap<String, String>,
        git_out: HashMap<String, String>,
        lc_answers: HashMap<String, (i32, String)>,
        calls: RefCell<Vec<String>>,
        deadline: Result<u64, String>,
    }

    impl Fake {
        fn new() -> Fake {
            let spec = r#"{"batch":"r-9","pass":3,"suite":"test-x.sh","repo":"/repo","base":"main","output":"ok 1\nfork: retry: Resource temporarily unavailable\n",
                "members":[{"bead":"sp-a","tip":"aaa"},{"bead":"sp-b","tip":"bbb"}]}"#;
            let mut f = Fake { files: HashMap::new(), git_out: HashMap::new(), lc_answers: HashMap::new(), calls: RefCell::new(vec![]), deadline: Ok(417) };
            f.files.insert("/spec".into(), spec.into());
            f.files.insert("/repo/spira/test-x.sh".into(), "#!/bin/bash\n# covers: spira/*.sh\nset -u\n".into());
            f.git_out.insert("diff --name-only main...aaa".into(), "spira/lib.sh\n".into());
            f.git_out.insert("diff main...aaa".into(), "+changed lib".into());
            f.git_out.insert("diff --name-only main...bbb".into(), "docs/a.md\n".into());
            f.git_out.insert("diff main...bbb".into(), "+changed doc".into());
            f
        }
        fn lc_calls(&self) -> Vec<String> {
            self.calls.borrow().iter().filter(|c| c.starts_with("lc ")).cloned().collect()
        }
    }

    impl World for Fake {
        fn lc(&self, args: &[&str]) -> (i32, String) {
            let key = args.join(" ");
            self.calls.borrow_mut().push(format!("lc {key}"));
            self.lc_answers.get(&key).cloned().unwrap_or((0, format!("<{}>", args[..2.min(args.len())].join(" "))))
        }
        fn git(&self, _repo: &str, args: &[&str]) -> Result<String, String> {
            self.git_out.get(&args.join(" ")).cloned().ok_or_else(|| format!("no git answer for {args:?}"))
        }
        fn read(&self, path: &str) -> Result<String, String> {
            self.files.get(path).cloned().ok_or_else(|| format!("no file {path}"))
        }
        fn deadline_secs(&self) -> Result<u64, String> {
            self.deadline.clone()
        }
        fn wiki(&self) -> Result<String, String> {
            Ok("/wiki".into())
        }
    }

    #[test]
    fn the_pack_points_at_the_touching_member_and_gives_its_whole_bead() {
        let mut w = Fake::new();
        w.lc_answers.insert("content show sp-a".into(), (0, "DESC see brain wiki/projects/x/design.md for it".into()));
        w.files.insert("/wiki/projects/x/design.md".into(), "THE DESIGN".into());
        let text = pack(&w, "/spec").unwrap();
        assert!(text.contains("EAGAIN at output line 2"), "{text}");
        assert!(text.contains("- sp-a @ aaa: spira/lib.sh"), "{text}");
        assert!(text.contains("## Bead sp-a") && text.contains("DESC see brain"), "{text}");
        assert!(text.contains("THE DESIGN"), "{text}");
        assert!(!text.contains("## Bead sp-b"), "a member the evidence does not point at gets no whole-bead section");
        assert!(text.contains("+changed doc"), "every diff is in the pack");
    }

    #[test]
    fn an_unreadable_design_says_so() {
        let mut w = Fake::new();
        w.lc_answers.insert("content show sp-a".into(), (0, "see wiki/gone.md".into()));
        assert!(pack(&w, "/spec").unwrap().contains("(unreadable: no file /wiki/gone.md)"));
    }

    #[test]
    fn design_paths_are_found_once_and_prose_is_ignored() {
        assert_eq!(design_paths("brain wiki/a/b.md, and (wiki/a/b.md) and wiki/c"), vec!["a/b.md"]);
    }

    #[test]
    fn a_bead_that_cannot_be_read_fails_the_pack_rather_than_hiding() {
        let mut w = Fake::new();
        w.lc_answers.insert("content show sp-a".into(), (2, "cannot tell".into()));
        assert!(pack(&w, "/spec").unwrap_err().contains("refused"));
    }

    #[test]
    fn start_sends_the_configured_deadline_and_a_missing_one_refuses() {
        let mut w = Fake::new();
        start(&w, "/spec").unwrap();
        assert_eq!(w.lc_calls(), vec!["lc mending pickup r-9 --pass 3 --suite test-x.sh --deadline-secs 417 --actor mender"]);
        w.deadline = Err("SPIRA_MENDER_DEADLINE_SECS is not a registered config key".into());
        assert!(start(&w, "/spec").is_err());
    }

    #[test]
    fn an_outcome_stamps_triage_then_the_diagnosis_then_the_terminal_event() {
        let w = Fake::new();
        finish(&w, "/spec", "return", "wrong approach at spira/lib.sh:12", None).unwrap();
        let calls: Vec<String> = w.lc_calls().into_iter().filter(|c| c.contains("mending event")).collect();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].contains("TriageEnded") && calls[1].contains("Diagnosis") && calls[2].contains("\"Return\""), "{calls:?}");
        assert!(calls.iter().all(|c| c.contains("--pass 3 --suite test-x.sh") && c.ends_with("--actor mender")));
    }

    fn mend_world() -> Fake {
        let mut w = Fake::new();
        w.lc_answers.insert("mending tips --beads sp-a,sp-b".into(), (0, "{}".into()));
        w.git_out.insert("rev-parse --verify refs/heads/spira/sp-a^{commit}".into(), "a2\n".into());
        w.git_out.insert("rev-parse --verify refs/heads/spira/sp-b^{commit}".into(), "bbb\n".into());
        w.git_out.insert("merge-base --is-ancestor aaa a2".into(), String::new());
        w.git_out.insert("diff --numstat --no-renames aaa a2".into(), "3\t1\tmender/src/x.rs\n".into());
        w.git_out.insert("diff --numstat --no-renames main aaa".into(), "1\t0\tmender/src/x.rs\n".into());
        w.git_out.insert("diff --numstat --no-renames main bbb".into(), "1\t0\tdocs/a.md\n".into());
        w.git_out.insert("cat-file -e a2:mender/Cargo.toml".into(), String::new());
        w.git_out.insert("show main:spira/mend-fences".into(), "# fences\nspira/gate-suites\n".into());
        w
    }

    #[test]
    fn a_mended_outcome_goes_through_mending_mend_with_the_branch_head_as_the_tip() {
        let w = mend_world();
        finish(&w, "/spec", "mended", "fixed", Some("crate:mender")).unwrap();
        let calls = w.lc_calls();
        assert!(!calls.iter().any(|c| c.contains("Mended")), "{calls:?}");
        let mend = calls.iter().find(|c| c.starts_with("lc mending mend ")).expect("mending mend was called");
        assert!(mend.starts_with("lc mending mend r-9 --pass 3 --suite test-x.sh --members sp-a:aaa:a2 --facts "), "{mend}");
        assert!(mend.contains(r#""required_checks":["crate:mender"]"#) && mend.contains(r#""checked":["crate:mender"]"#), "{mend}");
        assert!(mend.contains(r#"--fences ["spira/gate-suites"] --actor mender"#), "{mend}");
    }

    #[test]
    fn a_mended_outcome_with_no_moved_branch_sends_no_mend() {
        let mut w = mend_world();
        w.git_out.insert("rev-parse --verify refs/heads/spira/sp-a^{commit}".into(), "aaa".into());
        assert!(finish(&w, "/spec", "mended", "fixed", Some("crate:mender")).unwrap_err().contains("no member branch has moved"));
        assert!(!w.lc_calls().iter().any(|c| c.starts_with("lc mending mend ")));
    }

    #[test]
    fn triage_is_stamped_once_and_not_again_at_the_outcome() {
        let mut w = Fake::new();
        w.lc_answers.insert(
            "mending list --batch r-9".into(),
            (0, r#"[{"pass":3,"suite":"test-x.sh","triage_ended_at":1700000000}]"#.into()),
        );
        finish(&w, "/spec", "flaky", "passes on its own rerun in the history", None).unwrap();
        let events: Vec<String> = w.lc_calls().into_iter().filter(|c| c.contains("mending event")).collect();
        assert_eq!(events.len(), 2);
        assert!(!events.iter().any(|c| c.contains("TriageEnded")), "{events:?}");
    }

    #[test]
    fn an_already_terminal_row_is_a_no_op_and_sends_no_event() {
        let mut w = Fake::new();
        w.lc_answers.insert(
            "mending list --batch r-9".into(),
            (0, r#"[{"pass":3,"suite":"test-x.sh","state":"RETURNED","triage_ended_at":1}]"#.into()),
        );
        let out = finish(&w, "/spec", "environment", "EAGAIN at output line 2", None).unwrap();
        assert!(out.contains("no-op") && out.contains("RETURNED"), "{out}");
        assert!(!w.lc_calls().iter().any(|c| c.contains("mending event")), "{:?}", w.lc_calls());
    }

    #[test]
    fn a_refused_outcome_sends_nothing() {
        let w = Fake::new();
        assert!(finish(&w, "/spec", "return", "no location given", None).is_err());
        assert!(finish(&w, "/spec", "mended", "fixed", None).is_err());
        assert!(finish(&w, "/spec", "fixed", "x", None).is_err());
        assert!(w.lc_calls().is_empty());
    }

    #[test]
    fn a_machine_refusal_of_the_terminal_event_is_the_answer() {
        let mut w = Fake::new();
        let kind = r#"{"Environment":{"at":0}}"#;
        w.lc_answers.insert(format!("mending event r-9 --pass 3 --suite test-x.sh --kind {kind} --actor mender"), (3, "refused: DeadlinePassed".into()));
        let err = finish(&w, "/spec", "environment", "EAGAIN at output line 2", None).unwrap_err();
        assert!(err.contains("DeadlinePassed"), "{err}");
    }

    #[test]
    fn the_read_verbs_ask_the_machine_for_the_rounds_own_facts() {
        let w = Fake::new();
        failures(&w, "/spec").unwrap();
        passes(&w, "/spec").unwrap();
        verdicts(&w, "/spec").unwrap();
        assert_eq!(
            w.lc_calls(),
            vec!["lc mending list --batch r-9", "lc history r-9 --machine batch", "lc facts --ids sp-a,sp-b"]
        );
    }

    #[test]
    fn diff_gives_one_members_whole_diff_and_refuses_a_stranger() {
        let w = Fake::new();
        assert_eq!(diff(&w, "/spec", "sp-b").unwrap(), "+changed doc");
        assert!(diff(&w, "/spec", "sp-zz").is_err());
    }

    #[test]
    fn a_malformed_spec_is_refused() {
        let mut w = Fake::new();
        w.files.insert("/spec".into(), "{}".into());
        assert!(pack(&w, "/spec").unwrap_err().contains("not a failure spec"));
    }
}
