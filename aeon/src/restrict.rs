//! The model's restricted environment when `lifecycle_enforce` is on (design §3.5;
//! replaces `spira/work-env.sh`, sp-zpaq0). This bead's own acceptance criterion carries
//! over unchanged: "in the provided aeon environment, `command -v bd` fails and no
//! credential is readable."
//!
//! `env -i` with an explicit allow-list, not `unset` on a deny-list, for the reason
//! work-env.sh's own header gave and this crate keeps: a deny-list is only as complete as
//! whoever last remembered to update it, and a new credential-bearing var added anywhere
//! else in this harness would leak through it by default. An allow-list leaks nothing new
//! by construction.
//!
//! RETIRED RATHER THAN PORTED AS A WRAPPER PROCESS. work-env.sh's only caller was aeon.sh
//! itself (work/DESIGN.md §2: "only through aeon.sh's work-env.sh wrap"; no harness script
//! invoked it standalone) — so its general `<bead-id> [-- cmd args...]` CLI shape, built
//! for a caller that never existed, is dropped. What is kept is the one property a test
//! can hold it to: the exact set of variables the model's process sees.

use std::collections::BTreeMap;
use std::path::Path;

/// `dirname "$(command -v work)"` — the directory to add to the restricted PATH so `work`
/// is reachable and nothing else installed beside it changes that. `exists` is injected so
/// tests do not need real files on a real PATH.
pub fn work_bin_dir(path: &str, exists: impl Fn(&Path) -> bool) -> Option<String> {
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        if exists(&Path::new(dir).join("work")) {
            return Some(dir.to_string());
        }
    }
    None
}

/// The allow-listed vars carried through unconditionally, always present (empty when the
/// source is), exactly work-env.sh's `"KEY=$VAR"` tokens (no `${VAR:+…}` guard).
const UNCONDITIONAL: &[&str] = &["HOME", "SPIRA_LC_SOCKET"];

/// The allow-listed vars carried through only when set and non-empty, exactly
/// work-env.sh's `${VAR:+"KEY=$VAR"}` tokens.
const CONDITIONAL: &[&str] = &[
    "SPIRA_FAYTH",
    "TERM",
    "LANG",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "SPIRA_AEON",
    "SPIRA_AEON_OVERRIDE",
    "SPIRA_RUN",
    "SPIRA_PROD",
    "BEAD_ID",
    "BEADS_ACTOR",
    "SPIRA_MAIL",
];

