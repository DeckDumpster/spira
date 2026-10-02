//! `testenv container up|down|exec|probe|tag|image|publish` — the fixture container's
//! driver (DESIGN.md §12, sp-s0e1k; replaces spira/testenv.sh). Everything that touches the
//! host goes through [`Host`], so the orchestration is unit-tested against a fake.

use crate::settings::{self, Source};
use regex::Regex;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Baked into the image; must agree with the Containerfile (useradd's uid, the volume paths).
pub const SPIRA_USER: &str = "spirauser";
pub const SPIRA_UID: u32 = 1001;
pub const CONTAINER_CHECKOUT: &str = "/workspace";
pub const CONTAINER_CARGO: &str = "/var/spira/cargo";
/// Cargo's build output lives on the named volume, never in the host-owned bind mount.
pub const CONTAINER_CARGO_TARGET: &str = "/var/spira/cargo/target";
/// pasta's default forwards a container's loopback to the host's; `-T none` and `--no-map-gw`
/// close both, so a container never reaches a host service bound to loopback.
pub const ISOLATED_NETWORK: &str = "pasta:-T,none,--no-map-gw";
/// pasta is rootless-only; rootful podman's default bridge does not forward host loopback.
pub const ISOLATED_NETWORK_ROOTFUL: &str = "bridge";
pub const DEFAULT_NAME: &str = "spira-testenv";
/// Every container `up` starts carries it, so admission counts every caller's containers.
pub const TESTENV_LABEL: &str = "spira.testenv=1";

fn user_runtime() -> String {
    format!("/run/user/{SPIRA_UID}")
}

/// How a podman call's streams are wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// stdout captured, stderr discarded.
    Quiet,
    /// stdout discarded, stderr passed through.
    Loud,
    /// stdout sent to our stderr, stderr passed through (`podman push`).
    ToStderr,
}

/// A build running in the background.
pub trait Build {
    /// Wait up to `d`; Some(rc) once it has exited.
    fn wait(&mut self, d: Duration) -> Option<i32>;
}

pub trait Host {
    fn podman(&self, args: &[String], io: Io) -> (i32, String);
    /// `podman build <args>` with stdout+stderr into `log`.
    fn build(&self, args: &[String], log: &Path) -> Box<dyn Build + '_>;
    /// Replace this process with `podman <args>` (exec); returns only on failure.
    fn exec_replace(&self, args: &[String]) -> i32;
    fn read(&self, p: &Path) -> Option<Vec<u8>>;
    fn is_file(&self, p: &Path) -> bool;
    fn write(&self, p: &Path, s: &str);
    fn remove(&self, p: &Path);
    /// Copies `from` to `to`, executable bit included (a binary, never text); `to`'s parent
    /// is created if missing, a symlink `from` is followed, and whatever already sits at
    /// `to` is replaced. `Err` carries the reason; the caller decides whether it is fatal.
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), String>;
    /// This process's own binary's path (`std::env::current_exe`), so a sibling binary from
    /// the same build can be found by name. `None` when the OS cannot answer.
    fn current_exe(&self) -> Option<PathBuf>;
    /// Builds `spira-config` (release profile) in `repo_root` and returns its binary path,
    /// or `None` on any failure. The fallback `stage_spira_config` takes when no sibling of
    /// this process's own binary exists.
    fn build_spira_config(&self, repo_root: &Path) -> Option<PathBuf>;
    fn temp_log(&self) -> PathBuf;
    fn sleep(&self, d: Duration);
    fn now(&self) -> u64;
    fn pid(&self) -> u32;
    fn parent_pid(&self) -> u32;
    fn pid_live(&self, pid: u32) -> bool;
    fn ppid_of(&self, pid: u32) -> Option<u32>;
    /// Open inotify instances across /proc (the per-uid budget every rootless container charges).
    fn inotify_used(&self) -> u64;
    fn sysctl(&self, path: &str) -> Option<String>;
    fn free_disk(&self, p: &Path) -> String;
    fn free_mem(&self) -> String;
    fn owner_dir(&self) -> PathBuf;
    /// Each of `dir`'s immediate entries that is a symlink, resolved to its target's own
    /// absolute, canonical parent directory, each listed once (sp-e5v53-3): the extra bind
    /// mounts a checkout whose build directories are themselves symlinks elsewhere (a gate
    /// tree's `target/{aeon,release,debug,gate-tools}`, gate/src/target.rs's tmpfs root)
    /// needs, so the symlink still resolves once the checkout itself is bind-mounted into a
    /// container — nothing outside the checkout is otherwise visible in there, and a bind
    /// mount preserves a symlink as a symlink, never resolving it for the mount. Empty for a
    /// `dir` with no symlinked entries, or that does not exist — every ordinary (non-gate)
    /// worktree's `target/` is a real directory and this always reports nothing for one.
    fn symlinked_targets(&self, dir: &Path) -> Vec<PathBuf>;
    fn err(&self, line: &str);
    fn out(&self, line: &str);
}

/// What the subcommands read (DESIGN.md §12.2).
pub struct Conf {
    /// The harness root: `<root>/spira/testenv/Containerfile`. None = not found.
    pub harness: Option<PathBuf>,
    pub bd_pin: Option<PathBuf>,
    pub registry: String,
    pub max_concurrent: i64,
    pub queue_timeout: u64,
    pub queue_poll: u64,
    pub heartbeat: u64,
    pub basic_wait_ticks: u32,
    pub basic_retry_sleep: u64,
}

