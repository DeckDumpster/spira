//! The moot-sweep, in-process (was `cockpit/moot-sweep.sh --apply`, which sourced conf.sh
//! through db.sh and filtered with an inline python3): resolve the asks whose `MOOT-WHEN:`
//! predicate now exits 0.
//!
//! Three invariants, unchanged. An ask with no MOOT-WHEN is never touched. A predicate that
//! errors or times out leaves the ask open (a failed probe never reads as cleared). Resolution
//! goes through `resolve`, never a raw close.

use runtime::ports::{Proc, ProcOutput, ProcSpec};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const RESOLVE_DEADLINE: Duration = Duration::from_secs(5);

pub struct MootCfg {
    /// `COCKPIT_DB`: the beads database, verified to hold `.beads` before anything is read.
    pub db: Option<String>,
    pub lc: spira_store::lc_call::LcClient,
    pub ask_label: String,
    /// `SPIRA_MOOT_TIMEOUT`: each predicate's bound, seconds.
    pub timeout_s: u64,
    /// `SPIRA_RESOLVE_SH`: the `resolve` program.
    pub resolve: String,
    /// Runs the predicates and `resolve`.
    pub proc: Arc<dyn Proc>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub found: u32,
    pub resolved: u32,
    pub errors: u32,
}

/// The asks a sweep checks: ask-labelled, `open`, and carrying a predicate. `rows` is
/// `content list --json` output, any warning lines before the payload skipped.
pub fn asks_of(rows: &str, ask_label: &str) -> Vec<(String, String)> {
    let start = rows.lines().position(|l| l.starts_with('[') || l.starts_with('{'));
    let Some(start) = start else { return Vec::new() };
    let payload = rows.lines().skip(start).collect::<Vec<_>>().join("\n");
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&payload) else { return Vec::new() };
    let rows = match &doc {
        serde_json::Value::Array(a) => a.clone(),
        v => v.get("issues").and_then(|i| i.as_array()).cloned().unwrap_or_default(),
    };
    let mut out = Vec::new();
    for r in rows {
        let labelled = r.get("labels").and_then(|l| l.as_array()).is_some_and(|l| l.iter().any(|x| x.as_str() == Some(ask_label)));
        if !labelled || !spira_config::nonwork::is_open(spira_config::nonwork::Kind::Ask, spira_config::nonwork::status_of(spira_config::nonwork::Kind::Ask, &r)) {
            continue;
        }
        let desc = r.get("description").and_then(|d| d.as_str()).unwrap_or("");
        if let Some(pred) = desc.lines().find_map(moot_when) {
            out.push((r.get("id").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default(), pred));
        }
    }
    out
}

/// `^\s*MOOT-WHEN:\s*(.+)$` on one line, the group stripped.
fn moot_when(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("MOOT-WHEN:")?;
    (!rest.is_empty()).then(|| rest.trim().to_string())
}

/// The first 300 bytes of a predicate's output, as `head -c 300` cuts them.
fn short(out: &[u8]) -> String {
    let trimmed = {
        let mut end = out.len();
        while end > 0 && out[end - 1] == b'\n' {
            end -= 1;
        }
        &out[..end]
    };
    String::from_utf8_lossy(&trimmed[..trimmed.len().min(300)]).into_owned()
}

fn run(cfg: &MootCfg, prog: &str, args: [&str; 2], deadline: Duration) -> Result<ProcOutput, String> {
    cfg.proc.run(&ProcSpec { prog: prog.into(), args: args.map(String::from).to_vec(), stdin: vec![], deadline })
}

/// One predicate's exit code, and its stdout then stderr; `Err` when it could not be run or
/// passed its deadline.
fn check(cfg: &MootCfg, pred: &str) -> Result<(Option<i32>, Vec<u8>), String> {
    // FORK-EDGE: operator predicate — MOOT-WHEN is a shell command by contract (W1 allow-list: bash)
    let out = run(cfg, "bash", ["-c", pred], Duration::from_secs(cfg.timeout_s))?;
    Ok((out.code, [out.stdout, out.stderr].concat()))
}

/// The open asks' rows, or `None` when the store did not answer.
fn rows(cfg: &MootCfg) -> Option<String> {
    // db.sh read stdout whatever the exit code, so any answer carrying a payload is read.
    let labels = format!("insight,{},overseer", cfg.ask_label);
    let out = cfg.lc.call(&["content", "list", "--all", "--limit", "0", "--label-any", &labels, "--json"]).ok()?.out;
    Some(out).filter(|t| t.lines().any(|l| l.starts_with('[') || l.starts_with('{')))
}

