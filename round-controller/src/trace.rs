//! `round-controller trace`: the two adversarial tracers `law-a-twice-failed-landing-is-traced-before-the-third`
//! names, spawned from a round's members. Their predictions land in `<run>/rounds/<batch>.trace.md`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};

pub const VM_TEMPLATE: &str = include_str!("../trace/vm.md");
pub const HOST_TEMPLATE: &str = include_str!("../trace/host.md");

/// What has killed rounds before; both briefs open with this list.
pub const FAILURE_CLASSES: [&str; 6] = [
    "duplicate keys in merged TOML (two members each add the same key or table to one file)",
    "undeclared env on the VM (a test or script reads a variable the VM's environment does not set)",
    "a comment that a lint test matches (prose naming a banned token or a mechanism a matcher reads)",
    "missing image packages (a tool a test needs that the round image does not install)",
    "restart storms at activation (a unit or timer change that restarts services in a loop)",
    "record/system disagreement (a half-done activation whose record says done, or the reverse)",
];

const GIT_DEADLINE: Duration = Duration::from_secs(120); // batch-job: merge-tree of a large repo
const TRACER_TOOLS: &str = "Read,Grep,Glob,Bash(git show:*),Bash(git ls-tree:*),Bash(git grep:*),Bash(git log:*),Bash(git diff:*),Bash(git cat-file:*)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub tip: String,
}

/// `id:tip,...` (or `id@tip`). A bare `id` has no tip yet: [`resolve_tips`] reads its branch.
pub fn parse_members(spec: &str) -> Result<Vec<Member>, String> {
    let out: Vec<Member> = spec
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|m| {
            let (id, tip) = m.split_once([':', '@']).unwrap_or((m, ""));
            if id.is_empty() {
                return Err(format!("member {m:?} names no bead id"));
            }
            if !tip.is_empty() && !(tip.len() >= 7 && tip.bytes().all(|b| b.is_ascii_hexdigit())) {
                return Err(format!("member {m:?}: {tip:?} is not a commit sha"));
            }
            Ok(Member { id: id.to_string(), tip: tip.to_string() })
        })
        .collect::<Result<_, _>>()?;
    if out.is_empty() {
        return Err("no members".to_string());
    }
    Ok(out)
}

/// A spec is a member list when it holds a tip or a comma; otherwise it is a batch name.
pub fn is_member_list(spec: &str) -> bool {
    spec.contains([':', '@', ','])
}

/// The member ids of the newest `cert-pass` row of `batch` that recorded any.
pub fn batch_members(cert_pass: &str, batch: &str) -> Option<Vec<String>> {
    cert_pass
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r["family"] == "cert-pass" && r["batch"] == batch)
        .filter_map(|r| {
            let ids: Vec<String> = r["members"].as_array()?.iter().filter_map(|m| m.as_str().map(str::to_string)).collect();
            (!ids.is_empty()).then_some(ids)
        })
        .next_back()
}

/// The batch name a trace file is keyed by: the batch itself, or one derived from the member set.
pub fn trace_name(spec: &str, members: &[Member]) -> String {
    if !is_member_list(spec) {
        return spec.replace('/', "-");
    }
    let mut ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    ids.sort_unstable();
    format!("members-{}", ids.join("+"))
}

pub fn trace_path(run: &Path, name: &str) -> PathBuf {
    run.join("rounds").join(format!("{name}.trace.md"))
}

fn git(p: &dyn Proc, args: &[&str]) -> Result<(Option<i32>, String), String> {
    let o = p.run(&ProcSpec { prog: "git".into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: Vec::new(), deadline: GIT_DEADLINE })?;
    let text = String::from_utf8_lossy(if o.stdout.is_empty() { &o.stderr } else { &o.stdout }).trim().to_string();
    Ok((o.code, text))
}

fn git_ok(p: &dyn Proc, args: &[&str]) -> Result<String, String> {
    match git(p, args)? {
        (Some(0), out) => Ok(out),
        (code, out) => Err(format!("git {} exited {code:?}: {out}", args.join(" "))),
    }
}

