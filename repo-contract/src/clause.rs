//! The five clauses of a repository's landing contract, each a pure function of the ports.

use crate::ports::Ports;
use std::path::Path;

pub const CHECKS: &str = ".spira/checks.toml";
pub const TESTENV: &str = ".spira/testenv.toml";
pub const DEPLOY: &str = ".spira/deploy.toml";
pub const WORKFLOWS: &str = ".github/workflows";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clause {
    Environment,
    SubmitChecks,
    TestTiers,
    Landing,
    Deploy,
}

pub const ALL: [Clause; 5] = [Clause::Environment, Clause::SubmitChecks, Clause::TestTiers, Clause::Landing, Clause::Deploy];

impl Clause {
    pub fn name(self) -> &'static str {
        match self {
            Clause::Environment => "environment",
            Clause::SubmitChecks => "submit-checks",
            Clause::TestTiers => "test-tiers",
            Clause::Landing => "landing",
            Clause::Deploy => "deploy",
        }
    }

    pub fn parse(s: &str) -> Option<Clause> {
        ALL.into_iter().find(|c| c.name() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Met(String),
    NotMet(String),
    CannotTell(String),
}

impl Outcome {
    pub fn line(&self, clause: Clause) -> String {
        match self {
            Outcome::Met(e) => format!("met {}: {e}", clause.name()),
            Outcome::NotMet(e) => format!("not met {}: {e}", clause.name()),
            Outcome::CannotTell(e) => format!("cannot tell {}: {e}", clause.name()),
        }
    }
}

pub fn judge(p: &dyn Ports, clause: Clause, repo: &str, tree: Option<&Path>) -> Outcome {
    let r = match clause {
        Clause::Environment => environment(p, repo, tree),
        Clause::SubmitChecks => submit_checks(p, repo, tree),
        Clause::TestTiers => test_tiers(p, repo),
        Clause::Landing => landing(p, repo),
        Clause::Deploy => deploy(p, repo, tree),
    };
    r.unwrap_or_else(Outcome::CannotTell)
}

type Judged = Result<Outcome, String>;

fn row_program(run: &str) -> Option<String> {
    if run.trim() == "{unit-build}" {
        return Some("cargo".into());
    }
    habits::config::argv(run, &[]).into_iter().next()
}

fn environment(p: &dyn Ports, repo: &str, tree: Option<&Path>) -> Judged {
    for f in ["flake.nix", "flake.lock"] {
        if p.read(repo, tree, f)?.is_none() {
            return Ok(Outcome::NotMet(format!("{f} is not in the tree")));
        }
    }
    let env = match p.shell_env(repo, tree) {
        Ok(Some(e)) => e,
        Ok(None) => return Ok(Outcome::NotMet("flake.nix declares no devShells".into())),
        Err(e) => return Ok(Outcome::NotMet(format!("the flake's dev shell does not enter: {e}"))),
    };
    let rows = match p.read(repo, tree, CHECKS)? {
        Some(t) => habits::config::parse_checks(&t).map_err(|e| format!("{CHECKS}: {e}"))?,
        None => Vec::new(),
    };
    let mut programs = Vec::new();
    for row in rows.iter().filter(|r| r.build.is_none()) {
        let Some(prog) = row_program(&row.run) else { continue };
        if prog.contains('/') {
            continue;
        }
        if !p.on_path(&env, &prog) {
            return Ok(Outcome::NotMet(format!("check {:?} runs {prog:?}, which the flake's dev shell does not provide", row.name)));
        }
        programs.push(format!("{}={prog}", row.name));
    }
    let image = p.read(repo, tree, TESTENV)?.and_then(|t| t.parse::<toml::Table>().ok()).and_then(|t| t.get("image").and_then(|v| v.as_str()).map(String::from));
    Ok(Outcome::Met(format!(
        "flake dev shell {} provides {}; unit tier runs in image {}",
        env.key,
        if programs.is_empty() { "no check programs to resolve".to_string() } else { programs.join(", ") },
        image.as_deref().unwrap_or("(none declared)")
    )))
}

fn submit_checks(p: &dyn Ports, repo: &str, tree: Option<&Path>) -> Judged {
    let Some(text) = p.read(repo, tree, CHECKS)? else {
        return Ok(Outcome::NotMet(format!("{CHECKS} does not exist")));
    };
    if let Err(e) = habits::config::parse_checks(&text) {
        return Ok(Outcome::NotMet(format!("{CHECKS} does not parse: {e}")));
    }
    let run = p.run_checks(repo, tree)?;
    let rows = run.lines.join("; ");
    Ok(if run.ok { Outcome::Met(rows) } else { Outcome::NotMet(format!("work checks fails: {rows}")) })
}

fn test_tiers(p: &dyn Ports, repo: &str) -> Judged {
    let Some(text) = p.read(repo, None, TESTENV)? else {
        return Ok(Outcome::NotMet(format!("{TESTENV} does not exist")));
    };
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => return Ok(Outcome::NotMet(format!("{TESTENV} does not parse: {e}"))),
    };
    let default: Vec<String> = table.get("default").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    if default.is_empty() {
        return Ok(Outcome::NotMet(format!("{TESTENV} declares no `default` tiers")));
    }
    let tiers = p.run_tiers(repo, &default)?;
    let verdicts = tiers.iter().map(|t| format!("{} {}", t.name, if t.ok { "ok" } else { "FAIL" })).collect::<Vec<_>>().join(", ");
    Ok(if tiers.iter().all(|t| t.ok) { Outcome::Met(verdicts) } else { Outcome::NotMet(format!("tiers: {verdicts}")) })
}

