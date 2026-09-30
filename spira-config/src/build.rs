//! How a Spira build compiles (sp-z61hj; DESIGN-build-cache.md): through the box's one
//! shared compilation cache (`sccache`), and — for a one-shot build — without cargo's
//! incremental cache. Every build tool (gate, testenv, release) asks here; none spells the
//! wrapper or the switch itself.
//!
//! No `SCCACHE_BASEDIRS` or `--remap-path-prefix` knob lives here (sp-283wz,
//! DESIGN-build-cache.md §2.5): sccache 0.18.0's Rust frontend never consults basedirs (that
//! wiring exists only for its C/C++ frontend), and adding `--remap-path-prefix` would hash
//! literally, regressing the dependency-crate cache hits this module already gets right.
//! Read §2.5 before reaching for either on a workspace-crate cross-tree miss.

use std::path::{Path, PathBuf};

/// The operator's switch: unset, empty or `sccache` — the cache is required; `off` — an
/// explicit, loud opt-out (a host or container without sccache).
pub const CACHE_ENV: &str = "SPIRA_BUILD_CACHE";

/// The compiler wrapper every build path uses, resolved on the build's own PATH.
pub const WRAPPER: &str = "sccache";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrapper {
    /// Compile through sccache at this absolute path.
    Sccache(PathBuf),
    /// `SPIRA_BUILD_CACHE=off`: no wrapper, said out loud by the caller.
    Off,
}

impl Wrapper {
    /// The environment a cargo invocation gets. `RUSTC_WRAPPER=""` (off) also overrides a
    /// wrapper an operator's cargo config names. Never a `CARGO_*` variable: sccache hashes
    /// those into every key, so one would split the cache between callers (DESIGN §2.2).
    pub fn env(&self) -> Vec<(String, String)> {
        match self {
            Wrapper::Sccache(p) => vec![
                ("RUSTC_WRAPPER".into(), p.display().to_string()),
                // The server dies with whichever client's unit spawned it; a compile whose
                // server vanished runs rustc locally instead of failing the build.
                ("SCCACHE_IGNORE_SERVER_IO_ERROR".into(), "1".into()),
            ],
            Wrapper::Off => vec![("RUSTC_WRAPPER".into(), String::new())],
        }
    }

    /// The environment an AGENT's cargo gets (sp-f4ig1, gate/DESIGN-admission.md §3.3): the
    /// same compiler, fronted by `spira-admit` (`admit`, an absolute path) so every build the
    /// agent starts takes a compile slot for its cargo. The compiler this wrapper would have
    /// been is `SPIRA_ADMIT_INNER` (empty when off: rustc itself); `run` is the pools' home
    /// and `who` the lease's label. Still no `CARGO_*` variable (DESIGN §2.2). The gate and
    /// testenv never use this: they take their slots in-process and keep [`Wrapper::env`].
    pub fn admitted_env(&self, admit: &Path, run: &str, who: &str) -> Vec<(String, String)> {
        let inner = match self {
            Wrapper::Sccache(p) => p.display().to_string(),
            Wrapper::Off => String::new(),
        };
        let mut env = vec![
            ("RUSTC_WRAPPER".into(), admit.display().to_string()),
            (crate::admission::INNER_ENV.into(), inner),
            (crate::admission::WHO_ENV.into(), who.to_string()),
            ("SPIRA_RUN".into(), run.to_string()),
        ];
        if matches!(self, Wrapper::Sccache(_)) {
            env.push(("SCCACHE_IGNORE_SERVER_IO_ERROR".into(), "1".into()));
        }
        env
    }

    /// One line for a log: which cache this build compiles through.
    pub fn describe(&self) -> String {
        match self {
            Wrapper::Sccache(p) => format!(
                "build cache: sccache ({}) — dependency crates only, cross-tree; a workspace \
                 crate's own build and every binary's link/codegen are per-tree (sp-283wz, \
                 DESIGN-build-cache.md §2.5)",
                p.display()
            ),
            Wrapper::Off => format!("build cache: OFF ({CACHE_ENV}=off) — every dependency compiles cold"),
        }
    }
}

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// The first executable `name` on `path` (a PATH string), as an absolute path.
pub fn find_on(path: &str, name: &str) -> Option<PathBuf> {
    path.split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join(name))
        .find(|p| executable(p))
}

/// The wrapper a build running under `path` uses. `setting` is `$SPIRA_BUILD_CACHE`. Err is
/// the refusal: sccache is required and not on `path`, or the setting is not a known value.
pub fn wrapper(path: &str, setting: Option<&str>) -> Result<Wrapper, String> {
    match setting.map(str::trim).unwrap_or("") {
        "off" => Ok(Wrapper::Off),
        "" | "sccache" => find_on(path, WRAPPER).map(Wrapper::Sccache).ok_or_else(|| {
            format!(
                "sccache is not on the build's PATH ({path}) — every Spira build compiles through the box's one shared compilation cache (spira/deps.toml; install: cargo install sccache --locked). {CACHE_ENV}=off builds uncached, on purpose"
            )
        }),
        other => Err(format!("{CACHE_ENV}={other:?} is not a build cache setting (unset, `sccache` or `off`)")),
    }
}

