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
//!
//! THE SHARED STORE (sp-xtdqi, reversing sccache-dav/DESIGN.md §5's earlier call): a box
//! that runs `sccache-dav` (sp-xjnzl) names it with `SPIRA_SCCACHE_DAV_ADDR` — resolved
//! in-process, `[spira]`-table-aware, via [`Store::from_values`]/[`Store::from_env`], never a
//! bare environment read (law-a-binary-resolves-the-config-it-reads) — and [`Wrapper::env`]/
//! [`Wrapper::admitted_env`] carry it (`SCCACHE_WEBDAV_ENDPOINT`/`SCCACHE_WEBDAV_KEY_PREFIX`)
//! into every build this module wires up, not only the one whose own `~/.cargo/config.toml`
//! happened to set it. The earlier design (DESIGN.md §5) kept this an "ambient environment
//! change" on purpose; it did not survive a gate restarting the box's one sccache client
//! daemon, which fixes its backend at spawn time and then ignores every later invocation's
//! environment — the daemon silently kept answering from the LOCAL DISK cache until an
//! operator noticed and re-exported the webdav vars by hand.

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

/// The conf.d key naming this box's shared compilation cache (sp-xtdqi): `ip:port`, no
/// default — absent means the unit is not installed and no build points at a shared store.
pub const STORE_ADDR_ENV: &str = "SPIRA_SCCACHE_DAV_ADDR";

/// The configured store address, resolved in-process from this binary's own environment and
/// the `spira.toml` [`crate::discover`] finds. `None` on an unset `SPIRA_HOME`, a resolution
/// error, or an unconfigured store.
pub fn addr_from_env() -> Option<String> {
    let home = std::env::var("SPIRA_HOME").ok().filter(|v| !v.is_empty())?;
    addr_for_home(Path::new(&home))
}

/// [`addr_from_env`] for a caller that located `home` itself (`unit-ensure`, which may run
/// with `SPIRA_HOME` unset).
pub fn addr_for_home(home: &Path) -> Option<String> {
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let repo = crate::resolve::derive_home_repo(home, &env_map);
    let resolved = crate::resolve::resolve_for_process(home, &repo, &env_map).ok()?;
    Some(resolved.get(STORE_ADDR_ENV).to_string()).filter(|v| !v.trim().is_empty())
}

/// This box's shared compilation cache (sccache-dav, sp-xjnzl): the operator's own endpoint,
/// resolved from config, never hardcoded. `key_prefix` is fixed at `/` — the whole store is
/// one flat namespace today (round-vm's own generated scripts hardcode the same "/"); a
/// second namespace is a config knob to add later, not a speculative one to carry now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    pub endpoint: String,
    pub key_prefix: String,
}

impl Store {
    /// From an already-resolved config snapshot (`gate`'s `Ctx::vars`, `aeon`'s `Conf`, a
    /// test fixture) — `get` answers exactly like that snapshot's own accessor: an absent or
    /// empty value is "not set". `None` when the operator never named a store — a build must
    /// treat that exactly like any other absence: the local-disk cache, never a refusal.
    pub fn from_values(get: impl Fn(&str) -> Option<String>) -> Option<Store> {
        let addr = get(STORE_ADDR_ENV).filter(|v| !v.trim().is_empty())?;
        let endpoint = if addr.contains("://") { addr } else { format!("http://{addr}") };
        Some(Store { endpoint, key_prefix: "/".to_string() })
    }

    /// [`Store::from_values`], resolved in-process from this binary's own environment and
    /// whichever `spira.toml` [`crate::discover`] finds (law-a-binary-resolves-the-config-it-
    /// reads) — for a caller (`testenv`, `release`) that builds no config snapshot of its own
    /// the way `gate`/`aeon` already do. `None` on an unset `SPIRA_HOME` or any resolution
    /// error, same as a genuinely unconfigured store: a build never refuses for want of this.
    pub fn from_env() -> Option<Store> {
        let addr = addr_from_env()?;
        Store::from_values(|k| (k == STORE_ADDR_ENV).then(|| addr.clone()))
    }

    fn env_pairs(&self) -> [(String, String); 2] {
        [
            ("SCCACHE_WEBDAV_ENDPOINT".into(), self.endpoint.clone()),
            ("SCCACHE_WEBDAV_KEY_PREFIX".into(), self.key_prefix.clone()),
        ]
    }
}