fn landing(p: &dyn Ports, repo: &str) -> Judged {
    let pieces = p.landing_pieces(repo);
    if let Some(bad) = pieces.iter().find(|x| !x.ok) {
        return Ok(Outcome::NotMet(format!("{}: {}", bad.name, bad.detail)));
    }
    let mode = p.land_mode(repo);
    if mode == "queue" || mode == "queue.forge" {
        let name = p.workflow(repo)?;
        let files = p.workflow_files(repo)?;
        let found = files.iter().any(|f| *f == name || f.strip_suffix(".yml").or_else(|| f.strip_suffix(".yaml")) == Some(name.as_str()));
        if !found {
            return Ok(Outcome::NotMet(format!("the forge workflow {name:?} is not a file under {WORKFLOWS}/ on the landing ref")));
        }
        return Ok(Outcome::Met(format!("{} registration pieces ok; forge workflow {name:?} is under {WORKFLOWS}/", pieces.len())));
    }
    Ok(Outcome::Met(format!("{} registration pieces ok; land mode {mode}", pieces.len())))
}

fn deploy(p: &dyn Ports, repo: &str, tree: Option<&Path>) -> Judged {
    let Some(text) = p.read(repo, tree, DEPLOY)? else {
        return Ok(Outcome::NotMet(format!("{DEPLOY} does not exist")));
    };
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => return Ok(Outcome::NotMet(format!("{DEPLOY} does not parse: {e}"))),
    };
    Ok(match table.get("after_landing").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Outcome::Met(format!("after_landing: {s}")),
        None => Outcome::NotMet(format!("{DEPLOY} has no non-empty `after_landing` string")),
    })
}

/// Judges each clause in order, emitting its line as it settles; a red clause never stops the
/// next. 0 when all are met, 1 when any is not met, else 2 when any could not be told.
pub fn run(p: &dyn Ports, repo: &str, tree: Option<&Path>, clauses: &[Clause], emit: &mut dyn FnMut(&str)) -> i32 {
    let (mut unmet, mut unknown) = (false, false);
    for &c in clauses {
        let o = judge(p, c, repo, tree);
        match o {
            Outcome::NotMet(_) => unmet = true,
            Outcome::CannotTell(_) => unknown = true,
            Outcome::Met(_) => {}
        }
        emit(&o.line(c));
    }
    if unmet { 1 } else if unknown { 2 } else { 0 }
}