/// One pass; with `apply`, each cleared ask is resolved.
pub fn sweep(cfg: &MootCfg, apply: bool) -> Summary {
    let mut s = Summary::default();
    let Some(_db) = cfg.db.as_deref().filter(|d| Path::new(d).join(".beads").is_dir()) else { return s };
    let Some(rows) = rows(cfg) else { return s };
    for (id, pred) in asks_of(&rows, &cfg.ask_label) {
        if id.is_empty() {
            continue;
        }
        s.found += 1;
        match check(cfg, &pred) {
            Ok((Some(0), out)) if apply => {
                let reason = format!("auto-resolved: MOOT-WHEN predicate cleared — {pred} → exit 0. Evidence: {}", short(&out));
                if run(cfg, &cfg.resolve, [&id, &reason], RESOLVE_DEADLINE).is_ok_and(|o| o.code == Some(0)) {
                    s.resolved += 1;
                } else {
                    s.errors += 1;
                }
            }
            // 124 is a predicate's own `timeout` giving up.
            Ok((Some(124), _)) | Err(_) => s.errors += 1,
            Ok(_) => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(id: &str, status: &str, labels: &[&str], desc: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": status, "labels": labels, "description": desc})
    }

    #[test_id(tao5a5x)]
    #[test]
    fn only_open_labelled_asks_with_a_predicate_are_checked() {
        let rows = serde_json::Value::Array(vec![
            // literal-ok: test fixture label
            ask("sp-1", "open", &["needs-ryan"], "why\n  MOOT-WHEN:  test -e /x  \nmore"),
            // literal-ok: test fixture label
            ask("sp-2", "open", &["needs-ryan"], "no predicate"),
            // literal-ok: test fixture label
            ask("sp-3", "closed", &["needs-ryan"], "MOOT-WHEN: true"),
            ask("sp-4", "open", &["insight"], "MOOT-WHEN: true"),
        ]);
        let text = format!("warning: something\n{rows}");
        // literal-ok: test fixture label
        assert_eq!(asks_of(&text, "needs-ryan"), vec![("sp-1".to_string(), "test -e /x".to_string())]);
        // literal-ok: test fixture label
        assert!(asks_of("not json", "needs-ryan").is_empty());
    }

    const RESOLVE: &str = "scripted-resolve";
    type Answer = Result<(i32, &'static str), &'static str>;

    /// Answers a predicate by its text and `resolve` by its program, recording every spec.
    struct ScriptedProc {
        predicates: Vec<(&'static str, Answer)>,
        resolve: Answer,
        seen: std::sync::Mutex<Vec<ProcSpec>>,
    }

    impl ScriptedProc {
        fn new(predicates: &[(&'static str, Answer)], resolve: Answer) -> Arc<Self> {
            Arc::new(ScriptedProc { predicates: predicates.to_vec(), resolve, seen: Default::default() })
        }

        fn resolve_args(&self) -> Vec<Vec<String>> {
            self.seen.lock().unwrap().iter().filter(|s| s.prog == RESOLVE).map(|s| s.args.clone()).collect()
        }

        fn predicate_deadlines(&self) -> Vec<Duration> {
            self.seen.lock().unwrap().iter().filter(|s| s.prog == "bash").map(|s| s.deadline).collect()
        }
    }

    impl Proc for ScriptedProc {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            self.seen.lock().unwrap().push(spec.clone());
            let answer = if spec.prog == RESOLVE {
                self.resolve
            } else {
                assert_eq!((spec.prog.as_str(), spec.args[0].as_str()), ("bash", "-c"));
                self.predicates.iter().find(|(p, _)| *p == spec.args[1]).expect("a scripted predicate").1
            };
            answer.map(|(code, out)| ProcOutput { code: Some(code), stdout: out.into(), stderr: vec![] }).map_err(String::from)
        }
    }

    /// One open ask per predicate, ids `sp-0`.. in order, behind a scripted store.
    fn sweep_over(preds: &[&str], proc: Arc<dyn Proc>, apply: bool) -> Summary {
        let d = testkit::TempDir::new("wt-moot");
        std::fs::create_dir_all(d.join("db/.beads")).unwrap();
        // literal-ok: test fixture label
        let rows = preds.iter().enumerate().map(|(i, p)| ask(&format!("sp-{i}"), "open", &["needs-ryan"], &format!("MOOT-WHEN: {p}")));
        let (_, lc) = crate::lc::scripted::scripted(0, &serde_json::Value::Array(rows.collect()).to_string());
        sweep(&cfg_over(&d, lc, proc), apply)
    }

    fn cfg_over(d: &testkit::TempDir, lc: spira_store::lc_call::LcClient, proc: Arc<dyn Proc>) -> MootCfg {
        MootCfg {
            db: Some(d.join("db").to_string_lossy().into_owned()),
            lc,
            // literal-ok: test fixture label
            ask_label: "needs-ryan".into(),
            timeout_s: 1,
            resolve: RESOLVE.into(),
            proc,
        }
    }

    #[test_id(tq6a3fe)]
    #[test]
    fn a_cleared_predicate_is_resolved_a_live_one_kept_and_a_timeout_counted() {
        let d = testkit::TempDir::new("wt-moot");
        std::fs::create_dir_all(d.join("db/.beads")).unwrap();
        let rows = serde_json::Value::Array(vec![
            // literal-ok: test fixture label
            ask("sp-c", "open", &["needs-ryan"], "MOOT-WHEN: echo gone"),
            // literal-ok: test fixture label
            ask("sp-l", "open", &["needs-ryan"], "MOOT-WHEN: false"),
            // literal-ok: test fixture label
            ask("sp-t", "open", &["needs-ryan"], "MOOT-WHEN: sleep 5"),
        ]);
        let (t, lc) = crate::lc::scripted::scripted(0, &rows.to_string());
        let proc = ScriptedProc::new(&[("echo gone", Ok((0, "gone\n"))), ("false", Ok((1, ""))), ("sleep 5", Err("bash exceeded its 1s deadline"))], Ok((0, "")));
        assert_eq!(sweep(&cfg_over(&d, lc, proc.clone()), true), Summary { found: 3, resolved: 1, errors: 1 });
        assert_eq!(proc.resolve_args(), vec![vec!["sp-c", "auto-resolved: MOOT-WHEN predicate cleared — echo gone → exit 0. Evidence: gone"]]);
        assert_eq!(proc.predicate_deadlines(), vec![Duration::from_secs(1); 3]);
        // literal-ok: test fixture label
        assert_eq!(t.seen.lock().unwrap()[0], vec!["content", "list", "--all", "--limit", "0", "--label-any", "insight,needs-ryan,overseer", "--json"]);
    }

    #[test_id(tkzkxs5)]
    #[test]
    fn a_predicate_that_cannot_be_run_counts_an_error_and_resolves_nothing() {
        let refused: Answer = Err("spawn bash: resource temporarily unavailable");
        let proc = ScriptedProc::new(&[("a", refused), ("b", refused)], Ok((0, "")));
        assert_eq!(sweep_over(&["a", "b"], proc.clone(), true), Summary { found: 2, resolved: 0, errors: 2 });
        assert!(proc.resolve_args().is_empty());
    }

    #[test_id(t524z1b)]
    #[test]
    fn a_predicate_exiting_124_counts_an_error_and_resolves_nothing() {
        let proc = ScriptedProc::new(&[("a", Ok((124, "")))], Ok((0, "")));
        assert_eq!(sweep_over(&["a"], proc.clone(), true), Summary { found: 1, resolved: 0, errors: 1 });
        assert!(proc.resolve_args().is_empty());
    }

    #[test_id(tza9cz2)]
    #[test]
    fn a_resolve_that_fails_counts_an_error_and_no_resolution() {
        for failed in [Ok((1, "")), Err("scripted-resolve exceeded its 5s deadline")] {
            let proc = ScriptedProc::new(&[("a", Ok((0, "")))], failed);
            assert_eq!(sweep_over(&["a"], proc.clone(), true), Summary { found: 1, resolved: 0, errors: 1 });
            assert_eq!(proc.resolve_args().len(), 1);
        }
    }

    #[test_id(tpqdbpt)]
    #[test]
    fn without_apply_a_cleared_predicate_is_neither_resolved_nor_an_error() {
        let proc = ScriptedProc::new(&[("a", Ok((0, "")))], Ok((0, "")));
        assert_eq!(sweep_over(&["a"], proc.clone(), false), Summary { found: 1, resolved: 0, errors: 0 });
        assert!(proc.resolve_args().is_empty());
    }

    #[test_id(t7swceo)]
    #[test]
    fn no_database_checks_nothing() {
        let proc = runtime::ports::CountingProc::refusing();
        let cfg = MootCfg { db: Some("does/not/exist".into()), lc: crate::lc::scripted::scripted(0, "[]").1, ask_label: "a".into(), timeout_s: 1, resolve: RESOLVE.into(), proc: proc.clone() };
        assert_eq!(sweep(&cfg, true), Summary::default());
        assert_eq!(proc.spawned(), 0);
    }
}
