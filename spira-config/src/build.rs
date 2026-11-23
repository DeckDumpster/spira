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
//! bare environment read (law-a-binary-resolves-the-config-it-reads) — and [`Wrapper::env`] carries it (`SCCACHE_WEBDAV_ENDPOINT`/`SCCACHE_WEBDAV_KEY_PREFIX`)
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

/// The conf.d key naming the one cargo home every host cargo runs under.
pub const CARGO_HOME_KEY: &str = "SPIRA_CARGO_HOME";

/// `CARGO_HOME=<value>` for a configured cargo home, nothing when the key is empty. Every site
/// that starts a cargo appends this, so sccache sees one registry path whichever site built.
pub fn cargo_home_env(get: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    get(CARGO_HOME_KEY)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| vec![("CARGO_HOME".to_string(), v)])
        .unwrap_or_default()
}

/// [`cargo_home_env`] for a binary that builds no config snapshot of its own (`testenv`,
/// `release`, `queue`): resolved in-process through [`crate::process::cfg`].
pub fn cargo_home_env_from_process() -> Vec<(String, String)> {
    cargo_home_env(|k| {
        crate::process::cfg(k)
            .map_err(|e| eprintln!("spira_config::build: {k}: {e} — cargo runs under its default home"))
            .ok()
    })
}

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
    /// From an already-resolved config snapshot (`gate`'s `Ctx::vars`, `dwarf`'s `Conf`, a
    /// test fixture) — `get` answers exactly like that snapshot's own accessor: an absent or
    /// empty value is "not set". `None` when the operator never named a store — a build must
    /// treat that exactly like any other absence: the local-disk cache, never a refusal.
    pub fn from_values(get: impl Fn(&str) -> Option<String>) -> Option<Store> {
        let addr = get(STORE_ADDR_ENV).filter(|v| !v.trim().is_empty())?;
        let addr = match crate::hostaddr::resolve_hostport(&addr) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("spira_config::build: {STORE_ADDR_ENV}={addr}: {e} — building without the shared store");
                return None;
            }
        };
        let endpoint = if addr.contains("://") { addr } else { format!("http://{addr}") };
        Some(Store { endpoint, key_prefix: "/".to_string() })
    }

    /// [`Store::from_values`], resolved in-process from this binary's own environment and
    /// whichever `spira.toml` [`crate::discover`] finds (law-a-binary-resolves-the-config-it-
    /// reads) — for a caller (`testenv`, `release`) that builds no config snapshot of its own
    /// the way `gate`/`dwarf` already do. `None` on an unset `SPIRA_HOME` or any resolution
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
    /// the next cargo invocation this process's own [`Wrapper::env`]
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
/// [`Wrapper::env`] already sets `SCCACHE_IGNORE_SERVER_IO_ERROR=1`
/// for exactly a server vanishing mid-build (see `env`'s own comment): a build concurrent with
/// the stop falls back to an uncached `rustc` rather than failing. Call once per build setup
/// (inside `env` itself, not once per `cargo` invocation) — `--show-stats`
/// always talks to, or spawns, the daemon, so this is not free.
fn ensure_store_backend(sccache_bin: &Path, store: &Store) -> BackendCheck {
    let extra = store.env_pairs();
    ensure_server_unit_on_host(sccache_bin, store);
    match show_stats_cache_location(sccache_bin, &extra) {
        None => BackendCheck::Unknown,
        Some(line) if is_webdav_location(&line) => BackendCheck::Matches,
        Some(line) => {
            SystemEffectors.stop_server(sccache_bin);
            ensure_server_unit_on_host(sccache_bin, store);
            BackendCheck::Restarted(line)
        }
    }
}

/// The transient user unit the box's one sccache server runs in. A server a client spawns
/// lives in that client's cgroup (a round pass's scope, a builder's session) and keeps it
/// alive for as long as the server answers every other build.
pub const SERVER_UNIT: &str = "spira-sccache-server";

