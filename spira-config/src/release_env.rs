//! `law-a-binary-resolves-the-config-it-reads`, for the one shape every release binary that
//! shells into `conf.sh`/`lib.sh` shares: a Rust process spawns `bash -c '. "$HERE/conf.sh"
//! ...'` (or `lib.sh`) and lets the CHILD inherit whatever PATH the PARENT happened to have —
//! which is the release's own `bin/`+`spira/` only when a launcher (a systemd unit, `conf.sh`
//! itself) put it there first. A binary reached by bare name from a plain shell (`env -i
//! HOME=$HOME PATH=/usr/bin:/bin $RELEASE/bin/inbox-triage` — exactly how the SessionStart
//! compact hook arms the Concierge's inbox Monitor) has no such launcher: its own PATH is
//! bare, the child's is too, `command -v spira-config` in `conf.sh` fails, and the whole
//! process dies with exit 97 before it does anything (sp-kgzql).
//!
//! THE FIX: the PARENT, not `conf.sh`, is the one thing that can answer "where is my own
//! release" — `conf.sh` has no `BASH_SOURCE` standing for a compiled binary's own location,
//! and has no business guessing which binary launched it anyway. So every one of these
//! spawn sites resolves ITS OWN release root once, here, the same ascending search
//! `inbox-triage`/`watchd`/... already ran to find `conf.sh`'s own directory (never
//! `$SPIRA_HOME`, which is the CALLER's own config input — a fixture's `--home` may point
//! at some OTHER tree's `conf.sh`/`lib.sh` entirely; this binary's PATH fix is about ITS OWN
//! release's tools being reachable, regardless of whose config ends up getting read), then
//! prepends `<release>/bin:<release>/spira` onto the child's PATH and sets `SPIRA_RELEASE`,
//! exactly the two things a normal launcher already does for the TOP-level process
//! ([`crate::release_path`]) — this module is that same contract, applied one process hop
//! further in.
//!
//! NEVER `std::env::current_exe()` alone: it canonicalizes every symlink in the path, which
//! is exactly wrong for a staged test release whose `bin/<tool>` is a symlink to a cargo
//! build artifact with no `spira/` sibling at all (`testenv::fixture::STAGE_SCRIPT`) — the
//! same hazard `install::bootstrap::argv0_path`'s own doc describes. [`own_release_root`]
//! walks from the EXE's path as given (via [`std::env::current_exe`], which still resolves
//! the FINAL symlink component but not ones a caller substitutes with `argv0`), ascending
//! parent directories rather than trusting any one fixed depth, so it finds the nearest
//! ancestor that actually holds `spira/conf.sh` — the `current` symlink's target (a release
//! sha directory) and a sha directory named directly both satisfy this the same way, and a
//! staged fixture whose `bin/` symlinks elsewhere falls through to whatever real tree is
//! above it, same as today's per-crate `home_dir`/`locate_home` helpers already do.

use std::path::{Path, PathBuf};

