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

/// Registry keys: the directory holding each dwarf slot's warm `CARGO_TARGET_DIR` (a tmpfs of
/// its own), and the MiB one slot may hold.
pub const DWARF_WARM_BASE_ENV: &str = "SPIRA_DWARF_WARM_BASE";
pub const DWARF_WARM_CAP_ENV: &str = "SPIRA_DWARF_WARM_CAP_MIB";

/// Slots offered: one per dwarf name, so a free one always exists below the live-dwarf ceiling.
pub const DWARF_WARM_SLOTS: u64 = 10;

/// Tmpfs fullness at which idle slots are emptied at acquire and the watcher speaks.
pub const DWARF_WARM_FULL_PCT: u64 = 85;

pub const TF_PLUGIN_CACHE_ENV: &str = "TF_PLUGIN_CACHE_DIR";

/// The one Terraform provider cache of the box, under the run directory and outside every
/// worktree. Created here because terraform refuses a cache directory that does not exist.
/// The lock-file override stays unset, so `.terraform.lock.hcl` remains authoritative.
pub fn tf_plugin_cache(run: &Path) -> std::io::Result<std::path::PathBuf> {
    let dir = run.join("tf-plugin-cache");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Create `base` and every slot directory in it, so a session never creates them.
pub fn prepare_warm(base: &Path, slots: u64) -> std::io::Result<()> {
    for n in 1..=slots.max(1) {
        std::fs::create_dir_all(base.join(format!("slot.{n}")))?;
    }
    Ok(())
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

/// Claim the first free slot under `base` with a non-blocking exclusive `flock` that lives as
/// long as the returned file. None when every slot is busy: the session then builds in its
/// worktree's own target, never waits.
pub fn acquire_warm(base: &Path, slots: u64) -> Option<(PathBuf, std::fs::File)> {
    use std::os::unix::io::AsRawFd;
    std::fs::create_dir_all(base).ok()?;
    for n in 1..=slots.max(1) {
        let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(base.join(format!("slot.{n}.lock"))) else {
            continue;
        };
        // SAFETY: flock on a descriptor we own; the lock lives as long as `f`.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            let dir = base.join(format!("slot.{n}"));
            std::fs::create_dir_all(&dir).ok()?;
            return Some((dir, f));
        }
    }
    None
}

/// Bytes under `p`, not following symlinks.
pub fn size_of(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = std::fs::symlink_metadata(p) else { return 0 };
    if !md.is_dir() {
        return md.blocks() * 512;
    }
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

/// Empty `slot` when it holds more than `cap_mib`. True when it did. The caller holds the
/// slot's lock.
pub fn reap_over_cap(slot: &Path, cap_mib: u64) -> bool {
    let slot = &resolved(slot);
    if size_of(slot) <= cap_mib * 1024 * 1024 {
        return false;
    }
    empty_dir(slot);
    true
}

/// Empty `slot` at session start when it already holds more than half `cap_mib`: a slot that
/// full cannot absorb a build's growth under the cap. The caller holds the slot's lock.
pub fn reap_for_build(slot: &Path, cap_mib: u64) -> bool {
    reap_over_cap(slot, cap_mib / 2)
}

/// Used percent of the filesystem holding `p`; None when it cannot be read.
pub fn used_pct(p: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs into a zeroed struct we own, on a NUL-terminated path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 || st.f_blocks == 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    let (total, avail) = (st.f_blocks as u64, st.f_bavail as u64);
    Some((total - avail.min(total)) * 100 / total)
}

/// Empty every slot under `base` that no session holds and that is over `cap_mib`, or every
/// such slot whatever its size once the filesystem passes `full_pct`. Each is emptied under
/// its own lock, so a slot a session takes meanwhile is skipped. Returns the slots emptied.
pub fn reap_idle(base: &Path, slots: u64, cap_mib: u64, full_pct: u64) -> Vec<PathBuf> {
    use std::os::unix::io::AsRawFd;
    let mut out = Vec::new();
    for n in 1..=slots.max(1) {
        let dir = base.join(format!("slot.{n}"));
        if !dir.is_dir() {
            continue;
        }
        let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(base.join(format!("slot.{n}.lock"))) else {
            continue;
        };
        // SAFETY: flock on a descriptor we own; the lock lives as long as `f`.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        let real = resolved(&dir);
        let full = used_pct(base).is_some_and(|p| p >= full_pct);
        let reaped = if full && std::fs::read_dir(&real).is_ok_and(|mut rd| rd.next().is_some()) {
            empty_dir(&real);
            true
        } else {
            reap_over_cap(&dir, cap_mib)
        };
        if reaped {
            out.push(dir);
        }
    }
    out
}

fn resolved(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn empty_dir(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Hold a slot to `cap_mib` while a build runs by dropping its regenerable `incremental`
/// trees. Never anything else: emptying a slot under a running build deletes the cargo
/// fingerprints it is writing and leaves the worktree's `target/` links dangling; the full
/// reset is [`reap_over_cap`], at session start. True when the slot was over the cap.
pub fn enforce_cap(slot: &Path, cap_mib: u64) -> bool {
    let slot = &resolved(slot);
    if size_of(slot) <= cap_mib * 1024 * 1024 {
        return false;
    }
    if let Ok(profiles) = std::fs::read_dir(slot) {
        for p in profiles.flatten() {
            let _ = std::fs::remove_dir_all(p.path().join("incremental"));
        }
    }
    true
}

/// Move a running session's slot over its cap onto `disk`: the slot name becomes a symlink to
/// `disk`, so the next cargo invocation builds there and the tmpfs gets the space back. Never
/// run on a slot already spilled. True when it spilled.
pub fn spill(slot: &Path, disk: &Path, cap_mib: u64) -> bool {
    size_of(slot) > cap_mib * 1024 * 1024 && move_to_disk(slot, disk)
}

/// Spill a slot at session start when the tmpfs is already past `full_pct`: a session handed a
/// slot on a full tmpfs would otherwise fail its first build.
pub fn spill_if_full(slot: &Path, disk: &Path, full_pct: u64) -> bool {
    slot.parent().and_then(used_pct).is_some_and(|p| p >= full_pct) && move_to_disk(slot, disk)
}

pub fn is_spilled(slot: &Path) -> bool {
    std::fs::symlink_metadata(slot).is_ok_and(|m| m.file_type().is_symlink())
}

fn move_to_disk(slot: &Path, disk: &Path) -> bool {
    if is_spilled(slot) {
        return false;
    }
    let Some(name) = slot.file_name().map(|n| n.to_string_lossy().into_owned()) else { return false };
    let old = slot.with_file_name(format!("{name}.spill"));
    if std::fs::create_dir_all(disk).is_err() || std::fs::rename(slot, &old).is_err() {
        return false;
    }
    if std::os::unix::fs::symlink(disk, slot).is_err() {
        let _ = std::fs::rename(&old, slot);
        return false;
    }
    std::thread::spawn(move || {
        let _ = std::fs::remove_dir_all(old);
    });
    true
}

/// Put a real, empty slot directory back where [`spill`] left a symlink.
pub fn unspill(slot: &Path) {
    if is_spilled(slot) {
        let _ = std::fs::remove_file(slot);
        let _ = std::fs::create_dir_all(slot);
    }
}

/// Record `who` in the slot's lock file so the watcher can name the holder.
pub fn note_holder(lock: &mut std::fs::File, who: &str) {
    use std::io::Write;
    let _ = lock.set_len(0);
    let _ = lock.write_all(who.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(p: &Path, kib: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; kib * 1024]).unwrap();
    }

    #[test]
    fn a_slot_over_its_cap_sheds_only_incremental_while_a_build_runs() {
        let d = testkit::TempDir::new("warm-cap");
        let slot = d.path().join("slot.1");
        fill(&slot.join("debug/incremental/a"), 3072);
        fill(&slot.join("debug/deps/b"), 512);
        assert!(!enforce_cap(&slot, 4), "under the cap nothing is touched");
        assert!(slot.join("debug/incremental/a").exists());
        assert!(enforce_cap(&slot, 2), "over the cap is acted on");
        assert!(!slot.join("debug/incremental").exists());
        assert!(slot.join("debug/deps/b").exists(), "dropping incremental was enough: deps stay warm");
        fill(&slot.join("debug/deps/c"), 3072);
        assert!(enforce_cap(&slot, 2));
        assert!(slot.join("debug/deps/c").exists(), "still over: a running build's artifacts are never deleted");
    }

    #[test]
    fn an_unlocked_oversized_slot_is_emptied_and_a_held_one_is_not() {
        use std::os::unix::io::AsRawFd;
        let d = testkit::TempDir::new("warm-idle");
        let base = d.path();
        fill(&base.join("slot.1/debug/x"), 3072);
        fill(&base.join("slot.2/debug/y"), 3072);
        fill(&base.join("slot.3/debug/z"), 512);
        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(base.join("slot.2.lock")).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        let reaped = reap_idle(base, 3, 2, 101);
        assert_eq!(reaped, vec![base.join("slot.1")]);
        assert!(std::fs::read_dir(base.join("slot.1")).unwrap().next().is_none());
        assert!(base.join("slot.2/debug/y").exists(), "a held slot is never touched");
        assert!(base.join("slot.3/debug/z").exists(), "under the cap and the disk not full: stays warm");
        let reaped = reap_idle(base, 3, 2, 0);
        assert_eq!(reaped, vec![base.join("slot.3")], "a full disk empties every idle slot that holds anything");
    }

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

    #[test]
    fn a_slot_still_over_its_cap_spills_to_disk_and_is_restored() {
        let d = testkit::TempDir::new("warm-spill");
        let slot = d.path().join("slot.1");
        let disk = d.path().join("disk");
        fill(&slot.join("debug/deps/a"), 3072);
        assert!(!spill(&slot, &disk, 4), "under the cap nothing moves");
        assert!(spill(&slot, &disk, 2));
        fill(&slot.join("debug/deps/b"), 16);
        assert!(disk.join("debug/deps/b").exists(), "later writes land on disk");
        assert!(!spill(&slot, &disk, 0), "a spilled slot is not spilled again");
        unspill(&slot);
        assert!(slot.is_dir() && !std::fs::symlink_metadata(&slot).unwrap().file_type().is_symlink());
        assert!(std::fs::read_dir(&slot).unwrap().next().is_none());
    }

    #[test]
    fn a_slot_on_a_full_tmpfs_starts_spilled() {
        let d = testkit::TempDir::new("warm-start-full");
        let slot = d.path().join("slot.1");
        let disk = d.path().join("disk");
        std::fs::create_dir_all(&slot).unwrap();
        assert!(!spill_if_full(&slot, &disk, 101), "a filesystem below the limit leaves the slot alone");
        assert!(spill_if_full(&slot, &disk, 0));
        assert!(is_spilled(&slot));
    }

    #[test]
    fn a_slot_over_half_the_cap_starts_empty() {
        let d = testkit::TempDir::new("warm-start");
        let slot = d.path().join("slot.1");
        fill(&slot.join("debug/x"), 1024);
        assert!(!reap_for_build(&slot, 4));
        assert!(reap_for_build(&slot, 1));
        assert!(std::fs::read_dir(&slot).unwrap().next().is_none());
    }

    #[test]
    fn a_symlinked_slot_is_emptied_in_place() {
        let d = testkit::TempDir::new("warm-link");
        let real = d.path().join("real");
        fill(&real.join("debug/x"), 3072);
        let link = d.path().join("slot.1");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(reap_over_cap(&link, 1));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(std::fs::read_dir(&real).unwrap().next().is_none());
    }

    #[test]
    fn prepare_creates_every_slot() {
        let d = testkit::TempDir::new("warm-prep");
        prepare_warm(&d.path().join("base"), 3).unwrap();
        for n in 1..=3 {
            assert!(d.path().join(format!("base/slot.{n}")).is_dir());
        }
        assert!(!on_tmpfs(Path::new("/nonexistent-root-xyz/a")) || on_tmpfs(Path::new("/")), "an unmounted path takes its nearest ancestor's answer");
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
        let path = format!("/nonexistent-sp-z61hj:{}", d.path().display());
        (d, path)
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
}