/// The child environment the model runs under, given the bead it is bound to, the
/// aeon's own (unrestricted) child environment, and the resolved directory holding `work`.
pub fn restricted_env(bead_id: &str, base: &BTreeMap<String, String>, work_dir: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for k in UNCONDITIONAL {
        env.insert(k.to_string(), base.get(*k).cloned().unwrap_or_default());
    }
    env.insert("PATH".to_string(), format!("/usr/bin:/bin:{work_dir}"));
    env.insert("SPIRA_WORK_BEAD_ID".to_string(), bead_id.to_string());
    env.insert(
        "SPIRA_LIFECYCLE_ENFORCE".to_string(),
        base.get("SPIRA_LIFECYCLE_ENFORCE").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "0".to_string()),
    );
    for k in CONDITIONAL {
        if let Some(v) = base.get(*k).filter(|v| !v.is_empty()) {
            env.insert(k.to_string(), v.clone());
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn work_bin_dir_finds_the_first_hit() {
        let found = |p: &Path| p == Path::new("/rel/bin/work");
        assert_eq!(work_bin_dir("/usr/bin:/rel/bin:/rel/spira", found), Some("/rel/bin".to_string()));
    }

    #[test]
    fn work_bin_dir_none_when_not_on_path() {
        assert_eq!(work_bin_dir("/usr/bin:/bin", |_| false), None);
    }

    #[test]
    fn work_bin_dir_skips_empty_path_segments() {
        let found = |p: &Path| p == Path::new("/bin/work");
        assert_eq!(work_bin_dir("::/bin:", found), Some("/bin".to_string()));
    }

    #[test]
    fn bd_and_db_never_appear() {
        let b = base(&[("SPIRA_DB", "prod-db"), ("SPIRA_BD", "/bin/bd"), ("HOME", "/home/aeon")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert!(!e.contains_key("SPIRA_DB"), "SPIRA_DB must never leak into the restricted env");
        assert!(!e.contains_key("SPIRA_BD"), "SPIRA_BD must never leak into the restricted env");
    }

    #[test]
    fn path_is_exactly_the_two_system_dirs_plus_work() {
        let e = restricted_env("sp-x", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("PATH").unwrap(), "/usr/bin:/bin:/rel/bin");
    }

    #[test]
    fn bead_id_is_always_the_bound_one() {
        let e = restricted_env("sp-bound1", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("SPIRA_WORK_BEAD_ID").unwrap(), "sp-bound1");
    }

    #[test]
    fn lifecycle_enforce_defaults_to_0_when_absent() {
        let e = restricted_env("sp-x", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("SPIRA_LIFECYCLE_ENFORCE").unwrap(), "0");
    }

    #[test]
    fn lifecycle_enforce_defaults_to_0_when_present_but_empty() {
        let b = base(&[("SPIRA_LIFECYCLE_ENFORCE", "")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert_eq!(e.get("SPIRA_LIFECYCLE_ENFORCE").unwrap(), "0");
    }

    #[test]
    fn lifecycle_enforce_carries_through_when_set() {
        let b = base(&[("SPIRA_LIFECYCLE_ENFORCE", "1")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert_eq!(e.get("SPIRA_LIFECYCLE_ENFORCE").unwrap(), "1");
    }

    #[test]
    fn home_is_unconditional_even_when_absent() {
        let e = restricted_env("sp-x", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("HOME").unwrap(), "");
    }

    #[test]
    fn conditional_vars_are_absent_when_unset() {
        let e = restricted_env("sp-x", &BTreeMap::new(), "/rel/bin");
        for k in CONDITIONAL {
            assert!(!e.contains_key(*k), "{k} must be absent when unset, not present-and-empty");
        }
    }

    #[test]
    fn conditional_vars_are_absent_when_set_but_empty() {
        let b = base(&[("TERM", ""), ("SPIRA_MAIL", "")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert!(!e.contains_key("TERM"));
        assert!(!e.contains_key("SPIRA_MAIL"));
    }

    #[test]
    fn conditional_vars_carry_through_when_non_empty() {
        let b = base(&[
            ("SPIRA_FAYTH", "builder"),
            ("TERM", "xterm"),
            ("LANG", "C.UTF-8"),
            ("GIT_AUTHOR_NAME", "aeon-ifrit"),
            ("GIT_AUTHOR_EMAIL", "aeon-ifrit@spira.local"),
            ("GIT_COMMITTER_NAME", "aeon-ifrit"),
            ("GIT_COMMITTER_EMAIL", "aeon-ifrit@spira.local"),
            ("SPIRA_AEON", "ifrit"),
            ("SPIRA_AEON_OVERRIDE", "shiva"),
            ("SPIRA_RUN", "/run/spira"),
            ("SPIRA_PROD", "/prod/spira"),
            ("BEAD_ID", "sp-x"),
            ("BEADS_ACTOR", "aeon-ifrit"),
            ("SPIRA_MAIL", "/run/spira/mail"),
        ]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        for k in CONDITIONAL {
            assert_eq!(e.get(*k), b.get(*k), "{k} did not carry through unchanged");
        }
    }

    #[test]
    fn nothing_outside_the_allowlist_survives() {
        let b = base(&[("HOME", "/h"), ("PATH", "/whatever"), ("SHELL", "/bin/zsh"), ("SSH_AUTH_SOCK", "/tmp/sock"), ("AWS_SECRET_ACCESS_KEY", "leak")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        let mut allowed: Vec<&str> = UNCONDITIONAL.to_vec();
        allowed.extend(["PATH", "SPIRA_WORK_BEAD_ID", "SPIRA_LIFECYCLE_ENFORCE"]);
        allowed.extend(CONDITIONAL);
        for k in e.keys() {
            assert!(allowed.contains(&k.as_str()), "{k} is not on the allow-list");
        }
        assert!(!e.contains_key("SHELL"));
        assert!(!e.contains_key("SSH_AUTH_SOCK"));
        assert!(!e.contains_key("AWS_SECRET_ACCESS_KEY"));
    }
}