/// What [`ensure_store_backend`] found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendCheck {
    /// No server answered `--show-stats` (none running and none could be started, or the
    /// binary itself could not be run) — nothing to compare against; never refuses a build
    /// over a diagnostic that could not run.
    Unknown,
    /// A server answered and is already on the configured webdav store.
    Matches,
    /// A server answered on a DIFFERENT backend (almost always the local-disk default from a
    /// build that started it before `SCCACHE_WEBDAV_ENDPOINT` was ever set) and was stopped —
    /// the next cargo invocation this process's own [`Wrapper::env`]/[`Wrapper::admitted_env`]
    /// already point at the store starts a fresh one on the right backend. Carries the stale
    /// `Cache location` line that was seen, for the caller's own log line.
    Restarted(String),
}

/// `sccache --show-stats`'s `Cache location` line, verbatim (trimmed), or `None` if the
/// binary could not be run at all. Querying a server that is already running only reads its
/// socket; one that is not running is started by this call — on whatever backend ITS OWN
/// environment (this process's, via `extra_env`) names, which is exactly why callers pass the
/// configured store's own vars here rather than the bare ambient environment.
fn show_stats_cache_location(sccache_bin: &Path, extra_env: &[(String, String)]) -> Option<String> {
    let out = crate::bounded::bounded(sccache_bin).arg("--show-stats").envs(extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str()))).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.trim_start().starts_with("Cache location"))
        .map(|l| l.trim().to_string())
}

/// A `Cache location` line names the webdav backend — matched case-insensitively and by
/// substring, not by the exact phrasing sccache 0.18.0 happens to use today (`"webdav, name:
/// ..., prefix: ..."`), which is sccache's own `fmt::Display`, not this crate's contract.
fn is_webdav_location(line: &str) -> bool {
    line.to_ascii_lowercase().contains("webdav")
}

/// Before trusting a build to `store`, make sure the sccache CLIENT DAEMON already running on
/// this box (if any) is actually on it (sp-xtdqi): sccache fixes a daemon's backend at spawn
/// time and ignores every later invocation's environment once one is listening, so a server a
/// previous build (or a gate that has since exited) started before the store was configured
/// silently keeps every later build on the LOCAL DISK cache, a correctly-wired `Wrapper::env`
/// notwithstanding.
///
/// RESTARTS, NEVER REFUSES, on a mismatch. A refusal would turn a one-line, idempotent
/// self-heal into a hard stop for every build on the box until an operator happens to notice
/// and runs the same stop by hand — worse for throughput and no safer, since every caller of
/// [`Wrapper::env`]/[`Wrapper::admitted_env`] already sets `SCCACHE_IGNORE_SERVER_IO_ERROR=1`
/// for exactly a server vanishing mid-build (see `env`'s own comment): a build concurrent with
/// the stop falls back to an uncached `rustc` rather than failing. Call once per build setup
/// (inside `env`/`admitted_env` themselves, not once per `cargo` invocation) — `--show-stats`
/// always talks to, or spawns, the daemon, so this is not free.
fn ensure_store_backend(sccache_bin: &Path, store: &Store) -> BackendCheck {
    let extra = store.env_pairs();
    match show_stats_cache_location(sccache_bin, &extra) {
        None => BackendCheck::Unknown,
        Some(line) if is_webdav_location(&line) => BackendCheck::Matches,
        Some(line) => {
            let _ = crate::bounded::bounded(sccache_bin).arg("--stop-server").output();
            BackendCheck::Restarted(line)
        }
    }
}

/// [`ensure_store_backend`] plus the two `SCCACHE_WEBDAV_*` vars, in one call — the one place
/// [`Wrapper::env`]/[`Wrapper::admitted_env`] reach for either, so neither can add the vars
/// without also running the check (or vice versa).
fn sync_and_append(sccache_bin: &Path, store: &Store, v: &mut Vec<(String, String)>) {
    if let BackendCheck::Restarted(line) = ensure_store_backend(sccache_bin, store) {
        eprintln!(
            "spira_config::build: sccache server was not on the shared store ({line}) — \
             stopped it; the next build starts a fresh one on {}",
            store.endpoint
        );
    }
    v.extend(store.env_pairs());
}