/// `systemd-run` arguments that run the sccache server in the foreground as [`SERVER_UNIT`].
pub fn server_unit_args(sccache_bin: &Path, store: &Store, tmp: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["--user", "--quiet", "--collect", &format!("--unit={SERVER_UNIT}"), "--property=Restart=always", "--property=RestartSec=2", "--setenv=SCCACHE_START_SERVER=1", "--setenv=SCCACHE_NO_DAEMON=1", "--setenv=SCCACHE_IDLE_TIMEOUT=0", &format!("--setenv=TMPDIR={}", tmp.display())].iter().map(|s| s.to_string()).collect();
    args.extend(store.env_pairs().iter().map(|(k, v)| format!("--setenv={k}={v}")));
    args.push(sccache_bin.display().to_string());
    args
}

/// What the server-unit start does to the machine, behind a seam so a unit test spawns nothing.
trait ServerEffectors {
    fn start_unit(&self, args: &[String]);
    fn unit_active(&self) -> bool;
    fn stop_server(&self, sccache_bin: &Path);
}

struct SystemEffectors;

impl ServerEffectors for SystemEffectors {
    fn start_unit(&self, args: &[String]) {
        let _ = crate::bounded::bounded(Path::new("systemd-run")).args(args).output();
    }

    fn unit_active(&self) -> bool {
        let active = || crate::bounded::bounded(Path::new("systemctl")).args(["--user", "is-active", "--quiet", SERVER_UNIT]).output().map(|o| o.status.success()).unwrap_or(false);
        // A server that cannot bind its port fails moments after the start: active must hold across polls.
        for _ in 0..3 {
            if !active() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(200)); // waits-on: the unit's state; cap 3 polls
        }
        true
    }

    fn stop_server(&self, sccache_bin: &Path) {
        let _ = crate::bounded::bounded(sccache_bin).arg("--stop-server").output();
    }
}

/// What [`ensure_server_unit`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerUnit {
    Running,
    StrayReplaced,
}