/// The repositories `--all` covers: every registered one but the home repo, whose flake is the
/// harness's own and outside the contract.
pub fn without_home(all: Vec<String>, home: &str) -> Vec<String> {
    all.into_iter().filter(|n| n != home).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Piece, Rows, Tier};
    use crate::shell::ShellEnv;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct Fake {
        files: BTreeMap<String, String>,
        shell: Option<Vec<&'static str>>,
        shell_err: Option<String>,
        checks_ok: bool,
        tiers_ok: bool,
        bad_piece: Option<(&'static str, &'static str)>,
        mode: &'static str,
        workflow: &'static str,
        workflow_files: Vec<String>,
        repos: Vec<String>,
    }

    impl Ports for Fake {
        fn read(&self, _: &str, _: Option<&Path>, rel: &str) -> Result<Option<String>, String> {
            Ok(self.files.get(rel).cloned())
        }
        fn shell_env(&self, _: &str, _: Option<&Path>) -> Result<Option<ShellEnv>, String> {
            if let Some(e) = &self.shell_err {
                return Err(e.clone());
            }
            Ok(self.shell.as_ref().map(|progs| ShellEnv { vars: progs.iter().map(|p| (format!("has:{p}"), String::new())).collect(), key: "k1".into() }))
        }
        fn on_path(&self, env: &ShellEnv, program: &str) -> bool {
            env.vars.contains_key(&format!("has:{program}"))
        }
        fn run_checks(&self, _: &str, _: Option<&Path>) -> Result<Rows, String> {
            Ok(Rows { ok: self.checks_ok, lines: vec!["-- lint: ok".into(), "-- unit: ok".into()] })
        }
        fn run_tiers(&self, _: &str, suites: &[String]) -> Result<Vec<Tier>, String> {
            Ok(suites.iter().map(|s| Tier { name: s.clone(), ok: self.tiers_ok }).collect())
        }
        fn landing_pieces(&self, _: &str) -> Vec<Piece> {
            let mut v = vec![Piece { name: "registered".into(), detail: "in the repo registry".into(), ok: true }];
            if let Some((n, d)) = self.bad_piece {
                v.push(Piece { name: n.into(), detail: d.into(), ok: false });
            }
            v
        }
        fn land_mode(&self, _: &str) -> String {
            self.mode.into()
        }
        fn workflow(&self, _: &str) -> Result<String, String> {
            Ok(self.workflow.into())
        }
        fn workflow_files(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(self.workflow_files.clone())
        }
        fn product_repos(&self) -> Vec<String> {
            self.repos.clone()
        }
        fn registered(&self, r: &str) -> bool {
            self.repos.iter().any(|x| x == r)
        }
    }

    fn green() -> Fake {
        let mut f = Fake { shell: Some(vec!["cargo", "bash"]), checks_ok: true, tiers_ok: true, mode: "queue", workflow: "Gate", workflow_files: vec!["Gate.yml".into()], ..Fake::default() };
        for (k, v) in [
            ("flake.nix", "devShells"),
            ("flake.lock", "{}"),
            (CHECKS, "[[check]]\nname = \"lint\"\nrun = \"cargo clippy\"\nbudget_ms = 1000\n"),
            (TESTENV, "image = \"img:1\"\ndefault = [\"unit\", \"lint\"]\n"),
            (DEPLOY, "after_landing = \"curl the health url\"\n"),
        ] {
            f.files.insert(k.into(), v.into());
        }
        f
    }

    fn line(f: &Fake, c: Clause) -> String {
        judge(f, c, "r", None).line(c)
    }

    #[test]
    fn environment_met_names_the_shell_and_the_image() {
        assert_eq!(line(&green(), Clause::Environment), "met environment: flake dev shell k1 provides lint=cargo; unit tier runs in image img:1");
    }

    #[test]
    fn a_unit_build_row_runs_cargo_not_its_placeholder() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "[[check]]\nname = \"unit\"\nrun = \"{unit-build}\"\nbudget_ms = 1000\n".into());
        assert_eq!(line(&f, Clause::Environment), "met environment: flake dev shell k1 provides unit=cargo; unit tier runs in image img:1");
    }

    #[test]
    fn environment_not_met_names_the_row_and_program_the_shell_lacks() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "[[check]]\nname = \"lint\"\nrun = \"uv run ruff\"\nbudget_ms = 1000\n".into());
        assert_eq!(line(&f, Clause::Environment), "not met environment: check \"lint\" runs \"uv\", which the flake's dev shell does not provide");
    }

    #[test]
    fn environment_not_met_without_a_flake_lock_or_a_dev_shell_or_when_it_will_not_enter() {
        let mut f = green();
        f.files.remove("flake.lock");
        assert_eq!(line(&f, Clause::Environment), "not met environment: flake.lock is not in the tree");
        let mut f = green();
        f.shell = None;
        assert!(line(&f, Clause::Environment).contains("declares no devShells"));
        let mut f = green();
        f.shell_err = Some("boom".into());
        assert_eq!(line(&f, Clause::Environment), "not met environment: the flake's dev shell does not enter: boom");
    }

    #[test]
    fn a_built_row_is_not_judged_against_the_shell() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "[[check]]\nname = \"g\"\nrun = \"gate fence\"\nbudget_ms = 1\nbuild = \"gate\"\n".into());
        assert!(line(&f, Clause::Environment).starts_with("met environment"));
    }

    #[test]
    fn submit_checks_met_carries_each_rows_line_and_not_met_when_absent_unparsable_or_red() {
        assert_eq!(line(&green(), Clause::SubmitChecks), "met submit-checks: -- lint: ok; -- unit: ok");
        let mut f = green();
        f.files.remove(CHECKS);
        assert_eq!(line(&f, Clause::SubmitChecks), "not met submit-checks: .spira/checks.toml does not exist");
        f.files.insert(CHECKS.into(), "not [ toml".into());
        assert!(line(&f, Clause::SubmitChecks).starts_with("not met submit-checks: .spira/checks.toml does not parse"));
        let mut f = green();
        f.checks_ok = false;
        assert!(line(&f, Clause::SubmitChecks).starts_with("not met submit-checks: work checks fails: -- lint: ok"));
    }

    #[test]
    fn test_tiers_report_each_tier_and_need_a_default() {
        assert_eq!(line(&green(), Clause::TestTiers), "met test-tiers: unit ok, lint ok");
        let mut f = green();
        f.tiers_ok = false;
        assert_eq!(line(&f, Clause::TestTiers), "not met test-tiers: tiers: unit FAIL, lint FAIL");
        f.files.insert(TESTENV.into(), "image = \"x\"\n".into());
        assert_eq!(line(&f, Clause::TestTiers), "not met test-tiers: .spira/testenv.toml declares no `default` tiers");
        f.files.remove(TESTENV);
        assert_eq!(line(&f, Clause::TestTiers), "not met test-tiers: .spira/testenv.toml does not exist");
    }

    #[test]
    fn landing_needs_every_piece_and_for_a_forge_queue_the_workflow_file() {
        assert_eq!(line(&green(), Clause::Landing), "met landing: 1 registration pieces ok; forge workflow \"Gate\" is under .github/workflows/");
        let mut f = green();
        f.bad_piece = Some(("landref", "the landing ref does not resolve"));
        assert_eq!(line(&f, Clause::Landing), "not met landing: landref: the landing ref does not resolve");
        let mut f = green();
        f.workflow_files = vec!["other.yml".into()];
        assert!(line(&f, Clause::Landing).starts_with("not met landing: the forge workflow \"Gate\" is not a file"));
        f.mode = "queue.local";
        assert!(line(&f, Clause::Landing).starts_with("met landing"));
    }

    #[test]
    fn deploy_needs_a_non_empty_after_landing() {
        assert_eq!(line(&green(), Clause::Deploy), "met deploy: after_landing: curl the health url");
        let mut f = green();
        f.files.insert(DEPLOY.into(), "after_landing = \"  \"\n".into());
        assert!(line(&f, Clause::Deploy).starts_with("not met deploy: .spira/deploy.toml has no non-empty"));
        f.files.remove(DEPLOY);
        assert_eq!(line(&f, Clause::Deploy), "not met deploy: .spira/deploy.toml does not exist");
    }

    #[test]
    fn a_red_clause_does_not_stop_the_later_ones_and_the_exit_is_one() {
        let mut f = green();
        f.files.clear();
        f.workflow_files.clear();
        let mut lines = Vec::new();
        let code = run(&f, "r", None, &ALL, &mut |l| lines.push(l.to_string()));
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l.starts_with("not met ")));
        assert_eq!(code, 1);
        let mut lines = Vec::new();
        assert_eq!(run(&green(), "r", Some(&PathBuf::from("t")), &ALL, &mut |l| lines.push(l.to_string())), 0);
        assert!(lines.iter().all(|l| l.starts_with("met ")));
    }

    #[test]
    fn a_port_failure_is_cannot_tell_and_exits_two() {
        struct Broken(Fake);
        impl Ports for Broken {
            fn read(&self, _: &str, _: Option<&Path>, _: &str) -> Result<Option<String>, String> {
                Err("git unreadable".into())
            }
            fn shell_env(&self, r: &str, t: Option<&Path>) -> Result<Option<ShellEnv>, String> { self.0.shell_env(r, t) }
            fn on_path(&self, e: &ShellEnv, p: &str) -> bool { self.0.on_path(e, p) }
            fn run_checks(&self, r: &str, t: Option<&Path>) -> Result<Rows, String> { self.0.run_checks(r, t) }
            fn run_tiers(&self, r: &str, s: &[String]) -> Result<Vec<Tier>, String> { self.0.run_tiers(r, s) }
            fn landing_pieces(&self, r: &str) -> Vec<Piece> { self.0.landing_pieces(r) }
            fn land_mode(&self, r: &str) -> String { self.0.land_mode(r) }
            fn workflow(&self, r: &str) -> Result<String, String> { self.0.workflow(r) }
            fn workflow_files(&self, r: &str) -> Result<Vec<String>, String> { self.0.workflow_files(r) }
            fn product_repos(&self) -> Vec<String> { self.0.product_repos() }
            fn registered(&self, r: &str) -> bool { self.0.registered(r) }
        }
        let b = Broken(green());
        let mut lines = Vec::new();
        let code = run(&b, "r", None, &[Clause::Deploy, Clause::Landing], &mut |l| lines.push(l.to_string()));
        assert_eq!(lines[0], "cannot tell deploy: git unreadable");
        assert!(lines[1].starts_with("met landing"));
        assert_eq!(code, 2);
    }

    #[test]
    fn all_omits_the_home_repo() {
        let all = vec!["spira".to_string(), "a".into(), "b".into()];
        assert_eq!(without_home(all, "spira"), vec!["a", "b"]);
    }
}
