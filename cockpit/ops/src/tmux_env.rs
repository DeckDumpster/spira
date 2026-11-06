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

/// Removes [`VARS`] from a live server's global environment and pins `spira_toml` there when
/// the server lacks it; the lines the script printed. A server that is not running has nothing
/// to scrub, and that is not an error. Silent when there was nothing to do.
pub fn scrub(tmux: &Tmux, spira_toml: Option<&str>) -> Vec<String> {
    let mut said = Vec::new();
    if !tmux.ok(&["has-session"]) {
        return said;
    }
    if let Some(toml) = spira_toml.filter(|t| !t.is_empty() && std::path::Path::new(t).is_file()) {
        if !tmux.ok(&["show-environment", "-g", "SPIRA_TOML"]) && tmux.ok(&["set-environment", "-g", "SPIRA_TOML", toml]) {
            said.push("cockpit: pinned SPIRA_TOML in the tmux server environment".to_string());
        }
    }
    let mut n = 0;
    for v in VARS {
        if tmux.ok(&["show-environment", "-g", v]) && tmux.ok(&["set-environment", "-gu", v]) {
            n += 1;
        }
    }
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

    /// A tmux that logs every call and has `CLAUDECODE` and `CLAUDE_PID` set, no SPIRA_TOML,
    /// and a server only when `up`.
    fn fake(dir: &std::path::Path, up: bool) -> Tmux {
        let bin = dir.join("tmux");
        let log = dir.join("calls");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1 $3\" in\n  'has-session '*) {} ;;\n  'show-environment CLAUDECODE'|'show-environment CLAUDE_PID') exit 0 ;;\n  'show-environment '*) exit 1 ;;\n  *) exit 0 ;;\nesac\n",
            log.display(),
            if up { "exit 0" } else { "exit 1" }
        );
        testkit::write_exe(&bin, &script);
        Tmux::with_bin(bin.to_string_lossy().into_owned())
    }

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
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(calls.contains(&format!("set-environment -g SPIRA_TOML {}", spec.display())), "{calls}");
        assert!(calls.contains("set-environment -gu CLAUDECODE"), "{calls}");
        assert!(calls.contains("set-environment -gu CLAUDE_PID"), "{calls}");
        assert!(!calls.contains("set-environment -gu AI_AGENT"), "{calls}");
    }

    #[test]
    fn no_server_is_nothing_to_scrub() {
        let dir = testkit::TempDir::new("tmux-env-down");
        assert!(scrub(&fake(&dir, false), None).is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("calls")).unwrap().trim(), "has-session");
    }
}
