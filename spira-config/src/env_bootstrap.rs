//! Environment bootstrap: the PATH tail `conf.sh` evals each time it is sourced. It calls `env-bootstrap --sh` in `main.rs` and carries none of this logic itself.

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

/// `env-bootstrap --sh`'s whole output (`main.rs`), `conf.sh`'s own `eval` target. PATH is
/// `export`ed inline because it must reach this shell's already-sourced code at once.
pub fn bootstrap_sh(current_path: &str, spira_path: &str, home: &str) -> String {
    let new_path = path_tail(current_path, spira_path, home);
    format!("PATH={}\nexport PATH\n", crate::shell_quote(&new_path))
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

    #[test]
    fn bootstrap_sh_exports_only_the_path() {
        let out = bootstrap_sh("release/bin", "", "fixture-home");
        assert!(out.starts_with("PATH='release/bin:fixture-home/.local/bin:"), "{out}");
        assert!(out.ends_with("export PATH\n") && out.lines().count() == 2, "{out}");
    }
}