impl Wrapper {
    /// The environment a cargo invocation gets. `RUSTC_WRAPPER=""` (off) also overrides a
    /// wrapper an operator's cargo config names. Never a `CARGO_*` variable: sccache hashes
    /// those into every key, so one would split the cache between callers (DESIGN §2.2).
    ///
    /// `store`: the shared compilation cache this box names, if any (sp-xtdqi) —
    /// [`Store::from_values`]/[`Store::from_env`], resolved by the caller. `None` here means
    /// exactly what an absent `SPIRA_SCCACHE_DAV_ADDR` means: no shared store, build through
    /// whatever backend the operator's own `~/.cargo/config.toml` (if any) already names.
    /// `Some` is taken only for [`Wrapper::Sccache`] — [`Wrapper::Off`] runs no wrapper at all,
    /// so a store to point it at is moot — and, before the vars are added, synchronises the
    /// box's one sccache client daemon onto it (see [`ensure_store_backend`]'s own doc for why
    /// this restarts a wrong-backend daemon rather than refusing the build).
    pub fn env(&self, store: Option<&Store>) -> Vec<(String, String)> {
        match self {
            Wrapper::Sccache(p) => {
                let mut v = vec![
                    ("RUSTC_WRAPPER".into(), p.display().to_string()),
                    // The server dies with whichever client's unit spawned it; a compile whose
                    // server vanished runs rustc locally instead of failing the build.
                    ("SCCACHE_IGNORE_SERVER_IO_ERROR".into(), "1".into()),
                ];
                if let Some(s) = store {
                    sync_and_append(p, s, &mut v);
                }
                v
            }
            Wrapper::Off => vec![("RUSTC_WRAPPER".into(), String::new())],
        }
    }