impl Conf {
    pub fn load(src: &Source, harness: Option<PathBuf>) -> Conf {
        let num = |env: &str, cfg: Option<&str>, d: i64| -> i64 {
            src.get(env, cfg)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d)
        };
        let bd_pin = src
            .get("SPIRA_BD_PIN", Some("spira.bd_pin"))
            .map(PathBuf::from)
            .or_else(|| {
                let root = harness.clone().unwrap_or_default();
                Some(settings::resolve_run(src, &root).join("bd-pin"))
            });
        Conf {
            bd_pin,
            registry: src
                .get("SPIRA_TESTENV_REGISTRY", Some("spira.testenv_registry"))
                .unwrap_or_default(),
            max_concurrent: num(
                "SPIRA_TESTENV_MAX_CONCURRENT",
                Some("spira.testenv_max_concurrent"),
                8,
            ),
            queue_timeout: num(
                "SPIRA_TESTENV_QUEUE_TIMEOUT",
                Some("spira.testenv_queue_timeout"),
                900,
            )
            .max(0) as u64,
            queue_poll: num(
                "SPIRA_TESTENV_QUEUE_POLL",
                Some("spira.testenv_queue_poll"),
                5,
            )
            .max(1) as u64,
            heartbeat: num("SPIRA_TESTENV_BUILD_HEARTBEAT", None, 60).max(1) as u64,
            basic_wait_ticks: num("SPIRA_TESTENV_BASIC_WAIT_TICKS", None, 20).max(0) as u32,
            basic_retry_sleep: num("SPIRA_TESTENV_BASIC_RETRY_SLEEP", None, 2).max(0) as u64,
            harness,
        }
    }

    fn spira_dir(&self) -> Option<PathBuf> {
        self.harness.as_ref().map(|h| h.join("spira"))
    }
}

/// `up`'s exit when it gave up waiting for a container slot: not a failure to boot.
pub const RC_QUEUE: i32 = 71;

/// The marker [`crate::run::Harness::locate`] and the warm refill look for.
pub const HARNESS_MARKER: &str = "spira/testenv/Containerfile";

pub const USAGE: &[&str] = &[
    "usage: testenv container up|down|exec|probe|tag|image|publish [OPTIONS]",
    "  up      [--name NAME] [--checkout PATH] [--queue-timeout SECS]",
    "  down    [--name NAME] [--volumes] [--force-foreign]",
    "  exec    [--name NAME] [--user USER] CMD ARGS...",
    "  probe   [--name NAME]",
    "  tag              # print the computed image tag (the build closure hash)",
    "  image            # acquire the image (pull if configured, else build); print its ref",
    "  publish          # push the image to SPIRA_TESTENV_REGISTRY under that tag",
];

pub struct Driver<'a> {
    pub host: &'a dyn Host,
    pub conf: &'a Conf,
}

fn s(v: &str) -> String {
    v.to_string()
}

