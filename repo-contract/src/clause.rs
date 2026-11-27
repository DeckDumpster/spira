//! The five clauses of a repository's landing contract, each a pure function of the ports.
//! The check is read-only: it judges files and refs and runs no suite.

use crate::ports::Ports;
use crate::shell::DevShell;
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
        Clause::TestTiers => test_tiers(p, repo, tree),
        Clause::Landing => landing(p, repo),
        Clause::Deploy => deploy(p, repo, tree),
    };
    r.unwrap_or_else(Outcome::CannotTell)
}

type Judged = Result<Outcome, String>;

fn row_program(run: &str) -> Option<String> {
    habits::config::argv(run, &[]).into_iter().next()
}

fn environment(p: &dyn Ports, repo: &str, tree: Option<&Path>) -> Judged {
    for f in ["flake.nix", "flake.lock"] {
        if p.read(repo, tree, f)?.is_none() {
            return Ok(Outcome::NotMet(format!("{f} is not in the tree")));
        }
    }
    let env = match p.shell_env(repo, tree)? {
        DevShell::Entered(e) => e,
        DevShell::Undeclared => return Ok(Outcome::NotMet("flake.nix declares no devShells".into())),
        DevShell::DoesNotEnter(e) => return Ok(Outcome::NotMet(format!("the flake's dev shell does not enter: {e}"))),
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
    let rows = match habits::config::parse_checks(&text) {
        Err(e) => return Ok(Outcome::NotMet(format!("{CHECKS} does not parse: {e}"))),
        Ok(rows) if rows.is_empty() => return Ok(Outcome::Met(format!("{CHECKS} declares no [[check]] rows"))),
        Ok(rows) => rows,
    };
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    Ok(Outcome::Met(format!("{CHECKS} declares {} row(s): {}", names.len(), names.join(", "))))
}

fn test_tiers(p: &dyn Ports, repo: &str, tree: Option<&Path>) -> Judged {
    let Some(text) = p.read(repo, tree, TESTENV)? else {
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
    Ok(Outcome::Met(format!("{TESTENV} declares default tiers: {}", default.join(","))))
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
        let found = files.iter().any(|f| f.eq_ignore_ascii_case(&name) || f.strip_suffix(".yml").or_else(|| f.strip_suffix(".yaml")).is_some_and(|stem| stem.eq_ignore_ascii_case(&name)));
        if !found {
            return Ok(Outcome::NotMet(format!("the forge workflow {name:?} is not a file under {WORKFLOWS}/ on the landing ref")));
        }
        return Ok(Outcome::Met(format!("{} registration pieces ok; forge workflow {name:?} is under {WORKFLOWS}/", pieces.len())));
    }
    if mode == "pr" {
        if let Some(gate) = pieces.iter().find(|x| x.name == "gate") {
            return Ok(Outcome::Met(format!("gate: {}", gate.detail)));
        }
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

/// Read-only: runs no suite.
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
    use crate::ports::Piece;
    use crate::shell::{DevShell, ShellEnv};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct Fake {
        files: BTreeMap<String, String>,
        shell: Option<Vec<&'static str>>,
        shell_fault: Option<Result<String, String>>,
        bad_piece: Option<(&'static str, &'static str)>,
        gate_piece: Option<&'static str>,
        mode: &'static str,
        workflow: &'static str,
        workflow_files: Vec<String>,
        repos: Vec<String>,
        tree_files: Option<BTreeMap<String, String>>,
    }

    impl Ports for Fake {
        fn read(&self, _: &str, tree: Option<&Path>, rel: &str) -> Result<Option<String>, String> {
            let files = match (&self.tree_files, tree) {
                (Some(t), Some(_)) => t,
                _ => &self.files,
            };
            Ok(files.get(rel).cloned())
        }
        fn shell_env(&self, _: &str, _: Option<&Path>) -> Result<DevShell, String> {
            match &self.shell_fault {
                Some(Err(e)) => return Err(e.clone()),
                Some(Ok(e)) => return Ok(DevShell::DoesNotEnter(e.clone())),
                None => {}
            }
            Ok(match self.shell.as_ref() {
                Some(progs) => DevShell::Entered(ShellEnv { vars: progs.iter().map(|p| (format!("has:{p}"), String::new())).collect(), key: "k1".into() }),
                None => DevShell::Undeclared,
            })
        }
        fn on_path(&self, env: &ShellEnv, program: &str) -> bool {
            env.vars.contains_key(&format!("has:{program}"))
        }
        fn landing_pieces(&self, _: &str) -> Vec<Piece> {
            let mut v = vec![Piece { name: "registered".into(), detail: "in the repo registry".into(), ok: true }];
            if let Some(d) = self.gate_piece {
                v.push(Piece { name: "gate".into(), detail: d.into(), ok: true });
            }
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
        let mut f = Fake { shell: Some(vec!["cargo", "bash"]), mode: "queue", workflow: "Gate", workflow_files: vec!["Gate.yml".into()], ..Fake::default() };
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

    #[test_id(tx3mft5)]
    #[test]
    fn environment_met_names_the_shell_and_the_image() {
        assert_eq!(line(&green(), Clause::Environment), "met environment: flake dev shell k1 provides lint=cargo; unit tier runs in image img:1");
    }

    #[test_id(tk6233n)]
    #[test]
    fn environment_not_met_names_the_row_and_program_the_shell_lacks() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "[[check]]\nname = \"lint\"\nrun = \"uv run ruff\"\nbudget_ms = 1000\n".into());
        assert_eq!(line(&f, Clause::Environment), "not met environment: check \"lint\" runs \"uv\", which the flake's dev shell does not provide");
    }

    #[test_id(t2k02e8)]
    #[test]
    fn environment_not_met_without_a_flake_lock_or_a_dev_shell_or_when_it_will_not_enter() {
        let mut f = green();
        f.files.remove("flake.lock");
        assert_eq!(line(&f, Clause::Environment), "not met environment: flake.lock is not in the tree");
        let mut f = green();
        f.shell = None;
        assert!(line(&f, Clause::Environment).contains("declares no devShells"));
        let mut f = green();
        f.shell_fault = Some(Ok("boom".into()));
        assert_eq!(line(&f, Clause::Environment), "not met environment: the flake's dev shell does not enter: boom");
    }

    #[test_id(thfxoyq)]
    #[test]
    fn a_checker_fault_entering_the_shell_is_cannot_tell_and_exits_two() {
        let mut f = green();
        f.shell_fault = Some(Err("cannot cut a worktree from local/main: boom".into()));
        assert_eq!(line(&f, Clause::Environment), "cannot tell environment: cannot cut a worktree from local/main: boom");
        assert_eq!(run(&f, "r", None, &[Clause::Environment], &mut |_| {}), 2);
    }

    #[test_id(tt5gxfa)]
    #[test]
    fn a_built_row_is_not_judged_against_the_shell() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "[[check]]\nname = \"g\"\nrun = \"gate fence\"\nbudget_ms = 1\nbuild = \"gate\"\n".into());
        assert!(line(&f, Clause::Environment).starts_with("met environment"));
    }

    #[test_id(t8kx4ts)]
    #[test]
    fn submit_checks_met_carries_each_rows_line_and_not_met_when_absent_or_unparsable() {
        assert_eq!(line(&green(), Clause::SubmitChecks), "met submit-checks: .spira/checks.toml declares 1 row(s): lint");
        let mut f = green();
        f.files.remove(CHECKS);
        assert_eq!(line(&f, Clause::SubmitChecks), "not met submit-checks: .spira/checks.toml does not exist");
        f.files.insert(CHECKS.into(), "not [ toml".into());
        assert!(line(&f, Clause::SubmitChecks).starts_with("not met submit-checks: .spira/checks.toml does not parse"));
    }

    #[test_id(tvb6kme)]
    #[test]
    fn submit_checks_met_when_the_file_declares_no_rows() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "# Rows `work checks` runs before a branch is submitted.\n".into());
        assert_eq!(line(&f, Clause::SubmitChecks), "met submit-checks: .spira/checks.toml declares no [[check]] rows");
    }

    #[test_id(tn7fxfz)]
    #[test]
    fn a_checks_file_with_no_rows_leaves_the_check_exit_zero() {
        let mut f = green();
        f.files.insert(CHECKS.into(), "# no rows\n".into());
        let mut out = Vec::new();
        let code = run(&f, "r", None, &ALL, &mut |l| out.push(l.to_string()));
        assert_eq!(code, 0, "{out:?}");
        assert!(!out.iter().any(|l| l.starts_with("not met")), "{out:?}");
    }

    #[test_id(tx2u8qt)]
    #[test]
    fn test_tiers_name_the_declared_default_and_need_one() {
        assert_eq!(line(&green(), Clause::TestTiers), "met test-tiers: .spira/testenv.toml declares default tiers: unit,lint");
        let mut f = green();
        f.files.insert(TESTENV.into(), "image = \"x\"\n".into());
        assert_eq!(line(&f, Clause::TestTiers), "not met test-tiers: .spira/testenv.toml declares no `default` tiers");
        f.files.remove(TESTENV);
        assert_eq!(line(&f, Clause::TestTiers), "not met test-tiers: .spira/testenv.toml does not exist");
    }

    #[test_id(tbxz75i)]
    #[test]
    fn test_tiers_reads_the_tree_when_given_one() {
        let mut f = green();
        f.files.insert(TESTENV.into(), "default = [\"unit\"]\n".into());
        f.tree_files = Some(BTreeMap::new());
        let mut out = Vec::new();
        let tree = PathBuf::from("tree");
        run(&f, "r", Some(&tree), &[Clause::TestTiers], &mut |l| out.push(l.to_string()));
        assert_eq!(out, ["not met test-tiers: .spira/testenv.toml does not exist"]);
    }

    #[test_id(tir8pbw)]
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

    #[test_id(tk860rx)]
    #[test]
    fn landing_for_a_pr_repository_reports_what_judges_it() {
        let mut f = green();
        f.mode = "pr";
        f.gate_piece = Some("certifies nothing; the pull request's CI judges");
        assert_eq!(line(&f, Clause::Landing), "met landing: gate: certifies nothing; the pull request's CI judges");
    }

    #[test_id(tk9j71x)]
    #[test]
    fn landing_matches_the_workflow_name_to_the_file_stem_ignoring_case() {
        let mut f = green();
        f.workflow = "CI";
        f.workflow_files = vec!["ci.yml".into()];
        assert!(line(&f, Clause::Landing).starts_with("met landing"));
        f.workflow_files = vec!["ci.yaml".into()];
        assert!(line(&f, Clause::Landing).starts_with("met landing"));
        f.workflow_files = vec!["other.yml".into()];
        assert!(line(&f, Clause::Landing).starts_with("not met landing"));
    }

    #[test_id(ts4sf39)]
    #[test]
    fn deploy_needs_a_non_empty_after_landing() {
        assert_eq!(line(&green(), Clause::Deploy), "met deploy: after_landing: curl the health url");
        let mut f = green();
        f.files.insert(DEPLOY.into(), "after_landing = \"  \"\n".into());
        assert!(line(&f, Clause::Deploy).starts_with("not met deploy: .spira/deploy.toml has no non-empty"));
        f.files.remove(DEPLOY);
        assert_eq!(line(&f, Clause::Deploy), "not met deploy: .spira/deploy.toml does not exist");
    }

    #[test_id(tab7brv)]
    #[test]
    fn all_clauses_judge_tiers_and_rows_from_the_files_alone() {
        let mut lines = Vec::new();
        run(&green(), "r", None, &ALL, &mut |l| lines.push(l.to_string()));
        assert!(lines.contains(&"met test-tiers: .spira/testenv.toml declares default tiers: unit,lint".to_string()));
        assert!(lines.contains(&"met submit-checks: .spira/checks.toml declares 1 row(s): lint".to_string()));
    }

    #[test_id(t0akkea)]
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

    #[test_id(twyvhih)]
    #[test]
    fn a_port_failure_is_cannot_tell_and_exits_two() {
        struct Broken(Fake);
        impl Ports for Broken {
            fn read(&self, _: &str, _: Option<&Path>, _: &str) -> Result<Option<String>, String> {
                Err("git unreadable".into())
            }
            fn shell_env(&self, r: &str, t: Option<&Path>) -> Result<DevShell, String> { self.0.shell_env(r, t) }
            fn on_path(&self, e: &ShellEnv, p: &str) -> bool { self.0.on_path(e, p) }
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

    #[test_id(tj7icpb)]
    #[test]
    fn all_omits_the_home_repo() {
        let all = vec!["spira".to_string(), "a".into(), "b".into()];
        assert_eq!(without_home(all, "spira"), vec!["a", "b"]);
    }
}