/// Ascends from `exe`'s own directory to the nearest ancestor holding a `spira/conf.sh` —
/// this binary's own release root (`<release>` such that `<release>/bin` holds `exe` and
/// `<release>/spira/conf.sh` exists). The identical search `inbox-triage::find_spira_dir`
/// and `watchd`'s `home_dir` already run for `conf.sh`'s own directory, generalized here so
/// the RELEASE ROOT (`spira`'s parent, which also holds `bin/`) is available too — until
/// this bead nothing computed that half, only `spira/`'s own location.
///
/// `None` when no ancestor qualifies (a bare `cargo test` binary under `target/debug` with
/// no `spira/` anywhere above it) — the caller degrades to its PRE-this-bead behaviour
/// (inherited PATH, unset `SPIRA_RELEASE`) rather than refusing outright, matching every
/// other per-crate home-finder's own "best effort" contract.
/// BOTH `<dir>/bin` (a directory) AND `<dir>/spira/conf.sh` (a file) must exist —
/// `spira/conf.sh` alone is not enough: this process's OWN dev checkout satisfies that
/// (every workspace crate's `target/debug/deps/<test-binary>` ascends straight into the
/// checkout's own `spira/conf.sh`) but has no top-level `bin/`, and treating it as a
/// release anyway shadows a test fixture's own deliberate PATH prepend with whatever the
/// checkout's real `spira/*.sh` happens to hold — caught live by
/// `queue::real::tests::pf_gate_returns_gates_output_and_rc_and_enforces_its_own_wall`,
/// which stubs `gate.sh` in a tempdir it puts FIRST on PATH specifically so the stub wins;
/// prepending the checkout's own `spira/gate.sh` ahead of it broke that contract. A real
/// release (`<release>/bin/<tool>` + `<release>/spira/conf.sh`, or any test fixture that
/// stages the same two directories — `release/tests/session_hook_minimal_env.rs`,
/// `inbox-triage/tests/release_path.rs`, `testenv::fixture::STAGE_SCRIPT`) always has both.
pub fn own_release_root(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent()?;
    loop {
        if dir.join("bin").is_dir() && dir.join("spira").join("conf.sh").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// [`own_release_root`] of `std::env::current_exe()` — what every real call site uses; kept
/// separate from the pure function so a test can hand in a fabricated `exe` without
/// controlling where the test binary itself happens to be installed.
pub fn own_release_root_for_process() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|e| own_release_root(&e))
}

/// `PATH`/`SPIRA_RELEASE` for a CHILD process this binary is about to spawn to source
/// `conf.sh`/`lib.sh`: `<release>/bin` and `<release>/spira` PREPENDED onto `existing_path`
/// (never discarding it — unlike the top-level launcher's [`crate::release_path`], which
/// sets PATH outright, a binary already running may have its own extra PATH entries in
/// force for reasons that have nothing to do with this fix, and a child still needing one of
/// those should degrade exactly as it would have before this bead, not lose it). `release`
/// absent (nothing above this exe looks like a release) returns nothing to set, so the
/// child's PATH passes through completely unmodified — the exact pre-this-bead behaviour,
/// never a refusal.
pub fn child_path_env(release: Option<&Path>, existing_path: Option<&str>) -> Vec<(String, String)> {
    let Some(release) = release else { return Vec::new() };
    let release_s = release.display().to_string();
    let path = match existing_path.filter(|p| !p.is_empty()) {
        Some(p) => format!("{release_s}/bin:{release_s}/spira:{p}"),
        None => format!("{release_s}/bin:{release_s}/spira"),
    };
    vec![(crate::RELEASE_ENV.to_string(), release_s), ("PATH".to_string(), path)]
}