/// Gives a member with no tip the tip of its branch `spira/<id>`.
pub fn resolve_tips(p: &dyn Proc, members: Vec<Member>) -> Result<Vec<Member>, String> {
    members
        .into_iter()
        .map(|m| {
            if !m.tip.is_empty() {
                return Ok(m);
            }
            let tip = git_ok(p, &["rev-parse", "--verify", &format!("refs/heads/spira/{}^{{commit}}", m.id)]).map_err(|e| format!("member {} has no tip given and no branch: {e}", m.id))?;
            Ok(Member { tip, ..m })
        })
        .collect()
}

/// The tree of `base` with every tip merged in, one at a time; a conflict names the member.
pub fn merged_tree(p: &dyn Proc, base: &str, members: &[Member]) -> Result<String, String> {
    let mut cur = git_ok(p, &["rev-parse", "--verify", &format!("{base}^{{commit}}")])?;
    let mut tree = git_ok(p, &["rev-parse", "--verify", &format!("{cur}^{{tree}}")])?;
    for m in members {
        match git(p, &["merge-tree", "--write-tree", &cur, &m.tip])? {
            (Some(0), out) => tree = out.lines().next().unwrap_or_default().to_string(),
            (Some(1), out) => return Err(format!("member {} conflicts with the base and the members before it:\n{out}", m.id)),
            (code, out) => return Err(format!("git merge-tree exited {code:?} on member {}: {out}", m.id)),
        }
        cur = git_ok(p, &["commit-tree", &tree, "-p", &cur, "-p", &m.tip, "-m", "round trace"])?;
    }
    Ok(tree)
}

/// A member's line in the brief: its tip and how much it changes against the base, so an empty member shows.
pub fn member_line(p: &dyn Proc, base: &str, m: &Member) -> String {
    match git_ok(p, &["diff", "--numstat", &format!("{base}...{}", m.tip)]) {
        Ok(stat) if stat.is_empty() => format!("- {}:{} EMPTY: no change against the base (an uncommitted fix lands as nothing)", m.id, m.tip),
        Ok(stat) => format!("- {}:{} changes {} file(s) against the base", m.id, m.tip, stat.lines().count()),
        Err(e) => format!("- {}:{} (diff against the base unreadable: {e})", m.id, m.tip),
    }
}

#[derive(Debug, Clone)]
pub struct Facts {
    pub batch: String,
    pub base: String,
    pub merged_tree: String,
    pub installed: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracePath {
    Vm,
    Host,
}

impl TracePath {
    pub const BOTH: [TracePath; 2] = [TracePath::Vm, TracePath::Host];

    pub fn word(self) -> &'static str {
        match self {
            TracePath::Vm => "vm",
            TracePath::Host => "host",
        }
    }

    fn template(self) -> &'static str {
        match self {
            TracePath::Vm => VM_TEMPLATE,
            TracePath::Host => HOST_TEMPLATE,
        }
    }
}

