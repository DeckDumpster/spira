//! The model's restricted environment (design §3.5;
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

/// The model's release PATH entry (sp-zf4q3): `<release>/model-bin`, which holds only the
/// binaries a model may run — NEVER the release `bin/` that [`work_bin_dir`] finds, which
/// also holds ~40 tools that call bd themselves (bdq, mail, spira-claim, bead, sentinel,
/// queue, ...). The release root is `SPIRA_RELEASE` from the aeon's own environment when
/// set, else the parent of the directory holding `work` on `path`. FAIL-CLOSED: a release
/// with no executable `model-bin/work` is an error naming the one exit,
/// `SPIRA_MODEL_BIN_CONSIDERED=<dir>` (an operator-chosen directory, used as given and
/// held to the same "holds an executable `work`" check) — never a fallback to `bin/`.
pub fn model_bin_dir(base: &BTreeMap<String, String>, path: &str, is_exe: impl Fn(&Path) -> bool) -> Result<String, String> {
    use spira_config::release_env::{MODEL_BIN_DIR, MODEL_BIN_OVERRIDE_ENV};
    if let Some(dir) = base.get(MODEL_BIN_OVERRIDE_ENV).filter(|v| !v.is_empty()) {
        if is_exe(&Path::new(dir).join("work")) {
            return Ok(dir.clone());
        }
        return Err(format!("{MODEL_BIN_OVERRIDE_ENV}={dir} holds no executable work — refusing to start the model"));
    }
    let release = match base.get(spira_config::RELEASE_ENV).filter(|v| !v.is_empty()) {
        Some(r) => r.clone(),
        None => {
            let Some(work_dir) = work_bin_dir(path, &is_exe) else {
                return Err(format!(
                    "work is not on PATH and {} is unset — the launcher sets PATH to a release; or set {MODEL_BIN_OVERRIDE_ENV}=<dir holding only work>",
                    spira_config::RELEASE_ENV
                ));
            };
            match Path::new(&work_dir).parent() {
                Some(p) => p.display().to_string(),
                None => return Err(format!("{work_dir} has no parent release directory; set {MODEL_BIN_OVERRIDE_ENV}=<dir holding only work>")),
            }
        }
    };
    let dir = format!("{}/{MODEL_BIN_DIR}", release.trim_end_matches('/'));
    if is_exe(&Path::new(&dir).join("work")) {
        Ok(dir)
    } else {
        Err(format!(
            "release {release} has no {MODEL_BIN_DIR}/work — refusing to start the model with the full bin/ on PATH (it holds tools that call bd); activate a release built with {MODEL_BIN_DIR}/, or set {MODEL_BIN_OVERRIDE_ENV}=<dir holding only work>"
        ))
    }
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
    // `work` (the one tool on the model's PATH) resolves its lifecycle socket from the one
    // source of config: it needs the spec and the release that locates its key registry.
    // Both only name files the model's HOME can already reach — no new exposure.
    "SPIRA_TOML",
    "SPIRA_RELEASE",
    "BEAD_ID",
    "BEADS_ACTOR",
    "SPIRA_MAIL",
    "RUSTC_WRAPPER",
    "SPIRA_ADMIT_INNER",
    "SPIRA_ADMIT_WHO",
    "SCCACHE_IGNORE_SERVER_IO_ERROR",
    "SCCACHE_WEBDAV_ENDPOINT",
    "SCCACHE_WEBDAV_KEY_PREFIX",
];

/// The cargo toolchain directory: `$CARGO_HOME/bin`, else `$HOME/.cargo/bin`, from the aeon's
/// own environment. None when neither is set.
fn cargo_bin(base: &BTreeMap<String, String>) -> Option<String> {
    if let Some(c) = base.get("CARGO_HOME").filter(|v| !v.is_empty()) {
        return Some(format!("{c}/bin"));
    }
    base.get("HOME").filter(|v| !v.is_empty()).map(|h| format!("{h}/.cargo/bin"))
}