fn sv(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// sha256sum's stdin form: `<hex>  -\n`.
fn sha256sum_line(bytes: &[u8]) -> Vec<u8> {
    format!("{}  -\n", crate::verdict::sha256_hex(bytes)).into_bytes()
}

/// The tiers doctor-check.sh's build-time check actually branches on (its case
/// statement): FATAL on "runtime", WARN (present-or-waived) on "optional"/"operator".
/// "dev" is an explicit no-op arm (what tests need to exist, not what the image build
/// verifies) and "release" hits no arm at all — it is Spira's own binaries, which testenv
/// stages into the container per run rather than the image installing them (sp-ehj2t).
/// Neither tier can change whether the build passes, so neither may change the tag.
const DEPS_TIERS_DOCTOR_CHECK_READS: &[&str] = &["runtime", "optional", "operator"];

/// Where [`Driver::stage_spira_config`] copies the binary, relative to the harness root
/// (the podman build context) — `testenv/Containerfile` `COPY`s it from exactly here.
/// Named so a missing one is a loud `COPY` failure, not a silent absence.
pub const DOCTOR_CHECK_SPIRA_CONFIG: &str = "testenv/.doctor-check-spira-config";

#[derive(serde::Deserialize)]
struct DepsManifest {
    #[serde(default)]
    dep: Vec<DepsEntry>,
}

#[derive(serde::Deserialize)]
struct DepsEntry {
    name: String,
    #[serde(default)]
    tier: Option<String>,
}

/// The build closure's share of deps.toml: `<name> <tier>\n` for every entry whose tier
/// doctor-check.sh checks, sorted by name so the text is independent of the manifest's own
/// ordering. Unset tier defaults to "optional" (conf.sh's `spira_bin_tier` does the same).
/// None when the manifest does not parse — the closure cannot be summarized, so the tag
/// refuses rather than naming an image from an unreadable one.
fn deps_closure(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let manifest: DepsManifest = toml::from_str(text).ok()?;
    let mut rows: Vec<(String, String)> = manifest
        .dep
        .into_iter()
        .map(|d| (d.name, d.tier.unwrap_or_else(|| "optional".to_string())))
        .filter(|(_, tier)| DEPS_TIERS_DOCTOR_CHECK_READS.contains(&tier.as_str()))
        .collect();
    rows.sort();
    let mut out = String::new();
    for (name, tier) in rows {
        out.push_str(&name);
        out.push(' ');
        out.push_str(&tier);
        out.push('\n');
    }
    Some(out)
}

impl Driver<'_> {
    fn err(&self, l: &str) {
        self.host.err(l)
    }

    fn need_harness(&self, what: &str) -> Option<PathBuf> {
        match self.conf.spira_dir() {
            Some(d) => Some(d),
            None => {
                self.err(&format!(
                    "testenv {what}: cannot find the harness ({HARNESS_MARKER}) above the testenv binary; set SPIRA_TESTENV_HARNESS"
                ));
                None
            }
        }
    }

    /// The build-closure hash: Containerfile (by content), the bd pin, and the slice of
    /// deps.toml the image build actually consumes — never by path, so every checkout of
    /// one commit computes one tag. None when the closure cannot be read or deps.toml
    /// cannot be summarized (D18a).
    pub fn image_tag(&self) -> Option<String> {
        let dir = self.need_harness("tag")?;
        let cf = dir.join("testenv/Containerfile");
        let deps = dir.join("deps.toml");
        let Some(cf_bytes) = self.host.read(&cf) else {
            self.err(&format!(
                "testenv tag: cannot read {} — refusing to name an image nothing can build",
                cf.display()
            ));
            return None;
        };
        let Some(deps_bytes) = self.host.read(&deps) else {
            self.err(&format!(
                "testenv tag: cannot read {} — refusing to name an image from part of its closure",
                deps.display()
            ));
            return None;
        };
        let Some(deps_text) = deps_closure(&deps_bytes) else {
            self.err(&format!(
                "testenv tag: {} does not parse as the dependency manifest — refusing to name an image from an unreadable closure",
                deps.display()
            ));
            return None;
        };
        let mut closure = sha256sum_line(&cf_bytes);
        if let Some(pin) = &self.conf.bd_pin {
            if self.host.is_file(pin) {
                closure.extend(self.host.read(pin).unwrap_or_default());
            }
        }
        closure.extend(sha256sum_line(deps_text.as_bytes()));
        Some(crate::verdict::sha256_hex(&closure)[..12].to_string())
    }

    fn image_ref(&self, tag: &str) -> String {
        format!("localhost/spira-testenv:{tag}")
    }

    fn remote_ref(&self, tag: &str) -> Option<String> {
        let reg = self.conf.registry.trim_end_matches('/');
        (!self.conf.registry.is_empty()).then(|| format!("{reg}/spira-testenv:{tag}"))
    }

    fn pq(&self, args: &[&str]) -> bool {
        self.host.podman(&sv(args), Io::Quiet).0 == 0
    }

    /// Local, else pulled from the registry and retagged, else built. The local ref.
    /// The network mode that keeps a container off the host's loopback, chosen by what
    /// podman itself reports; None when it cannot say (fail closed).
    fn isolated_network(&self) -> Option<&'static str> {
        let (rc, out) = self.host.podman(
            &sv(&["info", "--format", "{{.Host.Security.Rootless}}"]),
            Io::Quiet,
        );
        match (rc, out.trim()) {
            (0, "true") => Some(ISOLATED_NETWORK),
            (0, "false") => Some(ISOLATED_NETWORK_ROOTFUL),
            _ => None,
        }
    }

    pub fn ensure_image(&self) -> Option<String> {
        let tag = self.image_tag()?;
        let img = self.image_ref(&tag);
        if self.pq(&["image", "exists", &img]) {
            return Some(img);
        }
        if let Some(remote) = self.remote_ref(&tag) {
            self.err(&format!("testenv: trying {remote}"));
            if self.pq(&["pull", "-q", &remote]) && self.pq(&["tag", &remote, &img]) {
                self.err(&format!("testenv: pulled {remote}"));
                return Some(img);
            }
            self.err(&format!(
                "testenv: {remote} is not in the registry — building it"
            ));
        }
        self.build_image(&img).then_some(img)
    }

    /// One heartbeat: furthest STEP (multi-stage `[i/n] STEP` too, sp-dvfea), last output
    /// line, elapsed, free disk and memory.
    pub fn heartbeat(&self, start: u64, log: &str) {
        let step =
            Regex::new(r"^(\[[0-9]+/[0-9]+\] )?STEP [0-9]+/[0-9]+:.*").expect("static regex");
        let stage = log
            .lines()
            .filter(|l| step.is_match(l))
            .last()
            .map(|l| l.chars().take(100).collect::<String>())
            .unwrap_or_else(|| "(no build output yet)".into());
        let last = log
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .map(|l| l.chars().take(160).collect::<String>());
        let dir = self.conf.spira_dir().unwrap_or_default();
        self.err(&format!(
            "testenv: building — {stage} — elapsed {}s — disk {} free, memory {} free",
            self.host.now().saturating_sub(start),
            self.host.free_disk(&dir),
            self.host.free_mem()
        ));
        if let Some(l) = last {
            self.err(&format!("testenv:   last output: {l}"));
        }
    }

    /// Stages a copy of THIS process's own sibling `spira-config` binary into the build
    /// context at [`DOCTOR_CHECK_SPIRA_CONFIG`], so `testenv/Containerfile`'s doctor-check
    /// step can `COPY` it onto the image's PATH. sp-xjnzl: conf.sh now resolves
    /// configuration through a `spira-config` subprocess call even for a plain `deps.toml`
    /// read (`spira_deps_list`) — a cold image build never staged one, so every doctor-check
    /// step failed closed with "deps.toml did not load" the first time this tree tried a
    /// fresh build since that change landed. The build context is `spira/` (this module's
    /// own doc comment), which does not contain `spira-config`'s source — building it
    /// in-container is not an option — so the already-built sibling binary from THIS
    /// process's own `cargo build --workspace` is copied in instead. `None` (and a loud
    /// `self.err`) when no sibling exists: the Containerfile's `COPY` then fails the build
    /// loudly rather than running doctor-check against a silently-absent manifest reader.
    fn stage_spira_config(&self, dir: &Path) -> Option<PathBuf> {
        let sibling = self
            .host
            .current_exe()
            .and_then(|exe| exe.parent().map(|p| p.join("spira-config")))
            .filter(|p| self.host.is_file(p));
        let source = match sibling {
            Some(p) => p,
            // No sibling next to THIS invocation of testenv — a caller that built only
            // `-p testenv` (the gate's own host-side container step does; round-vm's
            // REMOTE_SCRIPT and TEMPLATE_SCRIPT build both explicitly, so this path is for
            // every OTHER caller). Build it ourselves rather than depend on every caller
            // remembering to: a doctor-check that needs a prerequisite the caller forgot is
            // the same silent-absence failure mode this whole fix exists to close.
            None => {
                let Some(repo_root) = dir.parent() else {
                    self.err("testenv: cannot find the repository root above the harness — skipping the spira-config stage for doctor-check");
                    return None;
                };
                self.err("testenv: no spira-config next to this binary — building it now (cargo build --release -p spira-config)");
                match self.host.build_spira_config(repo_root) {
                    Some(p) => p,
                    None => {
                        self.err(&format!(
                            "testenv: could not build spira-config in {} — the Containerfile's doctor-check step needs it on PATH",
                            repo_root.display()
                        ));
                        return None;
                    }
                }
            }
        };
        let dest = dir.join(DOCTOR_CHECK_SPIRA_CONFIG);
        match self.host.copy_file(&source, &dest) {
            Ok(()) => Some(dest),
            Err(why) => {
                self.err(&format!("testenv: could not stage spira-config into the build context: {why}"));
                None
            }
        }
    }

    fn build_image(&self, img: &str) -> bool {
        let Some(dir) = self.need_harness("image") else {
            return false;
        };
        let hb = self.conf.heartbeat;
        self.err(&format!(
            "testenv: COLD build of {img} — the tag is a hash of testenv/Containerfile, the bd pin"
        ));
        self.err("testenv: and deps.toml, so this VM has nothing to pull and must build it. Cold");
        self.err("testenv: builds compile a Go and a Rust toolchain from source and have taken");
        self.err(&format!(
            "testenv: close to twenty minutes; a heartbeat line follows at least every {hb}s —"
        ));
        self.err("testenv: silence past that means stuck, not slow.");
        let Some(staged_spira_config) = self.stage_spira_config(&dir) else {
            // Both the sibling check and the on-demand build (stage_spira_config's own
            // fallback) failed: refuse now, with the reason already on stderr, rather than
            // run a podman build the Containerfile's own COPY step would only fail deep
            // inside, confusingly.
            self.err("testenv: refusing the image build — no spira-config to stage for doctor-check (see above)");
            return false;
        };
        let log = self.host.temp_log();
        let start = self.host.now();
        let args = vec![
            s("build"),
            s("-t"),
            s(img),
            s("-f"),
            dir.join("testenv/Containerfile").display().to_string(),
            dir.display().to_string(),
        ];
        let mut b = self.host.build(&args, &log);
        let read_log =
            || String::from_utf8_lossy(&self.host.read(&log).unwrap_or_default()).into_owned();
        let rc = loop {
            if let Some(rc) = b.wait(Duration::from_secs(hb)) {
                break rc;
            }
            self.heartbeat(start, &read_log());
        };
        let text = read_log();
        self.host.remove(&log);
        // Staged only to cross into the build context (sp-xjnzl's doctor-check fix below);
        // never left behind in the checkout either way the build went.
        self.host.remove(&staged_spira_config);
        if rc == 0 {
            return true;
        }
        let lower = text.to_lowercase();
        if lower.contains("no space left on device") || text.contains("ENOSPC") {
            self.err(&format!(
                "testenv: image build failed — disk exhausted on this VM ({} free)",
                self.host.free_disk(&dir)
            ));
        } else {
            self.err(&format!(
                "testenv: image build failed — check Containerfile in {}",
                dir.join("testenv").display()
            ));
            let lines: Vec<&str> = text.lines().collect();
            for l in &lines[lines.len().saturating_sub(40)..] {
                self.err(l);
            }
        }
        false
    }

    pub fn cmd_tag(&self) -> i32 {
        match self.image_tag() {
            Some(t) => {
                self.host.out(&t);
                0
            }
            None => 1,
        }
    }

    pub fn cmd_image(&self) -> i32 {
        match self.ensure_image() {
            Some(i) => {
                self.host.out(&i);
                0
            }
            None => 1,
        }
    }

    /// Push under the closure hash; nothing floating is ever pushed.
    pub fn cmd_publish(&self) -> i32 {
        if self.conf.registry.is_empty() {
            self.err("testenv publish: SPIRA_TESTENV_REGISTRY is unset — nowhere to publish to");
            return 1;
        }
        let Some(img) = self.ensure_image() else {
            return 1;
        };
        let tag = img.rsplit(':').next().unwrap_or_default().to_string();
        let remote = self.remote_ref(&tag).unwrap_or_default();
        if !self.pq(&["tag", &img, &remote]) {
            self.err(&format!("testenv publish: could not tag {img} as {remote}"));
            return 1;
        }
        self.err(&format!("testenv: pushing {remote}"));
        if self.host.podman(&sv(&["push", &remote]), Io::ToStderr).0 != 0 {
            self.err("testenv publish: push failed");
            return 1;
        }
        self.host.out(&remote);
        0
    }

    fn owner_file(&self, name: &str) -> PathBuf {
        self.host.owner_dir().join(format!("{name}.owner"))
    }

    /// True when `target` is this process or an ancestor of it.
    fn caller_or_ancestor(&self, target: u32) -> bool {
        let mut pid = self.host.pid();
        let mut hops = 0;
        while pid != 0 && hops < 4096 {
            if pid == target {
                return true;
            }
            pid = self.host.ppid_of(pid).unwrap_or(0);
            hops += 1;
        }
        false
    }

    fn running_count(&self) -> Option<usize> {
        let (rc, out) = self.host.podman(
            &sv(&["ps", "-q", "--filter", &format!("label={TESTENV_LABEL}")]),
            Io::Quiet,
        );
        (rc == 0).then(|| out.lines().filter(|l| !l.trim().is_empty()).count())
    }

    /// Block until fewer than `max_concurrent` testenv containers run (0 disables), for at
    /// most `bound` seconds. Every wait says its length as `testenv: queue-wait=<n>s
    /// pool=container`, the line the gate sums into its queue field.
    pub fn admit(&self, bound: u64) -> bool {
        let max = self.conf.max_concurrent;
        if max <= 0 {
            return true;
        }
        let max = max as usize;
        let mut n = self.running_count().unwrap_or(0);
        if n < max {
            return true;
        }
        self.err(&format!(
            "testenv: {n} testenv containers already running (limit {max}) — queueing"
        ));
        let mut waited = 0;
        while n >= max {
            if waited >= bound {
                self.err(&format!(
                    "testenv: gave up waiting for a slot after {bound}s (still {n}/{max} running)"
                ));
                self.err(&format!("testenv: queue-wait={waited}s pool=container"));
                return false;
            }
            self.host.sleep(Duration::from_secs(self.conf.queue_poll));
            waited += self.conf.queue_poll;
            n = self.running_count().unwrap_or(0);
        }
        self.err(&format!(
            "testenv: slot free ({n}/{max} running) — starting"
        ));
        self.err(&format!("testenv: queue-wait={waited}s pool=container"));
        true
    }

    fn wait_for(&self, ticks: u32, args: &[&str]) -> bool {
        for _ in 0..ticks {
            if self.pq(args) {
                return true;
            }
            self.host.sleep(Duration::from_millis(500));
        }
        false
    }

    fn exec_out(&self, name: &str, argv: &[&str]) -> Option<String> {
        let mut a = vec!["exec", name];
        a.extend_from_slice(argv);
        let (rc, out) = self.host.podman(&sv(&a), Io::Quiet);
        (rc == 0).then_some(out)
    }

    fn inotify_pressure(&self) -> (String, String) {
        let used = self.host.inotify_used();
        let a = self
            .host
            .sysctl("/proc/sys/fs/inotify/max_user_instances")
            .and_then(|v| v.trim().parse::<u64>().ok());
        let b = self
            .host
            .sysctl("/proc/sys/user/max_inotify_instances")
            .and_then(|v| v.trim().parse::<u64>().ok());
        let max = match (a, b) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => 0,
        };
        (used.to_string(), max.to_string())
    }

    fn pair(cur: Option<String>, max: Option<String>, unbounded: &str) -> (String, String) {
        match (cur, max) {
            (Some(c), Some(m)) if !c.is_empty() && !m.is_empty() && m != unbounded => (c, m),
            _ => ("?".into(), "?".into()),
        }
    }

    fn pids_pressure(&self, name: &str) -> (String, String) {
        let out = self.exec_out(
            name,
            &[
                "cat",
                "/sys/fs/cgroup/pids.current",
                "/sys/fs/cgroup/pids.max",
            ],
        );
        let Some(out) = out else {
            return ("?".into(), "?".into());
        };
        let mut l = out.lines();
        Self::pair(
            l.next().map(|v| v.trim().to_string()),
            l.next().map(|v| v.trim().to_string()),
            "max",
        )
    }

    fn slice_pressure(&self, name: &str) -> (String, String) {
        let slice = format!("user-{SPIRA_UID}.slice");
        let Some(out) = self.exec_out(
            name,
            &[
                "systemctl",
                "show",
                &slice,
                "-p",
                "TasksCurrent",
                "-p",
                "TasksMax",
            ],
        ) else {
            return ("?".into(), "?".into());
        };
        let field = |k: &str| {
            out.lines()
                .find_map(|l| l.strip_prefix(k))
                .map(|v| v.trim().to_string())
        };
        Self::pair(field("TasksCurrent="), field("TasksMax="), "infinity")
    }

    fn keyring_pressure(&self, name: &str) -> (String, String) {
        let cur = self
            .exec_out(name, &["sh", "-c", "wc -l < /proc/keys"])
            .map(|v| v.trim().to_string());
        let max = self
            .exec_out(name, &["cat", "/proc/sys/kernel/keys/maxkeys"])
            .map(|v| v.trim().to_string());
        Self::pair(cur, max, "\u{0}")
    }

    /// Measure all four candidates and name the one closest to its cap (sp-cvle7).
    pub fn diagnose_boot_failure(&self, name: &str) {
        let (iu, im) = self.inotify_pressure();
        let (pu, pm) = self.pids_pressure(name);
        let (tu, tm) = self.slice_pressure(name);
        let (ku, km) = self.keyring_pressure(name);
        self.err(&format!(
            "testenv: resource check for {name} — inotify instances {iu}/{im}, pids {pu}/{pm}, user-{SPIRA_UID}.slice tasks {tu}/{tm}, keyring {ku}/{km}"
        ));
        let mut best: Option<(u64, String)> = None;
        for (n, u, m) in [
            ("inotify", &iu, &im),
            ("pids", &pu, &pm),
            ("user-slice-tasks", &tu, &tm),
            ("keyring", &ku, &km),
        ] {
            let (Ok(uv), Ok(mv)) = (u.parse::<u64>(), m.parse::<u64>()) else {
                continue;
            };
            if mv == 0 {
                continue;
            }
            let pct = uv * 100 / mv;
            if best.as_ref().is_none_or(|(b, _)| pct > *b) {
                best = Some((pct, format!("{n} ({u}/{m}, {pct}%)")));
            }
        }
        match best {
            Some((pct, label)) if pct >= 90 => {
                self.err(&format!("testenv: exhausted resource for {name} — {label}"))
            }
            Some((_, label)) => self.err(&format!(
                "testenv: no candidate resource for {name} is near its cap (closest: {label}) — inconclusive"
            )),
            None => self.err(&format!(
                "testenv: could not measure any candidate resource for {name} — container unreachable"
            )),
        }
    }

    pub fn cmd_up(&self, args: &[String]) -> i32 {
        let mut name = s(DEFAULT_NAME);
        let mut checkout: Option<String> = None;
        let mut queue_bound = self.conf.queue_timeout;
        let mut i = 0;
        while i < args.len() {
            match (args[i].as_str(), args.get(i + 1)) {
                ("--queue-timeout", Some(v)) => match v.parse() {
                    Ok(n) => queue_bound = n,
                    Err(_) => {
                        self.err(&format!("testenv up: --queue-timeout is not a number: {v}"));
                        return 1;
                    }
                },
                ("--name", Some(v)) => name = v.clone(),
                ("--checkout", Some(v)) => checkout = Some(v.clone()),
                (a, _) => {
                    self.err(&format!("testenv up: unknown argument: {a}"));
                    return 1;
                }
            }
            i += 2;
        }
        let checkout = match checkout
            .or_else(|| self.conf.harness.as_ref().map(|h| h.display().to_string()))
        {
            Some(c) => c,
            None => {
                self.need_harness("up");
                return 1;
            }
        };
        if self.pq(&["container", "exists", &name]) {
            self.err(&format!("testenv: {name} already exists; nothing to do"));
            return 0;
        }
        let Some(img) = self.ensure_image() else {
            return 1;
        };
        if !self.admit(queue_bound) {
            return RC_QUEUE;
        }
        // sp-e5v53-3: a checkout whose build directories are themselves symlinks elsewhere
        // (a gate tree's `target/{aeon,release,debug,gate-tools}`, gate/src/target.rs's
        // tmpfs root) needs that elsewhere mounted too — the bind mount below preserves the
        // symlink as a symlink, and nothing outside the checkout is otherwise visible once
        // inside the container, so a suite built through it "was not built" from in here.
        // Empty, and this is one volume line fewer, for every ordinary (non-gate) worktree.
        let target_dir = Path::new(&checkout).join("target");
        let extra_mounts: Vec<String> = self
            .host
            .symlinked_targets(&target_dir)
            .into_iter()
            .flat_map(|p| [s("--volume"), format!("{}:{}:z", p.display(), p.display())])
            .collect();
        let Some(network) = self.isolated_network() else {
            self.err("testenv: cannot tell whether podman is rootless; refusing to start without network isolation");
            return 1;
        };
        // pids-limit 8192: 52 parallel suites exhausted podman's rootless default of 2048.
        let run: Vec<String> = [
            s("run"),
            s("-d"),
            s("--name"),
            name.clone(),
            s("--systemd=true"),
            s("--pids-limit"),
            s("8192"),
            s("--network"),
            s(network),
            s("--label"),
            s(TESTENV_LABEL),
            s("--volume"),
            format!("{checkout}:{CONTAINER_CHECKOUT}:z"),
        ]
        .into_iter()
        .chain(extra_mounts)
        .chain([
            s("--volume"),
            format!("{name}-cargo-reg:{CONTAINER_CARGO}/registry"),
            s("--volume"),
            format!("{name}-cargo-git:{CONTAINER_CARGO}/git"),
            img,
        ])
        .collect();
        if self.host.podman(&run, Io::Loud).0 != 0 {
            self.err(&format!("testenv: podman run {name} failed"));
            return 1;
        }
        // First writer wins: a caller (the runner) may already have claimed it.
        let owner = self.owner_file(&name);
        if !self.host.is_file(&owner) {
            self.host
                .write(&owner, &format!("{}\n", self.host.parent_pid()));
        }
        let mut attempt = 0;
        while !self.wait_for(
            self.conf.basic_wait_ticks,
            &["exec", &name, "systemctl", "is-active", "basic.target"],
        ) {
            if attempt < 1 {
                attempt += 1;
                self.err(&format!(
                    "testenv: systemd basic.target startup attempt {attempt} failed; retrying after {}s",
                    self.conf.basic_retry_sleep
                ));
                self.host
                    .sleep(Duration::from_secs(self.conf.basic_retry_sleep));
                continue;
            }
            self.err(&format!(
                "testenv: system systemd did not reach basic.target after {} attempt(s)",
                attempt + 1
            ));
            self.diagnose_boot_failure(&name);
            self.pq(&["stop", &name]);
            self.pq(&["rm", &name]);
            return 1;
        }
        self.pq(&["exec", &name, "loginctl", "enable-linger", SPIRA_USER]);
        self.pq(&[
            "exec",
            &name,
            "git",
            "config",
            "--system",
            "--add",
            "safe.directory",
            CONTAINER_CHECKOUT,
        ]);
        // The cargo volumes' mountpoints are created root-owned; chown once, when wrong (D21).
        let uid = SPIRA_UID.to_string();
        self.pq(&[
            "exec",
            &name,
            "bash",
            "-c",
            "mkdir -p \"$1/target\" 2>/dev/null\n    for d in \"$1\" \"$1/registry\" \"$1/git\" \"$1/target\"; do\n        [ -d \"$d\" ] || continue\n        [ \"$(stat -c %u \"$d\" 2>/dev/null)\" = \"$2\" ] || chown -R \"$2:$2\" \"$d\"\n    done",
            "_",
            CONTAINER_CARGO,
            &uid,
        ]);
        let unit = format!("user@{SPIRA_UID}.service");
        if self.wait_for(20, &["exec", &name, "systemctl", "is-active", &unit]) {
            self.err(&format!(
                "testenv: user@{SPIRA_UID}.service active; systemctl --user ready"
            ));
        } else {
            self.err(&format!(
                "testenv: WARNING user@{SPIRA_UID}.service did not start"
            ));
            self.err(
                "testenv: probe exits non-zero; use SPIRA_SYSTEMCTL stub for systemctl --user",
            );
        }
        0
    }

    pub fn cmd_down(&self, args: &[String]) -> i32 {
        let mut name = s(DEFAULT_NAME);
        let (mut volumes, mut force) = (false, false);
        let mut i = 0;
        while i < args.len() {
            match (args[i].as_str(), args.get(i + 1)) {
                ("--name", Some(v)) => {
                    name = v.clone();
                    i += 1;
                }
                ("--volumes", _) => volumes = true,
                ("--force-foreign", _) => force = true,
                (a, _) => {
                    self.err(&format!("testenv down: unknown argument: {a}"));
                    return 1;
                }
            }
            i += 1;
        }
        let owner = self.owner_file(&name);
        if !force && self.host.is_file(&owner) {
            let text =
                String::from_utf8_lossy(&self.host.read(&owner).unwrap_or_default()).into_owned();
            if let Ok(pid) = text.trim().parse::<u32>() {
                if pid != 0 && self.host.pid_live(pid) && !self.caller_or_ancestor(pid) {
                    self.err(&format!(
                        "testenv down: {name} is owned by pid {pid} (not this process or an ancestor of it)"
                    ));
                    self.err(
                        "testenv down: refusing — pass --force-foreign to tear it down anyway",
                    );
                    return 1;
                }
            }
        }
        self.pq(&["stop", &name]);
        self.pq(&["rm", &name]);
        // A failed teardown keeps the owner file, so an orphan sweep can still find both.
        if !self.pq(&["container", "exists", &name]) {
            self.host.remove(&owner);
        }
        if volumes {
            self.pq(&["volume", "rm", &format!("{name}-cargo-reg")]);
            self.pq(&["volume", "rm", &format!("{name}-cargo-git")]);
        }
        0
    }

    pub fn exec_args(name: &str, user: &str, cmd: &[String]) -> Vec<String> {
        let mut a = vec![s("exec"), s("--user"), s(user)];
        if user == SPIRA_USER {
            let rt = user_runtime();
            for kv in [
                format!("XDG_RUNTIME_DIR={rt}"),
                format!("DBUS_SESSION_BUS_ADDRESS=unix:path={rt}/bus"),
                format!("CARGO_HOME={CONTAINER_CARGO}"),
                format!("CARGO_TARGET_DIR={CONTAINER_CARGO_TARGET}"),
            ] {
                a.push(s("-e"));
                a.push(kv);
            }
        }
        a.push(s(name));
        a.extend(cmd.iter().cloned());
        a
    }

    pub fn cmd_exec(&self, args: &[String]) -> i32 {
        let mut name = s(DEFAULT_NAME);
        let mut user = s("root");
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--name" if i + 1 < args.len() => {
                    name = args[i + 1].clone();
                    i += 2;
                }
                "--user" if i + 1 < args.len() => {
                    user = args[i + 1].clone();
                    i += 2;
                }
                "--" => {
                    i += 1;
                    break;
                }
                a if a.starts_with('-') => {
                    self.err(&format!("testenv exec: unknown option: {a}"));
                    return 1;
                }
                _ => break,
            }
        }
        let cmd = &args[i..];
        if cmd.is_empty() {
            self.err("testenv exec: command required");
            return 1;
        }
        self.host.exec_replace(&Self::exec_args(&name, &user, cmd))
    }

    pub fn cmd_probe(&self, args: &[String]) -> i32 {
        let mut name = s(DEFAULT_NAME);
        let mut i = 0;
        while i < args.len() {
            match (args[i].as_str(), args.get(i + 1)) {
                ("--name", Some(v)) => name = v.clone(),
                (a, _) => {
                    self.err(&format!("testenv probe: unknown argument: {a}"));
                    return 1;
                }
            }
            i += 2;
        }
        let unit = format!("user@{SPIRA_UID}.service");
        let rt = format!("XDG_RUNTIME_DIR={}", user_runtime());
        let ok = self.pq(&["exec", &name, "systemctl", "is-active", &unit])
            && self.pq(&[
                "exec",
                "--user",
                SPIRA_USER,
                "-e",
                &rt,
                &name,
                "systemctl",
                "--user",
                "is-active",
                "default.target",
            ]);
        i32::from(!ok)
    }

    pub fn dispatch(&self, args: &[String]) -> i32 {
        let rest = args.get(1..).unwrap_or(&[]);
        match args.first().map(String::as_str) {
            Some("up") => self.cmd_up(rest),
            Some("down") => self.cmd_down(rest),
            Some("exec") => self.cmd_exec(rest),
            Some("probe") => self.cmd_probe(rest),
            Some("tag") => self.cmd_tag(),
            Some("image") => self.cmd_image(),
            Some("publish") => self.cmd_publish(),
            _ => {
                for l in USAGE {
                    self.err(l);
                }
                1
            }
        }
    }
}

