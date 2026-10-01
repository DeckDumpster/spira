//! The operator's `spira.toml` locator — see `DESIGN-locate.md` (sp-hconl). Mirrors
//! `conf.sh`'s `spira_toml_file`/`spira_conf_file` as they stand today (post sp-9hwim: no
//! `$SPIRA_REPO` tier — "the running system must not read [from beside the checkout] at
//! all").
//!
//! `discover()` in `lib.rs` is the thin, back-compat `Option<PathBuf>` wrapper the eleven
//! existing Rust callers already use; this module is where the actual tiers live, plus the
//! richer [`LocateOutcome`] a CLI or a human needs to see *why* nothing resolved.

use std::env;
use std::path::PathBuf;

use crate::FILE_NAME;

const LEGACY_FILE_NAME: &str = "spira.conf";

/// The result of searching for the operator's `spira.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateOutcome {
    /// A `spira.toml` was found, at this path.
    Found(PathBuf),
    /// No `spira.toml` anywhere, and no legacy `spira.conf` either. Not an error for a
    /// caller that means to fall back to derived defaults — `tried` is for a diagnostic to
    /// report, not a reason to fail.
    NotFound { tried: Vec<PathBuf> },
    /// No `spira.toml`, but a legacy `spira.conf` resolves. Converting it
    /// (`spira-config convert`) is this box's job before a `spira.toml` exists at all — this
    /// locator never writes one itself (DESIGN-locate.md decision 3).
    LegacyOnly { conf: PathBuf, tried: Vec<PathBuf> },
}

impl LocateOutcome {
    /// `Found` only — what every existing Rust caller already treats "no config" as: `None`,
    /// falling back to its own derived defaults. [`discover`](crate::discover) is this.
    pub fn found(self) -> Option<PathBuf> {
        match self {
            LocateOutcome::Found(p) => Some(p),
            _ => None,
        }
    }
}

/// `$XDG_CONFIG_HOME`, or `$HOME/.config` when it is unset — the directory both `spira.toml`
/// and legacy `spira.conf` tiers 3 resolve a `spira/<file>` candidate under. `None` when
/// neither variable is set (nothing to search at tier 3; tier 4, `/etc/spira`, still runs).
fn xdg_config_home() -> Option<PathBuf> {
    env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
}

/// The tiers searched when no pin (`$SPIRA_TOML`/`$SPIRA_CONF`) is set, for `name` —
/// `${XDG_CONFIG_HOME:-$HOME/.config}/spira/<name>`, then `/etc/spira/<name>`. Shared by the
/// `spira.toml` and legacy `spira.conf` searches, since `conf.sh`'s `spira_toml_file` and
/// `spira_conf_file` run the identical shape for their own file.
fn ambient_tiers(name: &str) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(xdg) = xdg_config_home() {
        c.push(xdg.join("spira").join(name));
    }
    c.push(PathBuf::from("/etc/spira").join(name));
    c
}

/// `spira_conf_file`'s own search, read-only: the legacy `spira.conf` path in force, or
/// `None`. Used only to name the `LegacyOnly` case below — never read, never converted, by
/// this module.
fn locate_legacy() -> Option<PathBuf> {
    if let Ok(p) = env::var("SPIRA_CONF") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    ambient_tiers(LEGACY_FILE_NAME).into_iter().find(|c| c.is_file())
}

