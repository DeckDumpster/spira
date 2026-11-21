//! The submit-time fence on a repository's landing contract files.

/// The files whose edit can break the contract; a repository's product branches meet no gate but
/// `work submit`, so this is where an edit to one is judged.
pub const CONTRACT_PATHS: [&str; 5] = ["flake.nix", "flake.lock", ".spira/checks.toml", ".spira/testenv.toml", ".spira/deploy.toml"];

/// The clauses that read the branch's own tree; only landing, which reads registry config and
/// the landing ref, is not judged at submit.
pub const FENCE_CLAUSES: &str = "environment,submit-checks,test-tiers,deploy";

pub fn touches_contract(changed: &[String]) -> bool {
    changed.iter().any(|p| CONTRACT_PATHS.contains(&p.as_str()))
}

/// The refusal for `repo-contract`'s exit and output: its `not met` lines on exit 1; nothing
/// otherwise (a checker that cannot tell is reported, not enforced).
pub fn refusal(code: Option<i32>, stdout: &str) -> Option<String> {
    if code != Some(1) {
        return None;
    }
    let lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with("not met ")).collect();
    Some(if lines.is_empty() { "repo-contract reports a clause not met".to_string() } else { lines.join("\n") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(thx62md)]
    #[test]
    fn the_fence_judges_the_four_clauses_that_read_the_tree() {
        use repo_contract::clause::Clause;
        let got: Vec<Clause> = FENCE_CLAUSES.split(',').map(|c| Clause::parse(c).unwrap()).collect();
        assert_eq!(got, [Clause::Environment, Clause::SubmitChecks, Clause::TestTiers, Clause::Deploy]);
    }

    #[test_id(t6wehmo)]
    #[test]
    fn touches_contract_covers_the_five_paths_and_nothing_else() {
        for p in CONTRACT_PATHS {
            assert!(touches_contract(&paths(&["src/a.rs", p])), "{p}");
        }
        assert_eq!(CONTRACT_PATHS.len(), 5);
        assert!(!touches_contract(&paths(&["src/flake.nix", ".spira/habits.toml", "flake.nix.bak", ".spira/units", "README.md"])));
        assert!(!touches_contract(&[]));
    }

    #[test_id(ta7y3jb)]
    #[test]
    fn only_exit_one_refuses_and_it_names_the_not_met_lines() {
        let out = "met deploy: x\nnot met environment: check \"lint\" runs \"uv\", which the flake's dev shell does not provide\n";
        assert_eq!(refusal(Some(1), out).unwrap(), "not met environment: check \"lint\" runs \"uv\", which the flake's dev shell does not provide");
        assert_eq!(refusal(Some(0), out), None);
        assert_eq!(refusal(Some(2), out), None);
        assert_eq!(refusal(None, out), None);
    }
}