/// The child environment the model runs under, given the bead it is bound to, the
/// aeon's own (unrestricted) child environment, and the model's release PATH entry
/// ([`model_bin_dir`] — never the release `bin/`).
pub fn restricted_env(bead_id: &str, base: &BTreeMap<String, String>, work_dir: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for k in UNCONDITIONAL {
        env.insert(k.to_string(), base.get(*k).cloned().unwrap_or_default());
    }
    // The cargo toolchain (cargo, rustc, sccache) and nothing else from the operator's tail
    // (sp-tx6ot): without it the model cannot build with the cache or run testenv/gate
    // (no-build-cache), and submits unverified work. ~/.local/bin stays out: it holds bd.
    let path = match cargo_bin(base) {
        Some(c) => format!("/usr/bin:/bin:{work_dir}:{c}"),
        None => format!("/usr/bin:/bin:{work_dir}"),
    };
    env.insert("PATH".to_string(), path);
    env.insert("SPIRA_WORK_BEAD_ID".to_string(), bead_id.to_string());
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

    /// Every variable that locates bd, its database, or dolt — none may reach the model.
    const DB_LOCATORS: &[&str] = &[
        "SPIRA_DB",
        "SPIRA_BD",
        "BEADS_DIR",
        "BEADS_DB",
        "BD_DB",
        "SPIRA_DOLT",
        "SPIRA_DOLT_PORT",
        "SPIRA_DOLT_HOST",
        "DOLT_ROOT_PATH",
        "DOLT_CLI_PASSWORD",
        "SPIRA_LC_PASSWORD_FILE",
        "SPIRA_MODEL_BIN_CONSIDERED",
    ];

    #[test]
    fn bd_and_db_never_appear() {
        let mut b = base(&[("HOME", "/home/aeon")]);
        for k in DB_LOCATORS {
            b.insert(k.to_string(), "/somewhere/db".to_string());
        }
        let e = restricted_env("sp-x", &b, "/rel/model-bin");
        for k in DB_LOCATORS {
            assert!(!e.contains_key(*k), "{k} must never leak into the restricted env");
        }
        for k in e.keys() {
            assert!(!k.contains("DOLT") && !k.starts_with("BEADS_D") && !k.ends_with("_DB") && !k.ends_with("_BD"), "{k} looks like a db locator");
        }
    }

    fn exe_at(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| paths.iter().any(|q| p == Path::new(q))
    }

    #[test]
    fn model_bin_is_the_release_sibling_of_bin_never_bin_itself() {
        let is = exe_at(&["/rel/bin/work", "/rel/model-bin/work"]);
        let d = model_bin_dir(&BTreeMap::new(), "/usr/bin:/rel/bin", is).unwrap();
        assert_eq!(d, "/rel/model-bin");
        let e = restricted_env("sp-x", &BTreeMap::new(), &d);
        let path = e.get("PATH").unwrap();
        assert!(path.split(':').any(|x| x == "/rel/model-bin"), "{path}");
        assert!(!path.split(':').any(|x| x == "/rel/bin"), "the full bin dir must never be on the model's PATH: {path}");
    }

    #[test]
    fn model_bin_prefers_spira_release() {
        let is = exe_at(&["/other/bin/work", "/rel/model-bin/work"]);
        let b = base(&[("SPIRA_RELEASE", "/rel/")]);
        assert_eq!(model_bin_dir(&b, "/other/bin", is).unwrap(), "/rel/model-bin");
    }

    #[test]
    fn a_missing_model_bin_refuses_and_names_the_override() {
        let is = exe_at(&["/rel/bin/work"]);
        let err = model_bin_dir(&BTreeMap::new(), "/rel/bin", is).unwrap_err();
        assert!(err.contains("SPIRA_MODEL_BIN_CONSIDERED"), "{err}");
        let err = model_bin_dir(&BTreeMap::new(), "/usr/bin", exe_at(&[])).unwrap_err();
        assert!(err.contains("SPIRA_MODEL_BIN_CONSIDERED"), "{err}");
    }

    #[test]
    fn the_override_is_used_as_given_and_still_needs_work() {
        let b = base(&[("SPIRA_MODEL_BIN_CONSIDERED", "/opt/model")]);
        assert_eq!(model_bin_dir(&b, "/rel/bin", exe_at(&["/opt/model/work"])).unwrap(), "/opt/model");
        let err = model_bin_dir(&b, "/rel/bin", exe_at(&["/rel/model-bin/work"])).unwrap_err();
        assert!(err.contains("no executable work"), "{err}");
    }

    #[test]
    fn path_is_exactly_the_two_system_dirs_plus_work() {
        let e = restricted_env("sp-x", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("PATH").unwrap(), "/usr/bin:/bin:/rel/bin");
    }

    #[test]
    fn path_adds_the_cargo_toolchain_and_never_the_inherited_path() {
        let b = base(&[("HOME", "/h"), ("PATH", "/h/.local/bin:/elsewhere")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert_eq!(e.get("PATH").unwrap(), "/usr/bin:/bin:/rel/bin:/h/.cargo/bin");
        let c = base(&[("HOME", "/h"), ("CARGO_HOME", "/opt/cargo")]);
        assert_eq!(restricted_env("sp-x", &c, "/rel/bin").get("PATH").unwrap(), "/usr/bin:/bin:/rel/bin:/opt/cargo/bin");
        assert!(!e.get("PATH").unwrap().contains(".local/bin"), "bd lives in ~/.local/bin; it must stay out");
    }

    #[test]
    fn bead_id_is_always_the_bound_one() {
        let e = restricted_env("sp-bound1", &BTreeMap::new(), "/rel/bin");
        assert_eq!(e.get("SPIRA_WORK_BEAD_ID").unwrap(), "sp-bound1");
    }

    #[test]
    fn the_retired_lifecycle_switch_never_reaches_the_model() {
        let b = base(&[("SPIRA_LIFECYCLE_ENFORCE", "1")]);
        let e = restricted_env("sp-x", &b, "/rel/bin");
        assert!(!e.contains_key("SPIRA_LIFECYCLE_ENFORCE"));
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
            ("RUSTC_WRAPPER", "/rel/bin/spira-admit"),
            ("SPIRA_ADMIT_INNER", "/usr/bin/sccache"),
            ("SPIRA_ADMIT_WHO", "sp-x"),
            ("SCCACHE_IGNORE_SERVER_IO_ERROR", "1"),
            ("SCCACHE_WEBDAV_ENDPOINT", "http://box:9431"),
            ("SCCACHE_WEBDAV_KEY_PREFIX", "/"),
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
        allowed.extend(["PATH", "SPIRA_WORK_BEAD_ID"]);
        allowed.extend(CONDITIONAL);
        for k in e.keys() {
            assert!(allowed.contains(&k.as_str()), "{k} is not on the allow-list");
        }
        assert!(!e.contains_key("SHELL"));
        assert!(!e.contains_key("SSH_AUTH_SOCK"));
        assert!(!e.contains_key("AWS_SECRET_ACCESS_KEY"));
    }
}