    /// The environment an AGENT's cargo gets (sp-f4ig1, gate/DESIGN-admission.md §3.3): the
    /// same compiler, fronted by `spira-admit` (`admit`, an absolute path) so every build the
    /// agent starts takes a compile slot for its cargo. The compiler this wrapper would have
    /// been is `SPIRA_ADMIT_INNER` (empty when off: rustc itself); `run` is the pools' home
    /// and `who` the lease's label. Still no `CARGO_*` variable (DESIGN §2.2). The gate and
    /// testenv never use this: they take their slots in-process and keep [`Wrapper::env`].
    /// `store`: see [`Wrapper::env`]'s own doc — same meaning, same synchronisation.
    pub fn admitted_env(&self, admit: &Path, run: &str, who: &str, store: Option<&Store>) -> Vec<(String, String)> {
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
        if let Wrapper::Sccache(p) = self {
            env.push(("SCCACHE_IGNORE_SERVER_IO_ERROR".into(), "1".into()));
            if let Some(s) = store {
                sync_and_append(p, s, &mut env);
            }
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

    fn bin_dir(with: bool) -> (testkit::TempDir, String) {
        let d = testkit::TempDir::new("spira-config-build");
        if with {
            let p = d.path().join("sccache");
            // `testkit::write_exe`, never a raw `fs::write` + `chmod` (sp-xtdqi-3): this
            // process never holds a write descriptor on the file, so another test
            // thread's own fork (any `Command::spawn` elsewhere in this binary) cannot
            // inherit one and leave a concurrent exec of THIS file seeing ETXTBSY.
            testkit::write_exe(&p, "#!/bin/sh\n");
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
        let env = wrapper(&path, None).unwrap().env(None);
        assert_eq!(env[0], ("RUSTC_WRAPPER".into(), d.path().join("sccache").display().to_string()));
        assert!(env.iter().any(|(k, v)| k == "SCCACHE_IGNORE_SERVER_IO_ERROR" && v == "1"));
        // No store configured: no webdav var at all, not even an empty one.
        assert!(env.iter().all(|(k, _)| !k.starts_with("SCCACHE_WEBDAV")), "{env:?}");
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
        assert_eq!(w.env(None), vec![("RUSTC_WRAPPER".to_string(), String::new())]);
        assert!(w.describe().contains("OFF"));
        // Off ignores a configured store too — there is no wrapper to point at it.
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        assert_eq!(w.env(Some(&store)), vec![("RUSTC_WRAPPER".to_string(), String::new())]);
    }

    #[test]
    fn an_agents_build_is_fronted_by_spira_admit_with_the_same_compiler_inside() {
        let (d, path) = bin_dir(true);
        let admit = Path::new("/rel/bin/spira-admit");
        let env = wrapper(&path, None).unwrap().admitted_env(admit, "/run/spira", "sp-abc", None);
        let get = |k: &str| env.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("RUSTC_WRAPPER").as_deref(), Some("/rel/bin/spira-admit"));
        assert_eq!(get("SPIRA_ADMIT_INNER"), Some(d.path().join("sccache").display().to_string()));
        assert_eq!(get("SPIRA_ADMIT_WHO").as_deref(), Some("sp-abc"));
        assert_eq!(get("SPIRA_RUN").as_deref(), Some("/run/spira"));
        assert_eq!(get("SCCACHE_IGNORE_SERVER_IO_ERROR").as_deref(), Some("1"));
        assert!(env.iter().all(|(k, _)| !k.starts_with("CARGO_")), "{env:?}");
        // Off: admission still fronts rustc; the inner compiler is rustc itself.
        let off = Wrapper::Off.admitted_env(admit, "/run/spira", "sp-abc", None);
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
            assert!(w.env(None).iter().all(|(k, _)| !k.starts_with("CARGO_")), "{:?}", w.env(None));
        }
        assert_eq!(one_shot("aeon"), ["--config".to_string(), "profile.aeon.incremental=false".to_string()]);
        assert_eq!(one_shot_words("release"), "--config profile.release.incremental=false");
    }

    // ------------------------------------------------------------- Store (sp-xtdqi)

    #[test]
    fn store_from_values_defaults_the_scheme_and_fixes_the_key_prefix() {
        let v: std::collections::BTreeMap<_, _> = [(STORE_ADDR_ENV.to_string(), "192.168.1.56:9431".to_string())].into();
        let s = Store::from_values(|k| v.get(k).cloned()).expect("an address was given");
        assert_eq!(s.endpoint, "http://192.168.1.56:9431");
        assert_eq!(s.key_prefix, "/");

        let v2: std::collections::BTreeMap<_, _> = [(STORE_ADDR_ENV.to_string(), "https://box:9431".to_string())].into();
        let s2 = Store::from_values(|k| v2.get(k).cloned()).unwrap();
        assert_eq!(s2.endpoint, "https://box:9431", "a scheme already present is kept, not doubled");
    }

    #[test]
    fn store_from_values_is_none_when_the_operator_never_named_a_store() {
        assert!(Store::from_values(|_| None).is_none(), "absent key");
        assert!(Store::from_values(|_| Some("   ".to_string())).is_none(), "blank value");
        assert!(Store::from_values(|_| Some(String::new())).is_none(), "empty value");
    }

    // ---------------------------------------------------- backend sync (sp-xtdqi, part c)

    /// A fake `sccache` that logs every invocation to `log` and, on `--show-stats`, prints a
    /// single `Cache location` line — enough for [`show_stats_cache_location`]/
    /// [`ensure_store_backend`] to drive without ever touching the box's real daemon.
    fn fake_sccache(dir: &Path, cache_location: &str, log: &Path) -> PathBuf {
        let p = dir.join("sccache");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"{log}\"\nif [ \"$1\" = '--show-stats' ]; then\n  printf 'Cache location                  %s\\n' \"{loc}\"\nfi\n",
            log = log.display(),
            loc = cache_location,
        );
        // `testkit::write_exe`, not a raw `fs::write` + `chmod` (sp-xtdqi-3, THE ROOT CAUSE
        // of this suite's own flip): see `bin_dir`'s comment — this is the one actually
        // exec'd by `ensure_store_backend`, so it is the one that was actually racing.
        testkit::write_exe(&p, &script);
        p
    }

