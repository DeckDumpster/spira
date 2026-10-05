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



/// The result of searching for the operator's `spira.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateOutcome {
    /// A `spira.toml` was found, at this path.
    Found(PathBuf),
    /// No `spira.toml` anywhere, and no legacy `spira.conf` either. Not an error for a
    /// caller that means to fall back to derived defaults — `tried` is for a diagnostic to
    /// report, not a reason to fail.
    NotFound { tried: Vec<PathBuf> },
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

    // THE ONE SOURCE (per Ryan 2026-10-05): `$SPIRA_TOML` names the file. No XDG/HOME/etc
    // tier, no legacy spira.conf, no shipped example — an unset or missing pin is NotFound,
    // and every caller refuses on it.
    match env::var("SPIRA_TOML") {
        Ok(spec) if !spec.is_empty() => {
            // A file, or base:override layers — every one must exist.
            let layers: Vec<PathBuf> = spec.split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect();
            match layers.iter().find(|p| !p.is_file()) {
                Some(missing) => LocateOutcome::NotFound { tried: vec![missing.clone()] },
                None => LocateOutcome::Found(PathBuf::from(spec)),
            }
        }
        _ => LocateOutcome::NotFound { tried: Vec::new() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ENV VARS ARE PROCESS-GLOBAL: every test takes crate::ENV_LOCK for its whole body.
    static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> testkit::TempDir {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        testkit::TempDir::new(&format!("spira-config-locate-test-{tag}-{n}"))
    }

    fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, env::var(n).ok())).collect();
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
        for (k, v) in saved {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
        r
    }

    #[test]
    fn spira_toml_names_the_file() {
        let d = scratch_dir("pin");
        let p = d.join("spira.toml");
        std::fs::write(&p, "[spira]\n").unwrap();
        let got = with_env(&[("SPIRA_TOML", Some(p.to_str().unwrap()))], || locate(None));
        assert_eq!(got, LocateOutcome::Found(p));
    }

    #[test]
    fn a_pin_to_a_missing_file_is_not_found_naming_it() {
        let d = scratch_dir("missing");
        let p = d.join("nope.toml");
        let got = with_env(&[("SPIRA_TOML", Some(p.to_str().unwrap()))], || locate(None));
        assert_eq!(got, LocateOutcome::NotFound { tried: vec![p] });
    }

    /// THE ONE SOURCE: with SPIRA_TOML unset, a spira.toml in every place the old search
    /// looked (XDG, HOME/.config) and a legacy spira.conf beside it are all ignored.
    #[test]
    fn nothing_is_discovered_without_the_pin() {
        let d = scratch_dir("nodiscover");
        let xdg = d.join("xdg");
        std::fs::create_dir_all(xdg.join("spira")).unwrap();
        std::fs::write(xdg.join("spira/spira.toml"), "[spira]\n").unwrap();
        std::fs::write(xdg.join("spira/spira.conf"), "SPIRA_HOME=/x\n").unwrap();
        let home = d.join("home");
        std::fs::create_dir_all(home.join(".config/spira")).unwrap();
        std::fs::write(home.join(".config/spira/spira.toml"), "[spira]\n").unwrap();
        let conf = xdg.join("spira/spira.conf");
        let got = with_env(
            &[
                ("XDG_CONFIG_HOME", Some(xdg.to_str().unwrap())),
                ("HOME", Some(home.to_str().unwrap())),
                ("SPIRA_CONF", Some(conf.to_str().unwrap())),
            ],
            || locate(None),
        );
        assert_eq!(got, LocateOutcome::NotFound { tried: Vec::new() });
    }

    #[test]
    fn an_explicit_path_is_taken_as_given() {
        let p = PathBuf::from("/explicit/spira.toml");
        assert_eq!(with_env(&[], || locate(Some(p.clone()))), LocateOutcome::Found(p));
    }
}
