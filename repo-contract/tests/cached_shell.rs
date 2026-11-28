//! `repo-contract check` on a landing ref, through the binary: a realised dev shell is answered
//! from the cache with no worktree and no nix.

#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;

const FLAKE: &str = "{ outputs = _: { devShells = {}; }; }\n";
const LOCK: &str = "{}\n";

struct Fx {
    dir: testkit::TempDir,
    toml: PathBuf,
}

/// A repository carrying a flake on its landing ref, and a run directory whose worktree root
/// cannot be written, so a check that cuts a worktree cannot answer.
fn fixture() -> Fx {
    use std::os::unix::fs::PermissionsExt;
    let dir = testkit::TempDir::new("repo-contract-cached-shell");
    let repo = dir.path().join("fixture");
    std::fs::create_dir_all(repo.join(".spira")).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "local/main"]);
    std::fs::write(repo.join("flake.nix"), FLAKE).unwrap();
    std::fs::write(repo.join("flake.lock"), LOCK).unwrap();
    std::fs::write(repo.join(".spira/checks.toml"), "[[check]]\nname = \"unit\"\nrun = \"mytool test\"\nbudget_ms = 1000\n").unwrap();
    testkit::git(&repo, &["add", "-A"]);
    testkit::git(&repo, &["commit", "-q", "-m", "c"]);
    let run = dir.path().join("run");
    std::fs::create_dir_all(run.join("worktree")).unwrap();
    std::fs::set_permissions(run.join("worktree"), std::fs::Permissions::from_mode(0o555)).unwrap();
    let map = dir.path().join("fixture.repos");
    std::fs::write(&map, format!("fixture | {} | queue.local | local/main |  |  | plan\n", repo.display())).unwrap();
    let no_nix = dir.path().join("no-nix");
    let toml = spira_config::process::fixture_toml(
        dir.path(),
        &[("SPIRA_REPO_MAP", map.to_str().unwrap()), ("SPIRA_RUN", run.to_str().unwrap()), ("SPIRA_NIX_PROFILE_BIN", no_nix.to_str().unwrap())],
    );
    Fx { dir, toml }
}

impl Fx {
    fn prime_cache(&self) -> String {
        let tools = self.dir.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        testkit::write_exe(tools.join("mytool"), "#!/bin/sh\n");
        let key = repo_contract::shell::cache_key(FLAKE.as_bytes(), LOCK.as_bytes());
        let cache = self.dir.path().join("run/repo-contract/shell");
        std::fs::create_dir_all(&cache).unwrap();
        let env = format!(r#"{{"variables":{{"PATH":{{"type":"exported","value":"{}"}}}}}}"#, tools.display());
        std::fs::write(cache.join(format!("{key}.json")), env).unwrap();
        key
    }

    /// `repo-contract check fixture <flags> --clauses environment`: its exit and output.
    fn check(&self, flags: &[&str]) -> (Option<i32>, String) {
        let out = spira_config::bounded::bounded_for(testkit::bin_exe("repo-contract"), "60")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", spira_config::resolve::locate_home_for_process().unwrap())
            .args(["check", "fixture"])
            .args(flags)
            .args(["--clauses", "environment"])
            .output()
            .unwrap();
        (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

#[test_id(ta4q27h)]
#[test]
fn a_realised_dev_shell_on_the_landing_ref_is_judged_without_cutting_a_worktree() {
    let f = fixture();
    let key = f.prime_cache();
    for flags in [&[][..], &["--cached-shell"][..]] {
        let (code, text) = f.check(flags);
        assert!(text.contains(&format!("met environment: flake dev shell {key} ")), "{flags:?}: {text}");
        assert_eq!(code, Some(0), "{flags:?}: {text}");
    }
}

#[test_id(tgkzxq0)]
#[test]
fn a_cached_shell_check_of_an_unrealised_shell_says_so_and_runs_nothing() {
    let f = fixture();
    let key = repo_contract::shell::cache_key(FLAKE.as_bytes(), LOCK.as_bytes());
    let (code, text) = f.check(&["--cached-shell"]);
    assert!(
        text.contains(&format!("cannot tell environment: the dev shell for flake {key} is not realised; repo-contract check fixture realises it")),
        "{text}"
    );
    assert!(!text.contains("worktree") && !text.contains("no-nix"), "{text}");
    assert_eq!(code, Some(2), "{text}");
}

#[test_id(tlru29u)]
#[test]
fn a_full_check_of_an_unrealised_shell_still_realises_it_through_nix() {
    let f = fixture();
    let root = f.dir.path().join("run/worktree");
    std::fs::remove_dir(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    let (code, text) = f.check(&[]);
    let nix = f.dir.path().join("no-nix/nix");
    assert!(text.contains(&format!("cannot run {}", nix.display())), "{text}");
    assert_eq!(code, Some(2), "{text}");
}
