//! Environment bootstrap: the PATH tail and `SPIRA_BD` resolution `conf.sh` evals each time it
//! is sourced. It calls `env-bootstrap --sh` in `main.rs` and carries none of this logic itself.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk, law). `current` is the
/// PATH already in force — the launcher put the release's `bin/` and `spira/` at the front —
/// and this function only ever APPENDS a tail: `spira_path` (`SPIRA_PATH`, colon-separated,
/// the box's own setting), then `<home>/.local/bin`, `<home>/.cargo/bin` (cargo, for the
/// gate/testenv that build a tree under test — it holds no Spira tool), then the system
/// directories, each segment added at most once. Mirrors the bash loop's own
/// `case ":${PATH:-}:" in *":$seg:"*)` test, which checks against the PATH assembled SO FAR
/// on every iteration, not only against the original `current` — so a segment named twice in
/// `spira_path` itself, or one that collides with `<home>/.cargo/bin`, is still added once.
/// Calling this again (conf.sh re-sourced in the same shell) is idempotent: nothing already
/// present is re-added, so PATH never grows without bound.
pub fn path_tail(current: &str, spira_path: &str, home: &str) -> String {
    let mut path = current.to_string();
    let mut segs: Vec<String> = spira_path
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    segs.push(format!("{home}/.local/bin"));
    segs.push(format!("{home}/.cargo/bin"));
    segs.push(system_paths::USR_LOCAL_BIN.to_string());
    segs.push(system_paths::USR_BIN.to_string());
    segs.push(system_paths::BIN.to_string());
    for seg in segs {
        let needle = format!(":{seg}:");
        let probe = format!(":{path}:");
        if !probe.contains(&needle) {
            path = if path.is_empty() { seg } else { format!("{path}:{seg}") };
        }
    }
    path
}

/// `SPIRA_BD` — resolved once, deterministically, from `path` (the PATH [`path_tail`] just
/// assembled, never whatever PATH the calling context happened to carry in). sp-s2zvn's
/// scar: a bare `${SPIRA_BD:-bd}` fallback in lib.sh picked a different binary depending on
/// whether the caller was a login shell, a systemd unit or a dwarf's confined environment —
/// all three disagree about PATH order. Mirrors `command -v bd`'s own search: the first `bd`
/// on `path` that is a regular file with any execute bit set; an empty PATH segment means
/// the current directory, exactly as the shell resolves one. Falls back to the bare name
/// `"bd"` when nothing is found, so invoking it later fails loudly (`bd: command not found`)
/// instead of silently running the wrong binary.
pub fn resolve_bd(path: &str) -> String {
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let candidate = Path::new(dir).join("bd");
        if is_executable_file(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "bd".to_string()
}

fn is_executable_file(p: &Path) -> bool {
    match fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

/// `env-bootstrap --sh`'s whole output (`main.rs`) — `conf.sh`'s own `eval` target for this
/// bead, the same `KEY='value'` shape `resolve --sh`/`--sh-all` already use. Both names are
/// `export`ed inline here (unlike `Resolved::to_sh_all`'s bare assignments) because `PATH`
/// and `SPIRA_BD` must reach THIS shell's own already-sourced code (and any further child it
/// spawns before reaching `conf.sh`'s own later, explicit export list) immediately, not only
/// once that later block runs.
///
/// `existing_bd`: the caller's/config's own prior value, if any (`conf.sh` passes
/// `${SPIRA_BD:-}` — the environment and the config file both outrank derivation, exactly as
/// bash's own `[ -z "${SPIRA_BD:-}" ]` guard did). A non-empty value survives untouched;
/// [`resolve_bd`] runs only when it is empty.
pub fn bootstrap_sh(current_path: &str, spira_path: &str, home: &str, existing_bd: &str) -> String {
    let new_path = path_tail(current_path, spira_path, home);
    let bd = if existing_bd.trim().is_empty() {
        resolve_bd(&new_path)
    } else {
        existing_bd.to_string()
    };
    format!(
        "PATH={}\nexport PATH\nSPIRA_BD={}\nexport SPIRA_BD\n",
        crate::shell_quote(&new_path),
        crate::shell_quote(&bd),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tjnfgso)]
    #[test]
    fn path_tail_appends_each_segment_once() {
        let got = path_tail("release/bin:release/spira", "opt/custom", "fixture-home");
        assert_eq!(
            got,
            "release/bin:release/spira:opt/custom:fixture-home/.local/bin:fixture-home/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[test_id(t54cxxa)]
    #[test]
    fn path_tail_never_shadows_the_launchers_release() {
        // THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk): a tail segment
        // that already appears at the front (here, SPIRA_PATH naming /usr/bin, which is also
        // a hardcoded tail entry) must not be duplicated or reordered.
        let got = path_tail("release/bin", system_paths::USR_BIN, "fixture-home");
        assert_eq!(got, "release/bin:/usr/bin:fixture-home/.local/bin:fixture-home/.cargo/bin:/usr/local/bin:/bin");
    }

    #[test_id(t22jxmf)]
    #[test]
    fn path_tail_is_idempotent_on_resource() {
        let once = path_tail("release/bin", "", "fixture-home");
        let twice = path_tail(&once, "", "fixture-home");
        assert_eq!(once, twice, "re-sourcing conf.sh must not grow PATH");
    }

    #[test_id(tcsz175)]
    #[test]
    fn path_tail_empty_spira_path_contributes_nothing() {
        let got = path_tail("release/bin", "", "fixture-home");
        assert_eq!(got, "release/bin:fixture-home/.local/bin:fixture-home/.cargo/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test_id(th1uq1c)]
    #[test]
    fn path_tail_collapses_doubled_colons_in_spira_path() {
        let got = path_tail("", "a::b", "h");
        assert_eq!(got, "a:b:h/.local/bin:h/.cargo/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test_id(tr0zoz6)]
    #[test]
    fn resolve_bd_skips_a_non_executable_candidate() {
        let dir = testkit::TempDir::new("spira-config-env-bootstrap-resolve-bd-noexec");
        fs::write(dir.path().join("bd"), "not executable").unwrap();
        let path = format!("{}:/usr/bin", dir.path().display());
        // /usr/bin/bd does not exist in this test environment either, so the fallback fires.
        assert_eq!(resolve_bd(&path), "bd");
    }

    #[test_id(tb5vq43)]
    #[test]
    fn resolve_bd_falls_back_to_bare_name_when_nothing_found() {
        assert_eq!(resolve_bd("nonexistent/one:nonexistent/two"), "bd");
    }

    #[test_id(tg7wjpo)]
    #[test]
    fn bootstrap_sh_leaves_an_existing_spira_bd_untouched() {
        let out = bootstrap_sh("release/bin", "", "fixture-home", "pinned/bd");
        assert!(out.contains("SPIRA_BD='pinned/bd'"), "{out}");
    }
}