/// [`wrapper`] for this process: its own PATH and `$SPIRA_BUILD_CACHE`.
pub fn wrapper_from_env() -> Result<Wrapper, String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let setting = std::env::var(CACHE_ENV).ok();
    wrapper(&path, setting.as_deref())
}

/// The arguments that make a cargo build of `profile` one-shot: no incremental cache
/// (write-heavy, useless to a build that is not repeated in place). A `--config` switch, not
/// `CARGO_INCREMENTAL=0`, so the dependencies' cache keys match an interactive build's.
pub fn one_shot(profile: &str) -> [String; 2] {
    ["--config".into(), format!("profile.{profile}.incremental=false")]
}

/// [`one_shot`] as shell words, for a command string.
pub fn one_shot_words(profile: &str) -> String {
    format!("--config profile.{profile}.incremental=false")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn bin_dir(with: bool) -> (testkit::TempDir, String) {
        let d = testkit::TempDir::new("spira-config-build");
        if with {
            let p = d.path().join("sccache");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("/nonexistent-sp-z61hj:{}", d.path().display());
        (d, path)
    }

    #[test]
    fn sccache_on_the_path_is_the_wrapper_by_absolute_path() {
        let (d, path) = bin_dir(true);
        for s in [None, Some(""), Some("sccache"), Some(" sccache ")] {
            assert_eq!(wrapper(&path, s).unwrap(), Wrapper::Sccache(d.path().join("sccache")), "{s:?}");
        }
        let env = wrapper(&path, None).unwrap().env();
        assert_eq!(env[0], ("RUSTC_WRAPPER".into(), d.path().join("sccache").display().to_string()));
        assert!(env.iter().any(|(k, v)| k == "SCCACHE_IGNORE_SERVER_IO_ERROR" && v == "1"));
    }

    #[test]
    fn absent_sccache_is_a_refusal_naming_deps_and_the_opt_out() {
        let (_d, path) = bin_dir(false);
        let e = wrapper(&path, None).unwrap_err();
        assert!(e.contains("sccache is not on the build's PATH"), "{e}");
        assert!(e.contains("spira/deps.toml") && e.contains("SPIRA_BUILD_CACHE=off"), "{e}");
        // A relative PATH entry is never searched.
        assert!(wrapper("relative/dir", None).is_err());
    }

    #[test]
    fn off_is_explicit_and_clears_any_configured_wrapper() {
        let (_d, path) = bin_dir(false);
        let w = wrapper(&path, Some("off")).unwrap();
        assert_eq!(w, Wrapper::Off);
        assert_eq!(w.env(), vec![("RUSTC_WRAPPER".to_string(), String::new())]);
        assert!(w.describe().contains("OFF"));
    }

    #[test]
    fn an_agents_build_is_fronted_by_spira_admit_with_the_same_compiler_inside() {
        let (d, path) = bin_dir(true);
        let admit = Path::new("/rel/bin/spira-admit");
        let env = wrapper(&path, None).unwrap().admitted_env(admit, "/run/spira", "sp-abc");
        let get = |k: &str| env.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("RUSTC_WRAPPER").as_deref(), Some("/rel/bin/spira-admit"));
        assert_eq!(get("SPIRA_ADMIT_INNER"), Some(d.path().join("sccache").display().to_string()));
        assert_eq!(get("SPIRA_ADMIT_WHO").as_deref(), Some("sp-abc"));
        assert_eq!(get("SPIRA_RUN").as_deref(), Some("/run/spira"));
        assert_eq!(get("SCCACHE_IGNORE_SERVER_IO_ERROR").as_deref(), Some("1"));
        assert!(env.iter().all(|(k, _)| !k.starts_with("CARGO_")), "{env:?}");
        // Off: admission still fronts rustc; the inner compiler is rustc itself.
        let off = Wrapper::Off.admitted_env(admit, "/run/spira", "sp-abc");
        assert!(off.contains(&("SPIRA_ADMIT_INNER".to_string(), String::new())));
        assert!(off.contains(&("RUSTC_WRAPPER".to_string(), "/rel/bin/spira-admit".to_string())));
    }

    #[test]
    fn an_unknown_setting_is_refused_not_read_as_off() {
        let (_d, path) = bin_dir(true);
        assert!(wrapper(&path, Some("no")).unwrap_err().contains("not a build cache setting"));
    }

    #[test]
    fn no_build_variable_is_a_cargo_variable() {
        // sccache hashes every CARGO_* variable into a key (DESIGN §2.2): none may come from here.
        let (_d, path) = bin_dir(true);
        for w in [wrapper(&path, None).unwrap(), Wrapper::Off] {
            assert!(w.env().iter().all(|(k, _)| !k.starts_with("CARGO_")), "{:?}", w.env());
        }
        assert_eq!(one_shot("aeon"), ["--config".to_string(), "profile.aeon.incremental=false".to_string()]);
        assert_eq!(one_shot_words("release"), "--config profile.release.incremental=false");
    }
}
