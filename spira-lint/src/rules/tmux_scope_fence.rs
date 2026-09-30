//! `tmux-scope-fence` — refuse a suite that can reach the default tmux socket.
//! Contract: DESIGN.md. Ported from `spira/tmux-scope-fence.sh` (deleted).
//!
//! A suite that calls a tmux session-affecting command with no socket scoping drives whatever
//! server the caller's environment already points at — the operator's own, twice (sp-pfca0).

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{direct_child, lines, Entry, Finding, LintError, Rule, Tree};

pub struct TmuxScopeFence;

const NAME: &str = "tmux-scope-fence";
/// This rule's own source names every command and marker it hunts.
const OWN_SOURCE: &str = "spira-lint/src/rules/tmux_scope_fence.rs";

const TMUX_CMDS: &[&str] = &[
    "new-session",
    "kill-server",
    "kill-session",
    "has-session",
    "attach-session",
    "attach",
    "send-keys",
    "list-sessions",
    "list-panes",
    "list-clients",
    "set-option",
    "respawn-pane",
    "start-server",
    "new-window",
    "show-environment",
    "split-window",
    "select-pane",
    "select-window",
    "kill-window",
    "rename-session",
    "display-message",
    "source-file",
];

fn tmux_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let cmds = TMUX_CMDS.join("|");
        Regex::new(&format!(r"(^|[;&|`]|\$\() *tmux ({cmds})\b")).expect("static regex")
    })
}

fn scoped_inline_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"-L[ \t]").expect("static regex"))
}

fn concierge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"concierge\.sh("|')?[ \t]+(start|wake|here|stop)\b"#).expect("static regex")
    })
}

fn contains(content: &[u8], needle: &[u8]) -> bool {
    content.windows(needle.len()).any(|w| w == needle)
}

fn is_comment(line: &[u8]) -> bool {
    let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
    line[start..].starts_with(b"#")
}

/// `spira/test-*.sh`, directly in spira/.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

/// One file's offenders: (1-based line, raw line text).
pub fn scan(content: &[u8]) -> Vec<(usize, String)> {
    let has_tmux_tmpdir = contains(content, b"TMUX_TMPDIR");
    let has_concierge_sock = contains(content, b"CONCIERGE_SOCKET") || contains(content, b"CONCIERGE_SESSION");
    let ls = lines(content);
    let mut out = Vec::new();
    for (i, l) in ls.iter().enumerate() {
        if is_comment(l) {
            continue;
        }
        if tmux_re().is_match(l) {
            if scoped_inline_re().is_match(l) || has_tmux_tmpdir {
                continue;
            }
            out.push((i + 1, String::from_utf8_lossy(l).into_owned()));
        }
    }
    if !has_concierge_sock {
        for (i, l) in ls.iter().enumerate() {
            if is_comment(l) {
                continue;
            }
            if concierge_re().is_match(l) {
                out.push((i + 1, String::from_utf8_lossy(l).into_owned()));
            }
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out
}

impl Rule for TmuxScopeFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in files {
            if e.path == OWN_SOURCE {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            for (line, text) in scan(content) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Scope every tmux call with -L/-S, or export TMUX_TMPDIR; scope every concierge.sh \
start/wake/here/stop with CONCIERGE_SOCKET or CONCIERGE_SESSION. An unscoped call drives \
whatever server the caller's environment already points at."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        TmuxScopeFence.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn empty_scope_refuses_then_bare_kill_server_is_seen_red_and_withdrawn() {
        let t = TempDir::new("tsf");
        t.git_init();
        assert_eq!(run(&t), Err(LintError::EmptyScope));

        t.write("spira/test-clean.sh", "#!/usr/bin/env bash\necho ok\n");
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty());

        t.write("spira/test-broken-tmux.sh", "#!/usr/bin/env bash\ntmux kill-server\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("test-broken-tmux.sh"));
        assert!(got[0].contains("tmux kill-server"));

        t.remove("spira/test-broken-tmux.sh");
        t.git(&["add", "-A"]);
        assert!(run(&t).unwrap().is_empty());
    }

    #[test]
    fn l_scoping_and_file_wide_tmux_tmpdir_both_clear_it() {
        assert!(scan(b"#!/usr/bin/env bash\nSOCK=\"x-$$\"\ntmux -L \"$SOCK\" kill-server 2>/dev/null || true\n").is_empty());
        assert!(scan(b"#!/usr/bin/env bash\nexport TMUX_TMPDIR=\"$(mktemp -d)\"\ntmux start-server\ntmux new-session -d -s cockpit\ntmux kill-server 2>/dev/null || true\n").is_empty());
    }

    #[test]
    fn unscoped_concierge_start_is_caught_and_socket_clears_it() {
        let broken = scan(b"#!/usr/bin/env bash\nHARNESS=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"\nbash \"$HARNESS/concierge.sh\" start\n");
        assert_eq!(broken.len(), 1);
        assert!(broken[0].1.contains("concierge.sh"));

        let scoped = scan(b"#!/usr/bin/env bash\nCONCIERGE_SOCKET=\"t-$$\" CONCIERGE_SESSION=\"t-$$\" bash \"$H/concierge.sh\" start\n");
        assert!(scoped.is_empty());
    }

    #[test]
    fn comment_lines_are_not_flagged() {
        let hits = scan(b"#!/usr/bin/env bash\n# do not call: tmux kill-server\n# and never: bash \"$HARNESS/concierge.sh\" start\nprintf 'ok\\n'\n");
        assert!(hits.is_empty());
    }

}