pub fn render_brief(path: TracePath, f: &Facts) -> String {
    let classes: Vec<String> = FAILURE_CLASSES.iter().map(|c| format!("- {c}")).collect();
    path.template()
        .replace("{batch}", &f.batch)
        .replace("{base}", &f.base)
        .replace("{merged_tree}", &f.merged_tree)
        .replace("{installed}", &f.installed)
        .replace("{members}", &f.members.join("\n"))
        .replace("{classes}", &classes.join("\n"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Certain,
    Likely,
    Possible,
}

impl Confidence {
    fn word(self) -> &'static str {
        match self {
            Confidence::Certain => "certain",
            Confidence::Likely => "likely",
            Confidence::Possible => "possible",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prediction {
    pub confidence: Confidence,
    pub failure: String,
    pub evidence: String,
    pub preventive: String,
}

/// The `- <confidence> | failure | evidence | preventive` lines of a tracer's report. A line that
/// does not parse is dropped, never read as "certain".
pub fn parse_predictions(report: &str) -> Vec<Prediction> {
    report
        .lines()
        .filter_map(|l| {
            let mut f = l.trim().strip_prefix("- ")?.splitn(4, '|').map(str::trim);
            let confidence = match f.next()?.to_ascii_lowercase().as_str() {
                "certain" => Confidence::Certain,
                "likely" => Confidence::Likely,
                "possible" => Confidence::Possible,
                _ => return None,
            };
            Some(Prediction { confidence, failure: f.next()?.to_string(), evidence: f.next()?.to_string(), preventive: f.next()?.to_string() })
        })
        .collect()
}

pub struct Report {
    pub path: TracePath,
    pub outcome: Result<String, String>,
}

pub fn render_trace(f: &Facts, reports: &[Report]) -> String {
    let mut out = format!("# Round trace {}\n\nbase: {}\nmerged tree: {}\ninstalled release: {}\n\nmembers:\n{}\n", f.batch, f.base, f.merged_tree, f.installed, f.members.join("\n"));
    for r in reports {
        out.push_str(&format!("\n## {} path\n\n", r.path.word()));
        match &r.outcome {
            Err(e) => out.push_str(&format!("tracer failed: {e}\n")),
            Ok(text) => {
                let ps = parse_predictions(text);
                for p in &ps {
                    out.push_str(&format!("- {} | {} | {} | {}\n", p.confidence.word(), p.failure, p.evidence, p.preventive));
                }
                if ps.is_empty() {
                    out.push_str("no predictions parsed from the report; raw report:\n\n");
                    out.push_str(text.trim_end());
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// The certain predictions across the reports, tagged with the path that made them.
pub fn certain(reports: &[Report]) -> Vec<(TracePath, Prediction)> {
    reports
        .iter()
        .filter_map(|r| r.outcome.as_ref().ok().map(|t| (r.path, t)))
        .flat_map(|(path, t)| parse_predictions(t).into_iter().filter(|p| p.confidence == Confidence::Certain).map(move |p| (path, p)))
        .collect()
}

pub struct Agent {
    pub program: String,
    pub model: String,
    pub deadline: Duration,
}

fn agent_spec(a: &Agent, brief: &str) -> ProcSpec {
    let args = ["-p", "--model", &a.model, "--tools", "Read,Grep,Glob,Bash", "--allowedTools", TRACER_TOOLS];
    ProcSpec { prog: a.program.clone(), args: args.iter().map(|x| x.to_string()).collect(), stdin: brief.as_bytes().to_vec(), deadline: a.deadline }
}

/// Runs both tracers at once and returns their reports, vm first.
pub fn run_tracers(p: &dyn Proc, a: &Agent, f: &Facts) -> Vec<Report> {
    std::thread::scope(|s| {
        let handles: Vec<_> = TracePath::BOTH
            .into_iter()
            .map(|path| {
                let spec = agent_spec(a, &render_brief(path, f));
                (path, s.spawn(move || p.run(&spec)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(path, h)| {
                let outcome = match h.join() {
                    Ok(Ok(o)) if o.code == Some(0) => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
                    Ok(Ok(o)) => Err(format!("exited {:?}: {}", o.code, String::from_utf8_lossy(&o.stderr).trim())),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("tracer thread panicked".to_string()),
                };
                Report { path, outcome }
            })
            .collect()
    })
}

pub struct Request {
    pub name: String,
    pub base: String,
    pub members: Vec<Member>,
    pub installed: String,
}

#[derive(Debug)]
pub struct Traced {
    pub file: PathBuf,
    pub certain: Vec<(TracePath, Prediction)>,
    pub failed: Vec<TracePath>,
}

/// Derives the facts, runs the tracers, writes the trace file; the certain predictions come back
/// for the caller to print and exit non-zero on, and the paths whose tracer produced no report.
pub fn trace(p: &dyn Proc, a: &Agent, run: &Path, r: &Request) -> Result<Traced, String> {
    let tree = merged_tree(p, &r.base, &r.members)?;
    let base = git_ok(p, &["rev-parse", "--verify", &format!("{}^{{commit}}", r.base)])?;
    let facts = Facts { batch: r.name.clone(), base, merged_tree: tree, installed: r.installed.clone(), members: r.members.iter().map(|m| member_line(p, &r.base, m)).collect() };
    let reports = run_tracers(p, a, &facts);
    let path = trace_path(run, &r.name);
    std::fs::create_dir_all(path.parent().unwrap_or(run)).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(&path, render_trace(&facts, &reports)).map_err(|e| format!("{}: {e}", path.display()))?;
    let failed = reports.iter().filter(|r| r.outcome.is_err()).map(|r| r.path).collect();
    Ok(Traced { file: path, certain: certain(&reports), failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::ProcOutput;
    use std::sync::Mutex;

    fn facts() -> Facts {
        Facts { batch: "b-1".into(), base: "ba5e".into(), merged_tree: "7ree5ha".into(), installed: "1n57a11ed".into(), members: vec!["- sp-a:aaaaaaa changes 2 file(s) against the base".into()] }
    }

    #[test]
    fn each_brief_carries_the_merged_tree_the_installed_sha_and_every_failure_class() {
        for path in TracePath::BOTH {
            let b = render_brief(path, &facts());
            assert!(b.contains("7ree5ha") && b.contains("1n57a11ed") && b.contains("sp-a:aaaaaaa"), "{b}");
            for c in FAILURE_CLASSES {
                assert!(b.contains(c), "{} brief lacks {c}", path.word());
            }
            assert!(!b.contains("{batch}") && !b.contains("{classes}") && !b.contains("{merged_tree}"), "unfilled placeholder in {b}");
        }
    }

    #[test]
    fn the_briefs_promise_only_the_tools_the_agent_is_allowed() {
        for path in TracePath::BOTH {
            let b = render_brief(path, &facts());
            assert!(!b.contains("grep and cat"), "{b}");
            for verb in ["show", "ls-tree", "grep", "log", "diff", "cat-file"] {
                assert!(b.contains(verb) && TRACER_TOOLS.contains(&format!("Bash(git {verb}:*)")), "{verb}");
            }
            let line = b.lines().find(|l| l.starts_with("- <certain|likely|possible>")).expect("the shape line");
            assert_eq!(parse_predictions(&line.replace("<certain|likely|possible>", "certain")).len(), 1, "{line}");
        }
    }

    #[test]
    fn members_parse_with_either_separator_and_a_bare_id_awaits_its_branch() {
        let m = parse_members("sp-a:abcdef1,sp-b@1234567,sp-c").unwrap();
        assert_eq!((m[0].tip.as_str(), m[1].tip.as_str(), m[2].tip.as_str()), ("abcdef1", "1234567", ""));
        assert!(parse_members("sp-a:nothex").is_err());
        assert!(parse_members(",").is_err());
        assert!(!is_member_list("spira-20261113T103854Z") && is_member_list("sp-a:abcdef1") && is_member_list("sp-a,sp-b"));
    }

    #[test]
    fn a_batch_resolves_to_the_newest_row_that_recorded_members() {
        let rows = r#"{"family":"cert-pass","batch":"b","members":["sp-a"]}
{"family":"cert-pass","batch":"b","members":["sp-a","sp-b"]}
{"family":"cert-pass","batch":"b","members":[]}
{"family":"cert-pass","batch":"other","members":["sp-z"]}"#;
        assert_eq!(batch_members(rows, "b"), Some(vec!["sp-a".into(), "sp-b".into()]));
        assert_eq!(batch_members(rows, "nope"), None);
    }

    #[test]
    fn predictions_parse_and_a_malformed_line_is_never_certain() {
        let report = "thinking...\n- certain | the TOML has a duplicate key | a.toml:3 | drop one\n- Likely | x | y | z\n- certain | only two | fields\n- none\n- maybe | a | b | c";
        let ps = parse_predictions(report);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].confidence, Confidence::Certain);
        assert_eq!(ps[0].evidence, "a.toml:3");
        assert_eq!(ps[1].confidence, Confidence::Likely);
    }

    struct Scripted {
        calls: Mutex<Vec<ProcSpec>>,
        reply: fn(&ProcSpec) -> Result<ProcOutput, String>,
    }

    impl Proc for Scripted {
        fn run(&self, s: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.calls).push(s.clone());
            (self.reply)(s)
        }
    }

    fn ok(stdout: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput { code: Some(0), stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() })
    }

    fn fake(s: &ProcSpec) -> Result<ProcOutput, String> {
        if s.prog == "git" {
            return match s.args[0].as_str() {
                "diff" if s.args[2].ends_with("...e0e0e0e") => ok(""),
                "diff" => ok("1\t0\tf.rs\n"),
                "merge-tree" => ok("7ree5ha\n"),
                _ => ok("c0ffee1\n"),
            };
        }
        let brief = String::from_utf8_lossy(&s.stdin);
        if brief.contains("Your path: the VM") {
            ok("- certain | the VM lacks bwrap | image.sh:9 | install it\n")
        } else {
            ok("- none\n")
        }
    }

    #[test]
    fn two_tracers_run_on_the_briefs_and_a_certain_failure_is_returned_and_written() {
        let run = testkit::TempDir::new("rc-trace");
        let p = Scripted { calls: Mutex::new(Vec::new()), reply: fake };
        let a = Agent { program: "agent".into(), model: "m-opus".into(), deadline: Duration::from_secs(5) };
        let req = Request {
            name: "b-1".into(),
            base: "local/main".into(),
            members: vec![Member { id: "sp-a".into(), tip: "aaaaaaa".into() }, Member { id: "sp-empty".into(), tip: "e0e0e0e".into() }],
            installed: "1n57a11ed".into(),
        };
        let Traced { file: path, certain, failed } = trace(&p, &a, run.path(), &req).unwrap();
        assert!(failed.is_empty());
        assert_eq!(path, run.path().join("rounds/b-1.trace.md"));
        assert_eq!(certain.len(), 1);
        assert_eq!(certain[0].0, TracePath::Vm);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("sp-empty:e0e0e0e EMPTY"), "{text}");
        assert!(text.contains("## vm path") && text.contains("## host path") && text.contains("- certain | the VM lacks bwrap | image.sh:9"), "{text}");
        let agents: Vec<_> = crate::lock(&p.calls).iter().filter(|c| c.prog == "agent").cloned().collect();
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().all(|c| c.args.contains(&"m-opus".to_string()) && String::from_utf8_lossy(&c.stdin).contains("1n57a11ed")));
    }

    #[test]
    fn a_tracer_that_fails_is_reported_in_the_file_and_names_nothing_certain() {
        let run = testkit::TempDir::new("rc-trace-fail");
        let p = Scripted { calls: Mutex::new(Vec::new()), reply: |s| if s.prog == "git" { ok("c0ffee1\n") } else { Err("spawn agent: no such file".into()) } };
        let a = Agent { program: "agent".into(), model: "m".into(), deadline: Duration::from_secs(5) };
        let req = Request { name: "b".into(), base: "local/main".into(), members: vec![Member { id: "sp-a".into(), tip: "aaaaaaa".into() }], installed: "i".into() };
        let Traced { file: path, certain, failed } = trace(&p, &a, run.path(), &req).unwrap();
        assert!(certain.is_empty());
        assert_eq!(failed, TracePath::BOTH);
        assert!(std::fs::read_to_string(path).unwrap().contains("tracer failed: spawn agent: no such file"));
    }

    #[test]
    fn a_conflicting_member_is_named_and_nothing_is_spawned() {
        let run = testkit::TempDir::new("rc-trace-conflict");
        let p = Scripted {
            calls: Mutex::new(Vec::new()),
            reply: |s| if s.args[0] == "merge-tree" { Ok(ProcOutput { code: Some(1), stdout: b"CONFLICT".to_vec(), stderr: Vec::new() }) } else { ok("c0ffee1\n") },
        };
        let a = Agent { program: "agent".into(), model: "m".into(), deadline: Duration::from_secs(5) };
        let req = Request { name: "b".into(), base: "local/main".into(), members: vec![Member { id: "sp-bad".into(), tip: "aaaaaaa".into() }], installed: "i".into() };
        let e = trace(&p, &a, run.path(), &req).unwrap_err();
        assert!(e.contains("sp-bad") && e.contains("CONFLICT"), "{e}");
        assert!(crate::lock(&p.calls).iter().all(|c| c.prog == "git"));
    }

    #[test]
    fn the_trace_file_is_keyed_by_batch_or_by_the_member_set() {
        let m = parse_members("sp-b:abcdef1,sp-a:1234567").unwrap();
        assert_eq!(trace_name("spira/q/2026", &m[..0]), "spira-q-2026");
        assert_eq!(trace_name("sp-b:abcdef1,sp-a:1234567", &m), "members-sp-a+sp-b");
    }
}
