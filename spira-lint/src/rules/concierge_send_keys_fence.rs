//! `concierge-send-keys-fence` — no harness component outside the documented set types into a
//! tmux pane or calls the Concierge's wake verb. Signals reach the Concierge through the durable
//! inbox; keystrokes land inside a person's half-written message.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{lines, Entry, Finding, LintError, Rule, Tree};

pub struct ConciergeSendKeysFence;

const NAME: &str = "concierge-send-keys-fence";
const OWN_SOURCE: &str = "spira-lint/src/rules/concierge_send_keys_fence.rs";

/// May name `send-keys`: the wake verb itself and the cockpit tools that drive their own panes.
const KEYS_ALLOWED: &[&str] = &["concierge.sh", "cockpit/", "spira-lint/src/rules/tmux_scope_fence.rs"];
/// May call the wake verb: the inbox-keeper, the push flusher, the mail deliverer, and the
/// config that names the verb.
const WAKE_ALLOWED: &[&str] = &[
    "concierge.sh",
    "spira/inbox-keeper.sh",
    "spira-runtime/src/wake.rs",
    "spira/inbox-append.sh",
    "spira/spira-mail-deliver.sh",
    "spira/conf.sh",
    "spira/conf.d.defaults.generated.sh",
    "spira/conf.d/",
    "spira/config-delta.toml",
];

fn keys_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"send-keys").expect("static regex"))
}

fn wake_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(concierge\.sh|\$CONCIERGE|\$\{CONCIERGE\})["' ]+wake\b|\.arg\("wake"\)"#).expect("static regex"))
}

fn is_comment_line(line: &[u8]) -> bool {
    let t = line.iter().position(|b| !b.is_ascii_whitespace()).map_or(&line[line.len()..], |i| &line[i..]);
    t.starts_with(b"#") || t.starts_with(b"//")
}

fn allowed(list: &[&str], path: &str) -> bool {
    list.iter().any(|a| if a.ends_with('/') { path.starts_with(a) } else { path == *a })
}

fn in_scope(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    path != OWN_SOURCE
        && !base.starts_with("test-")
        && !path.contains("/tests/")
        && !path.contains("/testlib/")
        && [".sh", ".rs", ".service", ".toml", ".timer"].iter().any(|x| base.ends_with(x))
}

pub fn scan(path: &str, content: &[u8]) -> Vec<(usize, String)> {
    let keys = !allowed(KEYS_ALLOWED, path);
    let wake = !allowed(WAKE_ALLOWED, path);
    lines(content)
        .iter()
        .enumerate()
        .filter(|(_, l)| !is_comment_line(l))
        .filter(|(_, l)| (keys && keys_re().is_match(l)) || (wake && wake_re().is_match(l)))
        .map(|(i, l)| (i + 1, String::from_utf8_lossy(l).trim().to_string()))
        .collect()
}

impl Rule for ConciergeSendKeysFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in files {
            let Some(content) = tree.content(e) else { continue };
            for (line, text) in scan(&e.path, content) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Signal the Concierge by writing a row to its inbox (spira/inbox-append.sh). Never type \
into its pane: keystrokes land inside a person's half-written message. Only the inbox-keeper, \
the push flusher and the mail deliverer may call `concierge.sh wake`, which holds until the \
input line is empty."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConciergeSendKeysFence)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_keys_caller_is_flagged_and_the_allowed_set_is_not() {
        let rogue = b"#!/usr/bin/env bash\ntmux -L x send-keys -t concierge -l hi\n";
        assert_eq!(scan("spira/rogue.sh", rogue).len(), 1);
        assert!(scan("concierge.sh", rogue).is_empty());
        assert!(scan("cockpit/ops/src/layout.rs", rogue).is_empty());
    }

    #[test]
    fn a_wake_caller_is_flagged_and_the_documented_callers_are_not() {
        let rogue = b"#!/usr/bin/env bash\nconcierge.sh wake \"hi\"\n\"$CONCIERGE\" wake x\n";
        assert_eq!(scan("spira/rogue.sh", rogue).len(), 2);
        assert!(scan("spira/inbox-keeper.sh", rogue).is_empty());
    }

    #[test]
    fn a_rust_wake_call_is_flagged_outside_the_runtime_wake_module() {
        let rogue = b"Command::new(c).arg(\"wake\").arg(line);\n";
        assert_eq!(scan("spira-runtime/src/other.rs", rogue).len(), 1);
        assert!(scan("spira-runtime/src/wake.rs", rogue).is_empty());
    }

    #[test]
    fn comments_and_tests_are_silent() {
        assert!(scan("spira/x.sh", b"# tmux send-keys is forbidden\n").is_empty());
        assert!(!in_scope("spira/test-x.sh"));
        assert!(in_scope("concierge-guard/src/main.rs"));
    }

    #[test]
    fn the_guard_is_in_scope_so_a_send_keys_there_is_caught() {
        assert_eq!(scan("concierge-guard/src/main.rs", b"Command::new(\"tmux\").arg(\"send-keys\");\n").len(), 1);
    }
}
