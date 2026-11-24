//! `tmux-env.sh scrub`, in process: keep one Claude session's identity out of the tmux
//! server's environment. tmux gives every new pane THE SERVER's environment, frozen when the
//! server was forked, so a server started from inside a session hands that session's identity
//! (`CLAUDE_CODE_CHILD_SESSION` and the rest) to every pane it will ever open. Absent is the
//! only right value for any of them in a shared server. The script stays for its `names` verb
//! and its other callers; this is its `scrub`, with tmux the only child.

use crate::tmux::Tmux;

/// Session identity, in the order the client sets them (tmux-env.sh's own `VARS`).
pub const VARS: &[&str] = &[
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDECODE",
    "AI_AGENT",
];

/// [`scrub_from`] over one `show-environment -g` read. A server that is not running answers
/// non-zero and has nothing to scrub, and that is not an error.
pub fn scrub(tmux: &Tmux, spira_toml: Option<&str>) -> Vec<String> {
    match tmux.stdout(&["show-environment", "-g"]) {
        Some(env) => scrub_from(tmux, &env, spira_toml),
        None => Vec::new(),
    }
}

/// Removes from the server's global environment each of [`VARS`] that `env` (its
/// `show-environment -g` answer) holds, and pins `spira_toml` there when `env` lacks it; the
/// lines the script printed. Writes only what `env` shows needs it, and never says a value.
pub fn scrub_from(tmux: &Tmux, env: &str, spira_toml: Option<&str>) -> Vec<String> {
    let mut said = Vec::new();
    let held = |name: &str| env.lines().any(|l| l.strip_prefix(name).is_some_and(|rest| rest.starts_with('=')));
    if let Some(toml) = spira_toml.filter(|t| !t.is_empty() && std::path::Path::new(t).is_file()) {
        if !held("SPIRA_TOML") && tmux.ok(&["set-environment", "-g", "SPIRA_TOML", toml]) {
            said.push("cockpit: pinned SPIRA_TOML in the tmux server environment".to_string());
        }
    }
    let n = VARS.iter().filter(|v| held(v) && tmux.ok(&["set-environment", "-gu", v])).count();
    if n > 0 {
        said.push(format!("cockpit: scrubbed {n} inherited Claude session variable(s) from the tmux server"));
    }
    said
}

/// [`scrub`] with this process's own `$SPIRA_TOML`, as the script read it.
pub fn scrub_here(tmux: &Tmux) -> Vec<String> {
    scrub(tmux, std::env::var(spira_config::process::TOML_ENV).ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tmux that logs every call, answers `show-environment -g` with `CLAUDECODE` and
    /// `CLAUDE_PID` set (and an unset marker for `AI_AGENT`) and no SPIRA_TOML, and has a
    /// server only when `up`.
    fn fake(dir: &std::path::Path, up: bool) -> Tmux {
        let bin = dir.join("tmux");
        let log = dir.join("calls");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nif [ {} = down ]; then exit 1; fi\ncase \"$*\" in\n  'show-environment -g') printf 'CLAUDECODE=secret-a\\nCLAUDE_PID=secret-b\\n-AI_AGENT\\nPATH=/bin\\n' ;;\nesac\nexit 0\n",
            log.display(),
            if up { "up" } else { "down" }
        );
        testkit::write_exe(&bin, &script);
        Tmux::with_bin(bin.to_string_lossy().into_owned())
    }

    #[test_id(tbep2qg)]
    #[test]
    fn scrub_unsets_only_the_identity_vars_the_server_holds_and_pins_the_spec() {
        let dir = testkit::TempDir::new("tmux-env-scrub");
        let spec = dir.join("config-spec");
        std::fs::write(&spec, "").unwrap();
        let said = scrub(&fake(&dir, true), Some(spec.to_str().unwrap()));
        assert_eq!(
            said,
            vec![
                "cockpit: pinned SPIRA_TOML in the tmux server environment".to_string(),
                "cockpit: scrubbed 2 inherited Claude session variable(s) from the tmux server".to_string(),
            ]
        );
        let calls: Vec<String> = std::fs::read_to_string(dir.join("calls")).unwrap().lines().map(str::to_string).collect();
        assert_eq!(
            calls,
            [
                "show-environment -g".to_string(),
                format!("set-environment -g SPIRA_TOML {}", spec.display()),
                "set-environment -gu CLAUDE_PID".to_string(),
                "set-environment -gu CLAUDECODE".to_string(),
            ],
            "one read, then a write for each name present and no other"
        );
        assert!(said.iter().all(|l| !l.contains("secret")), "a value is never said");
    }

    #[test_id(ts55a0g)]
    #[test]
    fn a_server_that_already_holds_the_pin_and_no_identity_gets_no_write() {
        let dir = testkit::TempDir::new("tmux-env-clean");
        let spec = dir.join("config-spec");
        std::fs::write(&spec, "").unwrap();
        let tmux = fake(&dir, true);
        let env = format!("SPIRA_TOML={}\n-CLAUDECODE\nPATH=/bin\n", spec.display());
        assert!(scrub_from(&tmux, &env, Some(spec.to_str().unwrap())).is_empty());
        assert!(!dir.join("calls").exists(), "nothing needed writing, so tmux was not run");
    }

    #[test_id(tbdmxdl)]
    #[test]
    fn no_server_is_nothing_to_scrub() {
        let dir = testkit::TempDir::new("tmux-env-down");
        assert!(scrub(&fake(&dir, false), None).is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("calls")).unwrap().trim(), "show-environment -g");
    }
}