/// [`child_path_env`] for THIS process: [`own_release_root_for_process`] and this process's
/// own `$PATH` — the one call every `conf.sh`/`lib.sh`-sourcing seam makes right before
/// spawning `bash`, e.g. `cmd.envs(spira_config::release_env::child_path_env_for_process())`.
pub fn child_path_env_for_process() -> Vec<(String, String)> {
    child_path_env(own_release_root_for_process().as_deref(), std::env::var("PATH").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_release_root_finds_spira_one_level_above_bin() {
        let t = testkit::TempDir::new("release-env-bin");
        let release = t.join("spira-releases/abc123");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::fs::create_dir_all(release.join("spira")).unwrap();
        std::fs::write(release.join("spira/conf.sh"), "").unwrap();
        let exe = release.join("bin/inbox-triage");
        assert_eq!(own_release_root(&exe), Some(release));
    }

    #[test]
    fn own_release_root_climbs_past_an_intermediate_directory() {
        // A staged testenv/aeon-profile release: target/aeon/<bin>, bin/+spira/ two levels
        // further up (testenv::fixture::STAGE_SCRIPT's own shape — `bin/` and `spira/`
        // both siblings of the checkout's `target/`, never inside it).
        let t = testkit::TempDir::new("release-env-climb");
        std::fs::create_dir_all(t.join("target/aeon")).unwrap();
        std::fs::create_dir_all(t.join("bin")).unwrap();
        std::fs::create_dir_all(t.join("spira")).unwrap();
        std::fs::write(t.join("spira/conf.sh"), "").unwrap();
        let exe = t.join("target/aeon/inbox-triage");
        assert_eq!(own_release_root(&exe), Some(t.path().to_path_buf()));
    }

    #[test]
    fn own_release_root_is_none_with_no_qualifying_ancestor() {
        let t = testkit::TempDir::new("release-env-none");
        let exe = t.join("a/b/c/inbox-triage");
        assert_eq!(own_release_root(&exe), None);
    }

    /// THE REGRESSION THIS BEAD'S FIRST VERSION CAUSED: a `cargo test` binary's own
    /// `current_exe()` ascends straight into the WORKSPACE CHECKOUT's `spira/conf.sh` —
    /// every crate's test binary sits a few `target/debug/deps/` levels under it — which
    /// is not a release at all (no top-level `bin/`), only a dev checkout. Treating it as
    /// one would prepend the checkout's own `spira/*.sh` onto PATH ahead of a test
    /// fixture's own deliberate stub directory, exactly the collision
    /// `queue::real::tests::pf_gate_returns_gates_output_and_rc_and_enforces_its_own_wall`
    /// hit (that test's tempdir `gate.sh` stub, prepended first on PATH, lost to the
    /// checkout's real `spira/gate.sh` once this function treated the checkout as the
    /// exe's own release). `spira/conf.sh` present with no sibling `bin/` must stay `None`.
    #[test]
    fn own_release_root_is_none_for_a_checkout_with_spira_but_no_bin() {
        let t = testkit::TempDir::new("release-env-checkout");
        std::fs::create_dir_all(t.join("target/debug/deps")).unwrap();
        std::fs::create_dir_all(t.join("spira")).unwrap();
        std::fs::write(t.join("spira/conf.sh"), "").unwrap();
        let exe = t.join("target/debug/deps/some-crate-abc123");
        assert_eq!(own_release_root(&exe), None);
    }

    #[test]
    fn child_path_env_prepends_bin_and_spira_and_keeps_the_existing_tail() {
        let release = Path::new("/opt/spira-releases/abc123");
        let got = child_path_env(Some(release), Some("/usr/bin:/bin"));
        let path = got.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, Some("/opt/spira-releases/abc123/bin:/opt/spira-releases/abc123/spira:/usr/bin:/bin"));
        let release_env = got.iter().find(|(k, _)| k == crate::RELEASE_ENV).map(|(_, v)| v.as_str());
        assert_eq!(release_env, Some("/opt/spira-releases/abc123"));
    }

    #[test]
    fn child_path_env_with_no_existing_path_still_sets_bin_and_spira() {
        let release = Path::new("/opt/spira-releases/abc123");
        let got = child_path_env(Some(release), None);
        let path = got.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, Some("/opt/spira-releases/abc123/bin:/opt/spira-releases/abc123/spira"));
    }

    #[test]
    fn child_path_env_is_empty_with_no_resolvable_release() {
        assert_eq!(child_path_env(None, Some("/usr/bin:/bin")), Vec::new());
    }

    #[test]
    fn child_path_env_reproduces_the_bare_shell_scar_without_a_release() {
        // The exact repro this bead names: `env -i HOME=$HOME PATH=/usr/bin:/bin
        // $RELEASE/bin/inbox-triage` — a bare shell with no release anywhere on PATH. Before
        // this bead's fix, nothing computed a release root at all, so a caller building a
        // child's PATH straight from `std::env::var("PATH")` got exactly `/usr/bin:/bin`
        // (no `spira-config` on it) — `command -v spira-config` in conf.sh fails and the
        // whole seam exits 97. With a resolvable release, the fix prepends it regardless of
        // how bare the parent's own PATH was.
        let release = Path::new("/opt/spira-releases/abc123");
        let bare = "/usr/bin:/bin";
        let got = child_path_env(Some(release), Some(bare));
        let path = got.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap();
        assert!(path.starts_with("/opt/spira-releases/abc123/bin:/opt/spira-releases/abc123/spira:"));
        assert!(path.ends_with(bare));
    }
}