/// The folder the unit's server's temporary files live in: its own, so no session's removed
/// `TMPDIR` can break a compile. Created here, before the unit starts.
fn server_tmp_dir(fx: &dyn ServerEffectors, sccache_bin: &Path) -> Result<PathBuf, String> {
    let dir = Path::new(&crate::process::cfg("SPIRA_RUN")?).join("sccache-server").join("tmp");
    reclaim_if_root_missing(fx, sccache_bin, &dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// A running server whose scratch root is gone would lose its compile outputs: stop it so the
/// next start gets a fresh root. Returns whether it did.
fn reclaim_if_root_missing(fx: &dyn ServerEffectors, sccache_bin: &Path, root: &Path) -> bool {
    if root.exists() {
        return false;
    }
    fx.stop_server(sccache_bin);
    true
}

/// Starts [`SERVER_UNIT`]. A unit that is not active afterwards lost its port to a server some
/// client spawned (in that client's cgroup and environment): stop that one and start the unit.
fn ensure_server_unit(fx: &dyn ServerEffectors, sccache_bin: &Path, store: &Store, tmp: &Path) -> ServerUnit {
    let args = server_unit_args(sccache_bin, store, tmp);
    fx.start_unit(&args);
    if fx.unit_active() {
        return ServerUnit::Running;
    }
    fx.stop_server(sccache_bin);
    fx.start_unit(&args);
    eprintln!("spira_config::build: a sccache server outside {SERVER_UNIT} held the port — stopped it and started the unit");
    ServerUnit::StrayReplaced
}

fn ensure_server_unit_on_host(sccache_bin: &Path, store: &Store) -> Option<ServerUnit> {
    match server_tmp_dir(&SystemEffectors, sccache_bin) {
        Ok(tmp) => Some(ensure_server_unit(&SystemEffectors, sccache_bin, store, &tmp)),
        Err(e) => {
            eprintln!("spira_config::build: {SERVER_UNIT} not started: {e}");
            None
        }
    }
}

/// [`ensure_store_backend`] plus the two `SCCACHE_WEBDAV_*` vars, in one call — the one place
/// [`Wrapper::env`] reaches for either, so it cannot add the vars
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

/// The cargo settings a builder session compiles under: its jobs and test threads, from the
/// harness's own sizes ([`crate::admission`]) rather than any host cargo config. Incremental is
/// the manifest's, and whole-workspace passes opt out per call ([`one_shot`]).
pub fn session_env() -> [(String, String); 2] {
    [
        ("CARGO_BUILD_JOBS".into(), crate::admission::RUN_CARGO_JOBS.to_string()),
        ("RUST_TEST_THREADS".into(), crate::admission::RUN_TEST_THREADS.to_string()),
    ]
}

/// The keys of a cargo config file's `[build]`, `[profile]` and `[env]` that the harness
/// declares itself (`SCCACHE_*` and a wrapper other than the retired `spira-admit` serve
/// interactive shells and are left alone), each as a dotted path; a parse failure is one entry.
/// Pure: the caller reads the file and reports.
pub fn host_drift(text: &str) -> Vec<String> {
    let doc: toml::Table = match text.parse() {
        Ok(d) => d,
        Err(e) => return vec![format!("unparsable ({})", e.message())],
    };
    let mut out = Vec::new();
    if let Some(toml::Value::Table(b)) = doc.get("build") {
        flatten("build", b, &mut out);
    }
    if let Some(toml::Value::Table(p)) = doc.get("profile") {
        flatten("profile", p, &mut out);
    }
    if let Some(toml::Value::Table(e)) = doc.get("env") {
        out.extend(e.keys().filter(|k| !k.starts_with("SCCACHE_")).map(|k| format!("env.{k}")));
    }
    out.retain(|k| k != "build.rustc-wrapper");
    if let Some(toml::Value::String(w)) = doc.get("build").and_then(|b| b.get("rustc-wrapper")) {
        if Path::new(w).file_name().is_some_and(|n| n == crate::admission::BIN) {
            out.insert(0, "build.rustc-wrapper".into());
        }
    }
    out
}

fn flatten(prefix: &str, t: &toml::Table, out: &mut Vec<String>) {
    for (k, v) in t {
        match v {
            toml::Value::Table(inner) => flatten(&format!("{prefix}.{k}"), inner, out),
            _ => out.push(format!("{prefix}.{k}")),
        }
    }
}

pub const TF_PLUGIN_CACHE_ENV: &str = "TF_PLUGIN_CACHE_DIR";

/// The one Terraform provider cache of the box, under the run directory and outside every
/// worktree. Created here because terraform refuses a cache directory that does not exist.
/// The lock-file override stays unset, so `.terraform.lock.hcl` remains authoritative.
pub fn tf_plugin_cache(run: &Path) -> std::io::Result<std::path::PathBuf> {
    let dir = run.join("tf-plugin-cache");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Whether `p` (or its nearest existing ancestor) is on a tmpfs.
pub fn on_tmpfs(p: &Path) -> bool {
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    let mut cur = Some(p);
    while let Some(d) = cur {
        if d.exists() {
            let Ok(c) = std::ffi::CString::new(d.as_os_str().as_encoded_bytes()) else { return false };
            // SAFETY: statfs into a zeroed struct we own, on a NUL-terminated path.
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            let r = unsafe { libc::statfs(c.as_ptr(), &mut st) };
            #[allow(clippy::unnecessary_cast)]
            return r == 0 && st.f_type as i64 == TMPFS_MAGIC;
        }
        cur = d.parent();
    }
    false
}

/// Bytes under `p`. A symlink at `p` itself is followed (a build directory may be a link to
/// another disk); no symlink below it is.
pub fn size_of(p: &Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(md) if md.is_dir() => entries_size(p),
        _ => within(p),
    }
}

fn within(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = std::fs::symlink_metadata(p) else { return 0 };
    if !md.is_dir() {
        return md.blocks() * 512;
    }
    entries_size(p)
}

fn entries_size(p: &Path) -> u64 {
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| within(&e.path())).sum()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_CONFIG: &str = r#"
[build]
rustc-wrapper = "/home/u/.cargo/bin/spira-admit"
jobs = 6
incremental = false
[env]
RUST_TEST_THREADS = "4"
TMPDIR = "/srv/tmp"
SCCACHE_DIR = "/srv/sccache"
[profile.dev]
codegen-units = 4
[profile.test]
codegen-units = 4
"#;

    #[test_id(tks7ex8)]
    #[test]
    fn host_drift_names_every_setting_the_harness_declares() {
        let mut got = host_drift(HOST_CONFIG);
        got.sort();
        assert_eq!(
            got,
            ["build.incremental", "build.jobs", "build.rustc-wrapper", "env.RUST_TEST_THREADS", "env.TMPDIR", "profile.dev.codegen-units", "profile.test.codegen-units"]
        );
    }

    #[test_id(twmux3s)]
    #[test]
    fn host_drift_leaves_sccache_settings_and_a_live_wrapper_alone() {
        let text = "[build]\nrustc-wrapper = \"sccache\"\n[env]\nSCCACHE_DIR = \"x\"\nSCCACHE_CACHE_SIZE = \"9G\"\n";
        assert!(host_drift(text).is_empty());
        assert!(host_drift("").is_empty());
    }

    #[test_id(tke4xsw)]
    #[test]
    fn host_drift_reports_a_file_it_cannot_parse() {
        let got = host_drift("[build\njobs = ");
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("unparsable"), "{got:?}");
    }

    #[test_id(tpkr2ex)]
    #[test]
    fn session_env_is_the_harness_jobs_and_test_threads() {
        assert_eq!(
            session_env(),
            [("CARGO_BUILD_JOBS".to_string(), "6".to_string()), ("RUST_TEST_THREADS".to_string(), "4".to_string())]
        );
    }

    #[test_id(trfsicp)]
    #[test]
    fn the_root_manifest_compiles_dev_incrementally() {
        let m: toml::Table = include_str!("../../Cargo.toml").parse().unwrap();
        assert_eq!(m["profile"]["dev"]["incremental"].as_bool(), Some(true));
    }

    #[test_id(to4tvfa)]
    #[test]
    fn cargo_home_env_carries_a_set_key_and_nothing_for_an_empty_one() {
        let set = cargo_home_env(|k| (k == "SPIRA_CARGO_HOME").then(|| "vol/cargo".to_string()));
        assert_eq!(set, vec![("CARGO_HOME".to_string(), "vol/cargo".to_string())]);
        assert!(cargo_home_env(|_| Some(String::new())).is_empty());
        assert!(cargo_home_env(|_| None).is_empty());
    }

    fn fill(p: &Path, kib: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; kib * 1024]).unwrap();
    }

    #[test_id(tfx4lxt)]
    #[test]
    fn size_counts_blocks_so_a_sparse_file_holds_nothing() {
        let d = testkit::TempDir::new("warm-sparse");
        let sparse = d.path().join("sparse");
        std::fs::File::create(&sparse).unwrap().set_len(512 * 1024).unwrap();
        let dense = d.path().join("dense");
        fill(&dense, 512);
        assert_eq!(std::fs::metadata(&sparse).unwrap().len(), 512 * 1024);
        assert!(size_of(&sparse) < 512 * 1024, "length is not usage: unallocated blocks do not count");
        assert!(size_of(&dense) >= 512 * 1024, "allocated blocks count");
    }

    #[test_id(tl4w7pt)]
    #[test]
    fn size_follows_a_link_at_the_root_but_not_below_it() {
        let d = testkit::TempDir::new("size-link");
        let real = d.path().join("real");
        fill(&real.join("x"), 512);
        let elsewhere = d.path().join("elsewhere");
        fill(&elsewhere.join("big"), 512);
        std::os::unix::fs::symlink(&elsewhere, real.join("inner")).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(size_of(&link) >= 512 * 1024, "a linked build directory holds what its target holds");
        assert!(size_of(&link) < 1024 * 1024, "a link below the root is not followed");
        assert_eq!(size_of(&link), size_of(&real));
    }

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
        let path = format!("nonexistent-sp-z61hj:{}", d.path().display());
        (d, path)
    }

    #[test_id(ti5ncvf)]
    #[test]
    fn absent_sccache_is_a_refusal_naming_deps_and_the_opt_out() {
        let (_d, path) = bin_dir(false);
        let e = wrapper(&path, None).unwrap_err();
        assert!(e.contains("sccache is not on the build's PATH"), "{e}");
        assert!(e.contains("spira/deps.toml") && e.contains("SPIRA_BUILD_CACHE=off"), "{e}");
        // A relative PATH entry is never searched.
        assert!(wrapper("relative/dir", None).is_err());
    }

    #[test_id(tehp5we)]
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

    #[test_id(tu9skdk)]
    #[test]
    fn the_shared_server_runs_as_its_own_foreground_unit_on_the_configured_store() {
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let args = server_unit_args(Path::new("opt/bin/sccache"), &store, Path::new("run/tmp"));
        assert!(args.contains(&"--user".to_string()) && args.contains(&format!("--unit={SERVER_UNIT}")), "{args:?}");
        for want in ["--setenv=SCCACHE_START_SERVER=1", "--setenv=SCCACHE_NO_DAEMON=1", "--setenv=SCCACHE_WEBDAV_ENDPOINT=http://box:9431", "--setenv=SCCACHE_WEBDAV_KEY_PREFIX=/", "--setenv=TMPDIR=run/tmp", "--setenv=SCCACHE_IDLE_TIMEOUT=0", "--property=Restart=always", "--property=RestartSec=2"] {
            assert!(args.contains(&want.to_string()), "{want} in {args:?}");
        }
        assert_eq!(args.last().map(String::as_str), Some("opt/bin/sccache"), "the command comes after every option");
    }

    struct Recorder {
        calls: std::cell::RefCell<Vec<&'static str>>,
        active: bool,
    }

    impl ServerEffectors for Recorder {
        fn start_unit(&self, _: &[String]) {
            self.calls.borrow_mut().push("start");
        }
        fn unit_active(&self) -> bool {
            self.calls.borrow_mut().push("is-active");
            self.active
        }
        fn stop_server(&self, _: &Path) {
            self.calls.borrow_mut().push("stop-server");
        }
    }

    fn ensure_with(active: bool) -> (ServerUnit, Vec<&'static str>) {
        let fx = Recorder { calls: Default::default(), active };
        let store = Store { endpoint: "http://box:9431".into(), key_prefix: "/".into() };
        let r = ensure_server_unit(&fx, Path::new("opt/bin/sccache"), &store, Path::new("run/tmp"));
        (r, fx.calls.into_inner())
    }

    #[test_id(t9o4dn2)]
    #[test]
    fn an_inactive_unit_after_the_first_start_means_a_stray_holds_the_port_and_is_replaced() {
        let (r, calls) = ensure_with(false);
        assert_eq!(calls, ["start", "is-active", "stop-server", "start"]);
        assert_eq!(r, ServerUnit::StrayReplaced);
    }

    #[test_id(tuiycp7)]
    #[test]
    fn an_active_unit_is_left_alone() {
        let (r, calls) = ensure_with(true);
        assert_eq!(calls, ["start", "is-active"]);
        assert_eq!(r, ServerUnit::Running);
    }

    #[test_id(thi3wbe)]
    #[test]
    fn a_server_whose_scratch_root_is_gone_is_stopped_and_a_present_root_is_left_alone() {
        let tmp = testkit::TempDir::new("spira-config-scratch-root");
        let base = tmp.to_path_buf();
        let fx = Recorder { calls: Default::default(), active: true };
        assert!(reclaim_if_root_missing(&fx, Path::new("opt/bin/sccache"), &base.join("gone")));
        assert_eq!(fx.calls.borrow().as_slice(), ["stop-server"]);
        let fx = Recorder { calls: Default::default(), active: true };
        assert!(!reclaim_if_root_missing(&fx, Path::new("opt/bin/sccache"), &base));
        assert!(fx.calls.borrow().is_empty());
    }

    // ------------------------------------------------------------- Store (sp-xtdqi)

    #[test_id(ts44ha2)]
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

    #[test_id(t45074w)]
    #[test]
    fn store_from_values_is_none_when_the_operator_never_named_a_store() {
        assert!(Store::from_values(|_| None).is_none(), "absent key");
        assert!(Store::from_values(|_| Some("   ".to_string())).is_none(), "blank value");
        assert!(Store::from_values(|_| Some(String::new())).is_none(), "empty value");
    }

    // ---------------------------------------------------- backend sync (sp-xtdqi, part c)
}