/// The search `conf.sh`'s `spira_toml_file` runs today: `explicit` if the caller already has
/// one, else `$SPIRA_TOML` (exclusive — a pinned path that is not a file stops the search
/// right here, it does not fall through), else
/// `${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml`, else `/etc/spira/spira.toml`. No
/// `$SPIRA_REPO` tier (sp-9hwim removed it from bash on 2026-09-29; this was still missing
/// from Rust's `discover()` until this bead).
pub fn locate(explicit: Option<PathBuf>) -> LocateOutcome {
    // NO EXISTENCE CHECK: the caller already decided this is the file (a `--config` flag,
    // not an ambient guess) — same trust `read_input`'s own explicit-file-argument branch
    // in `main.rs` gives a named path, which lets `load()`'s own I/O error name exactly the
    // path the caller asked for instead of this function silently downgrading a typo to "no
    // config, use defaults".
    if let Some(p) = explicit {
        return LocateOutcome::Found(p);
    }

    if let Ok(p) = env::var("SPIRA_TOML") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            LocateOutcome::Found(p)
        } else {
            // EXCLUSIVE PIN, NO FALLTHROUGH: conf.sh's own words for this are "pointing it
            // at a nonexistent path means 'read no file at all', not 'keep looking'" — the
            // other tiers are not even consulted, so `tried` names only the pinned path.
            LocateOutcome::NotFound { tried: vec![p] }
        };
    }

    let tried = ambient_tiers(FILE_NAME);
    if let Some(found) = tried.iter().find(|c| c.is_file()) {
        return LocateOutcome::Found(found.clone());
    }
    match locate_legacy() {
        Some(conf) => LocateOutcome::LegacyOnly { conf, tried },
        None => LocateOutcome::NotFound { tried },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ENV VARS ARE PROCESS-GLOBAL (same hazard lib.rs's own discover tests guard against):
    // every test holding SPIRA_TOML/SPIRA_CONF/SPIRA_REPO/XDG_CONFIG_HOME/HOME takes this
    // lock for its whole body.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> testkit::TempDir {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        testkit::TempDir::new(&format!("spira-config-locate-test-{tag}-{n}"))
    }

    /// True when the machine running this suite has a real `/etc/spira/spira.toml` (or
    /// `spira.conf`) — tests asserting a negative (`NotFound`, "$SPIRA_REPO is ignored")
    /// skip rather than false-pass or false-fail against a host's own file, which this
    /// module's tiers reach even with every other variable cleared.
    fn etc_spira_has(name: &str) -> bool {
        PathBuf::from("/etc/spira").join(name).is_file()
    }

    /// Clears every variable this module's search reads, runs `f`, then restores exactly
    /// what was there before — so a test can set only the variables its scenario needs
    /// without a real operator `~/.config/spira/spira.toml` on the machine running the suite
    /// making it pass for the wrong reason.
    fn with_cleared_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _g = ENV_LOCK.lock().unwrap();
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> =
            names.iter().map(|n| (*n, env::var(n).ok())).collect();
        for n in names {
            env::remove_var(n);
        }
        for (k, v) in vars {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
        let r = f();
        for (n, v) in saved {
            match v {
                Some(v) => env::set_var(n, v),
                None => env::remove_var(n),
            }
        }
        r
    }

    #[test]
    fn explicit_wins_outright() {
        let dir = scratch_dir("explicit");
        let p = dir.join(FILE_NAME);
        std::fs::write(&p, "[spira]\n").unwrap();
        with_cleared_env(&[("SPIRA_TOML", Some("/should-not-be-read/spira.toml"))], || {
            assert_eq!(locate(Some(p.clone())), LocateOutcome::Found(p));
        });
    }

    #[test]
    fn explicit_is_returned_even_when_missing_and_never_falls_through() {
        // The caller named this path on purpose (a --config flag, not an ambient guess) —
        // `locate` trusts it outright, exactly as `discover`'s old unconditional-return
        // behaviour did for an explicit argument, and leaves the "does this file actually
        // exist" failure to `load()`'s own I/O error rather than silently downgrading a typo
        // into "no config, use defaults". A real file sitting at another tier must not be
        // substituted for it.
        let dir = scratch_dir("explicit-miss");
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(xdg.join("spira")).unwrap();
        std::fs::write(xdg.join("spira").join(FILE_NAME), "[spira]\n").unwrap();
        let missing = dir.join("nonexistent.toml");
        with_cleared_env(&[("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()))], || {
            assert_eq!(locate(Some(missing.clone())), LocateOutcome::Found(missing));
        });
    }

    #[test]
    fn pinned_spira_toml_wins_when_it_exists() {
        let dir = scratch_dir("pin-hit");
        let p = dir.join(FILE_NAME);
        std::fs::write(&p, "[spira]\n").unwrap();
        let env_val = p.to_str().unwrap().to_string();
        with_cleared_env(&[("SPIRA_TOML", Some(env_val.as_str()))], || {
            assert_eq!(locate(None), LocateOutcome::Found(p.clone()));
        });
    }

    #[test]
    fn pinned_spira_toml_missing_reports_not_found_naming_only_itself() {
        // The parity case conf.sh's own comment calls out: a pin that does not exist means
        // "read no file at all", not "keep looking" — even with a real file sitting at the
        // XDG tier right next to it.
        let dir = scratch_dir("pin-miss");
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(xdg.join("spira")).unwrap();
        std::fs::write(xdg.join("spira").join(FILE_NAME), "[spira]\n").unwrap();
        let missing = dir.join("nonexistent.toml");
        with_cleared_env(
            &[
                ("SPIRA_TOML", Some(missing.to_str().unwrap())),
                ("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap())),
            ],
            || {
                assert_eq!(
                    locate(None),
                    LocateOutcome::NotFound { tried: vec![missing.clone()] }
                );
            },
        );
    }

    #[test]
    fn spira_repo_is_never_consulted() {
        // THE REGRESSION THIS BEAD FIXES: discover() used to offer $SPIRA_REPO/spira.toml as
        // a candidate (sp-cx0mj), which sp-9hwim's bash rewrite deliberately stopped doing a
        // day later. A spira.toml sitting right there must NOT be found.
        if etc_spira_has(FILE_NAME) {
            eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
            return;
        }
        let dir = scratch_dir("repo-ignored");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(FILE_NAME), "[spira]\n").unwrap();
        let home = dir.join("home-empty");
        std::fs::create_dir_all(&home).unwrap();
        with_cleared_env(
            &[
                ("SPIRA_REPO", Some(repo.to_str().unwrap())),
                ("HOME", Some(home.to_str().unwrap())),
            ],
            || {
                let xdg_candidate = home.join(".config/spira").join(FILE_NAME);
                let etc_candidate = PathBuf::from("/etc/spira").join(FILE_NAME);
                match locate(None) {
                    LocateOutcome::NotFound { tried } => {
                        assert_eq!(tried, vec![xdg_candidate, etc_candidate]);
                    }
                    other => panic!("expected NotFound (never the $SPIRA_REPO file), got {other:?}"),
                }
            },
        );
    }

    #[test]
    fn falls_back_to_xdg_config_home_tier() {
        let dir = scratch_dir("xdg");
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(xdg.join("spira")).unwrap();
        let p = xdg.join("spira").join(FILE_NAME);
        std::fs::write(&p, "[spira]\n").unwrap();
        with_cleared_env(&[("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap()))], || {
            assert_eq!(locate(None), LocateOutcome::Found(p.clone()));
        });
    }

    #[test]
    fn falls_back_to_home_dot_config_when_xdg_unset() {
        let dir = scratch_dir("home");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".config/spira")).unwrap();
        let p = home.join(".config/spira").join(FILE_NAME);
        std::fs::write(&p, "[spira]\n").unwrap();
        with_cleared_env(&[("HOME", Some(home.to_str().unwrap()))], || {
            assert_eq!(locate(None), LocateOutcome::Found(p.clone()));
        });
    }

    #[test]
    fn not_found_when_nothing_exists_and_names_both_tiers() {
        if etc_spira_has(FILE_NAME) {
            eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
            return;
        }
        let dir = scratch_dir("none");
        let home = dir.join("home-empty");
        std::fs::create_dir_all(&home).unwrap();
        with_cleared_env(&[("HOME", Some(home.to_str().unwrap()))], || {
            assert_eq!(
                locate(None),
                LocateOutcome::NotFound {
                    tried: vec![
                        home.join(".config/spira").join(FILE_NAME),
                        PathBuf::from("/etc/spira").join(FILE_NAME),
                    ]
                }
            );
        });
    }

    #[test]
    fn legacy_conf_only_is_reported_as_legacy_only_not_not_found() {
        let dir = scratch_dir("legacy");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".config/spira")).unwrap();
        let conf = home.join(".config/spira").join(LEGACY_FILE_NAME);
        std::fs::write(&conf, "SPIRA_HOME=/x\n").unwrap();
        with_cleared_env(&[("HOME", Some(home.to_str().unwrap()))], || {
            assert_eq!(
                locate(None),
                LocateOutcome::LegacyOnly {
                    conf: conf.clone(),
                    tried: vec![
                        home.join(".config/spira").join(FILE_NAME),
                        PathBuf::from("/etc/spira").join(FILE_NAME),
                    ],
                }
            );
        });
    }

    #[test]
    fn toml_present_wins_over_legacy_conf_at_the_same_tier() {
        let dir = scratch_dir("both");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".config/spira")).unwrap();
        std::fs::write(home.join(".config/spira").join(LEGACY_FILE_NAME), "SPIRA_HOME=/x\n")
            .unwrap();
        let toml = home.join(".config/spira").join(FILE_NAME);
        std::fs::write(&toml, "[spira]\n").unwrap();
        with_cleared_env(&[("HOME", Some(home.to_str().unwrap()))], || {
            assert_eq!(locate(None), LocateOutcome::Found(toml));
        });
    }

    #[test]
    fn found_collapses_other_variants_to_none() {
        assert_eq!(
            LocateOutcome::NotFound { tried: vec![PathBuf::from("/x")] }.found(),
            None
        );
        assert_eq!(
            LocateOutcome::LegacyOnly {
                conf: PathBuf::from("/x/spira.conf"),
                tried: vec![PathBuf::from("/x/spira.toml")]
            }
            .found(),
            None
        );
        let p = PathBuf::from("/x/spira.toml");
        assert_eq!(LocateOutcome::Found(p.clone()).found(), Some(p));
    }
}