/// The real host.
pub struct RealHost;

struct RealBuild(std::process::Child);

impl Build for RealBuild {
    fn wait(&mut self, d: Duration) -> Option<i32> {
        let end = std::time::Instant::now() + d;
        loop {
            match self.0.try_wait() {
                Ok(Some(st)) => return Some(st.code().unwrap_or(1)),
                Ok(None) => {}
                Err(_) => return Some(1),
            }
            if std::time::Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

struct FailedBuild;

impl Build for FailedBuild {
    fn wait(&mut self, _: Duration) -> Option<i32> {
        Some(127)
    }
}

/// Bytes as `df -h`/`free -h` print them: one decimal under 10, binary units.
pub fn human(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", units[u])
    } else {
        format!("{v:.0}{}", units[u])
    }
}

impl Host for RealHost {
    fn podman(&self, args: &[String], io: Io) -> (i32, String) {
        use std::process::{Command, Stdio};
        let mut c = Command::new("podman");
        c.args(args).stdin(Stdio::null());
        // `podman run`/`up` starts conmon, which daemonizes and outlives this call; it must
        // never inherit a caller's lock fd this process did not open itself (sp-ohwg7).
        crate::util::close_inherited_fds(&mut c);
        match io {
            Io::Quiet => {
                c.stderr(Stdio::null());
                match c.output() {
                    Ok(o) => (
                        o.status.code().unwrap_or(1),
                        String::from_utf8_lossy(&o.stdout).into_owned(),
                    ),
                    Err(_) => (127, String::new()),
                }
            }
            Io::Loud => {
                c.stdout(Stdio::null());
                (
                    c.status().map(|s| s.code().unwrap_or(1)).unwrap_or(127),
                    String::new(),
                )
            }
            Io::ToStderr => {
                let err = std::io::stderr()
                    .as_fd()
                    .try_clone_to_owned()
                    .map(Stdio::from);
                c.stdout(err.unwrap_or(Stdio::null()));
                (
                    c.status().map(|s| s.code().unwrap_or(1)).unwrap_or(127),
                    String::new(),
                )
            }
        }
    }

    fn build(&self, args: &[String], log: &Path) -> Box<dyn Build + '_> {
        use std::process::{Command, Stdio};
        let Ok(f) = std::fs::File::create(log) else {
            return Box::new(FailedBuild);
        };
        let Ok(g) = f.try_clone() else {
            return Box::new(FailedBuild);
        };
        let mut c = Command::new("podman");
        c.args(args).stdin(Stdio::null()).stdout(f).stderr(g);
        crate::util::close_inherited_fds(&mut c);
        match c.spawn() {
            Ok(c) => Box::new(RealBuild(c)),
            Err(_) => Box::new(FailedBuild),
        }
    }

    fn exec_replace(&self, args: &[String]) -> i32 {
        let mut c = std::process::Command::new("podman");
        c.args(args);
        // A true exec, not a fork: pre_exec still runs, in this process, right before it —
        // the one chance to drop any inherited lock fd before podman (and conmon, if this
        // is a `run`) takes over this process's image (sp-ohwg7).
        crate::util::close_inherited_fds(&mut c);
        use std::os::unix::process::CommandExt;
        let e = c.exec();
        eprintln!("testenv exec: podman: {e}");
        127
    }

    fn read(&self, p: &Path) -> Option<Vec<u8>> {
        std::fs::read(p).ok()
    }
    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn write(&self, p: &Path, s: &str) {
        let _ = std::fs::write(p, s);
    }
    fn remove(&self, p: &Path) {
        let _ = std::fs::remove_file(p);
    }
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), String> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let real = std::fs::canonicalize(from).map_err(|e| format!("resolve {}: {e}", from.display()))?;
        let _ = std::fs::remove_file(to);
        std::fs::copy(&real, to).map_err(|e| format!("copy {} -> {}: {e}", real.display(), to.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn build_spira_config(&self, repo_root: &Path) -> Option<PathBuf> {
        let ok = std::process::Command::new("cargo")
            .args(["build", "--release", "-p", "spira-config"])
            .current_dir(repo_root)
            .stdin(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
        let bin = repo_root.join("target/release/spira-config");
        bin.is_file().then_some(bin)
    }
    fn temp_log(&self) -> PathBuf {
        std::env::temp_dir().join(format!(
            "testenv-build-{}-{}.log",
            std::process::id(),
            self.now()
        ))
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn parent_pid(&self) -> u32 {
        std::os::unix::process::parent_id()
    }
    fn pid_live(&self, pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).is_dir()
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        let t = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        t.lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse().ok())
    }
    fn inotify_used(&self) -> u64 {
        let mut n = 0;
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return 0;
        };
        for p in procs.flatten() {
            if !p
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
            {
                continue;
            }
            let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(l) = std::fs::read_link(fd.path()) {
                    if l.to_string_lossy().starts_with("anon_inode:inotify") {
                        n += 1;
                    }
                }
            }
        }
        n
    }
    fn sysctl(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    fn free_disk(&self, p: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
            return "?".into();
        };
        // SAFETY: statvfs fills the zeroed struct from a valid NUL-terminated path.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return "?".into();
        }
        human(st.f_bavail.saturating_mul(st.f_frsize))
    }
    fn free_mem(&self) -> String {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| crate::util::meminfo_kb(&t, "MemAvailable"))
            .map(|kb| human(kb * 1024))
            .unwrap_or_else(|| "?".into())
    }
    fn owner_dir(&self) -> PathBuf {
        PathBuf::from("/tmp")
    }
    fn symlinked_targets(&self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if !meta.file_type().is_symlink() {
                continue;
            }
            let Ok(canon) = std::fs::canonicalize(&p) else {
                continue;
            };
            let Some(parent) = canon.parent() else {
                continue;
            };
            let parent = parent.to_path_buf();
            if !out.contains(&parent) {
                out.push(parent);
            }
        }
        out
    }
    fn err(&self, line: &str) {
        eprintln!("{line}");
    }
    fn out(&self, line: &str) {
        use std::io::Write;
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
}

/// `testenv container <args>`.
pub fn main(
    args: &[String],
    harness: Option<PathBuf>,
    config: Option<&spira_config::SpiraToml>,
) -> i32 {
    let env = |k: &str| std::env::var(k).ok();
    let src = Source { env: &env, config };
    let conf = Conf::load(&src, harness);
    Driver {
        host: &RealHost,
        conf: &conf,
    }
    .dispatch(args)
}

#[cfg(test)]
mod tests;