    #[test]
    fn a_server_already_on_the_store_is_left_alone() {
        let d = testkit::TempDir::new("spira-config-build-backend");
        let log = d.path().join("calls.log");
        let bin = fake_sccache(d.path(), "webdav, name: , prefix: /", &log);
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        assert_eq!(ensure_store_backend(&bin, &store), BackendCheck::Matches);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!calls.contains("--stop-server"), "a matching server must not be touched: {calls:?}");
    }

    /// THE POSITIVE CONTROL for part (c): a server on the wrong backend (the shape
    /// `sccache --show-stats` prints for the local-disk default) is STOPPED, never silently
    /// trusted and never a build refusal.
    #[test]
    fn a_server_on_the_wrong_backend_is_stopped_not_silently_used_and_not_refused() {
        let d = testkit::TempDir::new("spira-config-build-backend");
        let log = d.path().join("calls.log");
        let bin = fake_sccache(d.path(), "Local disk: \"/tmp/wrong\"", &log);
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let check = ensure_store_backend(&bin, &store);
        assert!(matches!(check, BackendCheck::Restarted(ref l) if l.contains("Local disk")), "{check:?}");
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("--stop-server"), "the wrong-backend server must be stopped: {calls:?}");
    }

    #[test]
    fn a_sccache_binary_that_cannot_run_is_unknown_never_a_refusal() {
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let check = ensure_store_backend(Path::new("/nonexistent-sp-xtdqi/sccache"), &store);
        assert_eq!(check, BackendCheck::Unknown);
    }

    /// THE POSITIVE CONTROL for part (b): `Wrapper::env`/`admitted_env` carry the store's own
    /// vars into the build, and run the backend sync as a side effect of doing so — a build
    /// configured with a store can never silently skip both.
    #[test]
    fn env_with_a_store_adds_the_webdav_vars_and_syncs_the_backend() {
        let d = testkit::TempDir::new("spira-config-build-envstore");
        let log = d.path().join("calls.log");
        let bin = fake_sccache(d.path(), "Local disk: \"/tmp/wrong\"", &log);
        let w = Wrapper::Sccache(bin);
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let env = w.env(Some(&store));
        let get = |k: &str| env.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SCCACHE_WEBDAV_ENDPOINT").as_deref(), Some("http://box:9431"));
        assert_eq!(get("SCCACHE_WEBDAV_KEY_PREFIX").as_deref(), Some("/"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("--stop-server"), "{calls:?}");
    }

    #[test]
    fn admitted_env_with_a_store_adds_the_webdav_vars() {
        let d = testkit::TempDir::new("spira-config-build-admitstore");
        let log = d.path().join("calls.log");
        let bin = fake_sccache(d.path(), "webdav, name: , prefix: /", &log);
        let w = Wrapper::Sccache(bin);
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let env = w.admitted_env(Path::new("/rel/bin/spira-admit"), "/run/spira", "sp-abc", Some(&store));
        let get = |k: &str| env.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SCCACHE_WEBDAV_ENDPOINT").as_deref(), Some("http://box:9431"));
        assert_eq!(get("SCCACHE_WEBDAV_KEY_PREFIX").as_deref(), Some("/"));
    }

    #[test]
    fn off_and_no_store_add_no_webdav_vars() {
        let (_d, path) = bin_dir(true);
        let w = wrapper(&path, None).unwrap();
        assert!(w.env(None).iter().all(|(k, _)| !k.starts_with("SCCACHE_WEBDAV")));
        assert!(Wrapper::Off.admitted_env(Path::new("/x"), "/run", "sp-abc", None).iter().all(|(k, _)| !k.starts_with("SCCACHE_WEBDAV")));
    }
}

#[cfg(test)]
mod from_env_tests {
    use super::*;

    fn with_env<R>(pairs: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _l = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = testkit::env(pairs);
        f()
    }

    #[test]
    fn the_store_resolves_from_spira_toml_with_no_addr_in_the_environment() {
        let d = testkit::TempDir::new("spira-config-store-from-toml");
        let home = d.path().join("repo/spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        let toml = crate::fixture_toml_file(d.path(), &[("SPIRA_SCCACHE_DAV_ADDR".to_string(), "10.9.8.7:9431".to_string())].into_iter().collect());
        let got = with_env(
            &[
                ("SPIRA_HOME", Some(home.to_str().unwrap())),
                ("SPIRA_REPO", Some(d.path().join("repo").to_str().unwrap())),
                ("SPIRA_TOML", Some(toml.to_str().unwrap())),
                (STORE_ADDR_ENV, None),
            ],
            Store::from_env,
        );
        assert_eq!(got.map(|s| s.endpoint), Some("http://10.9.8.7:9431".to_string()));
    }

    #[test]
    fn addr_for_home_resolves_from_spira_toml_when_spira_home_is_unset() {
        let d = testkit::TempDir::new("spira-config-addr-for-home");
        let home = d.path().join("repo/spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        let toml = crate::fixture_toml_file(d.path(), &[("SPIRA_SCCACHE_DAV_ADDR".to_string(), "10.9.8.7:9431".to_string())].into_iter().collect());
        let got = with_env(
            &[("SPIRA_HOME", None), ("SPIRA_REPO", Some(d.path().join("repo").to_str().unwrap())), ("SPIRA_TOML", Some(toml.to_str().unwrap())), (STORE_ADDR_ENV, None)],
            || addr_for_home(&home),
        );
        assert_eq!(got.as_deref(), Some("10.9.8.7:9431"));
        let unset = with_env(&[("SPIRA_TOML", Some("/nonexistent-sp-ei6jt/spira.toml")), (STORE_ADDR_ENV, None)], || addr_for_home(&home));
        assert_eq!(unset, None, "an unconfigured store stays None");
    }
}
