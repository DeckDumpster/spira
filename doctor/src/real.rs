//! The production [`crate::ports::World`]. Locates `conf.sh` via an explicit `$SPIRA_HOME`
//! first (the deliberate override `_activated_release_cmd` pins to the release just
//! activated, sp-r15cf — see DESIGN.md §2), and only when nothing set it falls back to
//! `current_exe()`'s release-relative sibling `spira/`, the equivalent of doctor.sh's own
//! `$(dirname "${BASH_SOURCE[0]}")` for a bare interactive invocation. Everything conf.sh
//! derives is captured ONCE at startup, under `SPIRA_DOCTOR=1` exactly as doctor.sh itself
//! sourced it, so every check reads a consistent snapshot rather than re-sourcing per call.

use crate::ports::{StoreMeta, World};
use std::collections::BTreeMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct Real {
    pub home: PathBuf, // the spira/ directory conf.sh and lib.sh live in
    pub env: BTreeMap<String, String>,
}

impl Real {
    /// Resolves `home` (an explicit, valid `$SPIRA_HOME` first; else release-relative:
    /// `<release>/bin/doctor` -> `<release>/spira/`), then captures conf.sh's derived
    /// environment once.
    pub fn try_new() -> Result<Real, String> {
        let home = Self::resolve_home()?;
        let env = Self::capture_env(&home);
        Ok(Real { home, env })
    }

    pub fn new() -> Real {
        Real::try_new().unwrap_or_else(|e| panic!("{e}"))
    }

    /// `argv[0]`'s directory, never `current_exe()`'s. `current_exe()` canonicalizes every
    /// symlink; testenv's own release staging (and the gate's fixture release) link
    /// `bin/<tool>` to wherever cargo actually built it and `spira/` to the real checkout —
    /// two unrelated directories once resolved, so `current_exe()`-based release-relative
    /// resolution always missed silently there (empty output, exit code standing in for a
    /// verdict). `argv[0]` is exactly the path PATH search resolved to, unresolved further
    /// — bash's own `$0`/`${BASH_SOURCE[0]}` never re-resolved it either.
    fn resolve_home() -> Result<PathBuf, String> {
        if let Ok(h) = std::env::var("SPIRA_HOME") {
            if !h.is_empty() {
                let p = PathBuf::from(&h);
                if p.join("conf.sh").is_file() {
                    return Ok(p);
                }
            }
        }
        if let Some(exe) = argv0_path() {
            if let Some(bin_dir) = exe.parent() {
                if let Some(release_dir) = bin_dir.parent() {
                    let candidate = release_dir.join("spira");
                    if candidate.join("conf.sh").is_file() {
                        return Ok(candidate);
                    }
                }
            }
        }
        match std::env::var("SPIRA_HOME") {
            Ok(h) if !h.is_empty() => Ok(PathBuf::from(h)),
            _ => Err("doctor: SPIRA_HOME is not set and no spira/conf.sh sits beside the executable".to_string()),
        }
    }

    fn capture_env(home: &Path) -> BTreeMap<String, String> {
        let conf = home.join("conf.sh");
        let script = format!("export SPIRA_DOCTOR=1; . \"{}\" >/dev/null 2>&1; env -0", conf.display());
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): `home` is always
        // `<release>/spira` (resolve_home's own contract — an explicit SPIRA_HOME or the
        // argv0-relative derivation, never an unrelated target repo), so its parent IS this
        // binary's own release root; prepend its bin/+spira/ onto the child's PATH rather
        // than only inheriting whatever PATH this process happened to start with.
        let envs = spira_config::release_env::child_path_env(home.parent(), std::env::var("PATH").ok().as_deref());
        let mut cmd = spira_config::bounded::bounded("bash");
        cmd.arg("-c").arg(script).envs(envs);
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output();
        let mut map = BTreeMap::new();
        if let Ok(o) = out {
            for kv in o.stdout.split(|&b| b == 0) {
                if kv.is_empty() {
                    continue;
                }
                if let Some(eq) = kv.iter().position(|&b| b == b'=') {
                    let k = String::from_utf8_lossy(&kv[..eq]).into_owned();
                    let v = String::from_utf8_lossy(&kv[eq + 1..]).into_owned();
                    map.insert(k, v);
                }
            }
        }
        map
    }

    /// Source conf.sh and lib.sh, then run `body` with `args` as `$1`, `$2`, ... Captures
    /// stdout only, trimmed of a trailing newline.
    ///
    /// THE "--" IS NOT DECORATION. `bash -c script arg0 arg1 arg2` assigns the FIRST
    /// argument after the script string to `$0` (bash's own command-name slot), and only
    /// the REST become `$1`, `$2`, ... Without a placeholder there, `args[0]` silently
    /// lands in `$0` and every real argument shifts down by one — `body`'s own `"$1"`
    /// reads what should have been `$2`, and the true `$1` is simply gone. `skew`'s own
    /// seam avoids this by spending the `$0` slot on `lib.sh`'s own path (its `. "$0"`
    /// sourcing trick); `--` is the same fix without that trick. Caught live by the
    /// identical bug in census's `seam`/`seam_full` (sp-yyk47) — ported the fix here too,
    /// since this `seam` has the exact same shape and every two-argument caller
    /// (`counter_events_query`, `spira_unit`) was silently reading `$2` as empty.
    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(
            "export SPIRA_DOCTOR=1; . \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; {body}",
            self.home.display(),
            self.home.display()
        );
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): see capture_env's own note —
        // `self.home` is always `<release>/spira`, so its parent is this binary's release.
        let envs = spira_config::release_env::child_path_env(self.home.parent(), std::env::var("PATH").ok().as_deref());
        let out = spira_config::bounded::bounded("bash").arg("-c").arg(script).arg("--").args(args).envs(envs).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }
}

impl Default for Real {
    fn default() -> Self {
        Real::new()
    }
}

// Registered config keys (spira/conf.d) this crate's generic `World::env` seam also
// serves — $SPIRA_TOML only, through `spira_config::process::cfg`, never the captured
// conf.sh environment, never the raw environment, never a fallback (per Ryan 2026-10-05:
// one source of config). Every other name `env()` is called with has no `spira/conf.d`
// entry and keeps reading the captured `conf.sh` snapshot, exactly as before.
const RESOLVED_KEYS: &[&str] = &[
    "COCKPIT_MAIL",
    "COCKPIT_SESSIONS",
    "SPIRA_BD",
    "SPIRA_BD_TAG",
    "SPIRA_CHAMBER_OVERLAY",
    "SPIRA_DB",
    "SPIRA_DOLT_DATA",
    "SPIRA_INSTANCE",
    "SPIRA_MAIL_MUTE",
    "SPIRA_OPERATED",
    "SPIRA_OVERRIDES",
    "SPIRA_RELEASES",
    "SPIRA_REPO_MAP",
    "SPIRA_ROUND_VM_MIRROR_PORT",
    "SPIRA_RUN",
    "SPIRA_SNAP_STALE_S",
];

impl World for Real {
    fn env(&self, k: &str) -> Option<String> {
        if RESOLVED_KEYS.contains(&k) {
            return spira_config::process::cfg(k).ok();
        }
        self.env.get(k).cloned().or_else(|| std::env::var(k).ok())
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        if name.contains('/') {
            return if self.is_executable_file(name) { Some(PathBuf::from(name)) } else { None };
        }
        let path = self.env("PATH")?;
        std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_exec(p))
    }

    fn is_executable_file(&self, p: &str) -> bool {
        is_exec(Path::new(p))
    }

    fn deps_list_release(&self) -> Vec<String> {
        self.seam("spira_deps_list release", &[]).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
    }

    fn release_status(&self) -> String {
        // batch-job: release runs for as long as its work does
        Command::new("release").arg("status").stdin(Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn spira_config_validate(&self, toml_path: &Path) -> Result<(), String> {
        let out = spira_config::bounded::bounded("spira-config").arg("validate").arg(toml_path).stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
    }

    fn spira_config_migrate(&self, toml_path: &Path) -> String {
        // Exit code deliberately ignored — matching doctor.sh's own `mig="$(spira-config
        // migrate "$toml" 2>&1)"`, which never checked it either; only the text, when
        // non-empty, is logged.
        spira_config::bounded::bounded("spira-config")
            .arg("migrate")
            .arg(toml_path)
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim_end().to_string())
            .unwrap_or_default()
    }

    /// `find <dir> -maxdepth 2 -name '*.md'`, each match relative to `dir`: files directly
    /// in `dir` (depth 1) and files in its immediate subdirectories (depth 2).
    fn find_md_files(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut subdirs = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if p.is_file() && name.ends_with(".md") {
                out.push(name);
            } else if p.is_dir() {
                subdirs.push((name, p));
            }
        }
        for (name, p) in subdirs {
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let child = e.file_name().to_string_lossy().into_owned();
                if e.path().is_file() && child.ends_with(".md") {
                    out.push(format!("{name}/{child}"));
                }
            }
        }
        out.sort();
        out
    }

    fn overrides_doctor(&self) -> Result<String, String> {
        let out = spira_config::bounded::bounded("overrides.sh").arg("doctor").stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stdout).into_owned()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn repo_names(&self) -> Vec<String> {
        self.seam("repo_names", &[]).lines().map(str::to_string).collect()
    }
    fn repo_field(&self, name: &str, field: &str) -> Option<String> {
        let out = self.seam("repo_field \"$1\" \"$2\" 2>/dev/null", &[name, field]);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
    fn dir_has_cargo_toml_within(&self, path: &Path) -> bool {
        has_file_within(path, "Cargo.toml", 2)
    }
    fn gate_definition(&self, home: &Path, name: &str) -> Result<String, String> {
        let out = spira_config::bounded::bounded("gate").arg("--home").arg(home).arg("--definition").arg(name).stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn dir_exists(&self, p: &Path) -> bool {
        p.is_dir()
    }
    fn file_exists(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn bd_list(&self, db: &Path, timeout_secs: u64) -> Result<String, String> {
        let out = Command::new("timeout")
            .arg(timeout_secs.to_string())
            .arg(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
            .arg("-C")
            .arg(db)
            .args(["list", "--limit", "1", "--json"])
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
    }
    fn bd_role_warnings(&self, db: &Path, cwd: &Path) -> Option<usize> {
        let o = Command::new("timeout")
            .arg("5")
            .arg(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
            .arg("-C")
            .arg(db)
            .args(["list", "--limit", "1", "--json"])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        Some(text.lines().filter(|l| l.contains("beads.role")).count())
    }
    fn read_store_meta(&self, metadata_json: &Path) -> Option<StoreMeta> {
        let text = std::fs::read_to_string(metadata_json).ok()?;
        Some(StoreMeta {
            dolt_mode: json_string_field(&text, "dolt_mode"),
            dolt_server_host: json_string_field(&text, "dolt_server_host").unwrap_or_else(|| "127.0.0.1".to_string()),
            dolt_server_port: json_string_field(&text, "dolt_server_port")
                .or_else(|| json_number_field(&text, "dolt_server_port"))
                .and_then(|s| s.parse().ok()),
        })
    }
    fn systemd_user_is_active(&self, unit: &str) -> bool {
        spira_config::bounded::bounded(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
            .args(["--user", "is-active", "--quiet", unit])
            .stdin(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    fn tcp_connect(&self, host: &str, port: u16) -> bool {
        let addr = format!("{host}:{port}");
        match addr.to_socket_addrs() {
            Ok(mut addrs) => addrs.next().map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok()).unwrap_or(false),
            Err(_) => false,
        }
    }

    fn dolt_metrics_disabled(&self) -> bool {
        spira_config::bounded::bounded(self.env("SPIRA_DOLT_BIN").unwrap_or_else(|| "dolt".to_string()))
            .args(["config", "--global", "--get", "metrics.disabled"])
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false)
    }
    fn dolt_events_count(&self) -> usize {
        let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".dolt").join("eventsData");
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    fn bd_first_id(&self, db: &Path) -> Option<String> {
        let out = spira_config::bounded::bounded(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
            .arg("-C")
            .arg(db)
            .args(["list", "--limit", "1", "--json"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        first_json_id(&text)
    }
    fn bump_write_event_try(&self, id: &str, etype: &str, actor: &str) -> bool {
        let script = format!(
            "export SPIRA_DOCTOR=1; . \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; _bump_write_event_try \"$1\" \"$2\" \"$3\"",
            self.home.display(),
            self.home.display()
        );
        spira_config::bounded::bounded("bash").arg("-c").arg(script).arg("--").arg(id).arg(etype).arg(actor).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }
    fn counter_events_query(&self, id: &str, etype: &str) -> String {
        let out = self.seam("_counter_events_query \"$1\" \"$2\"", &[id, etype]);
        if out.is_empty() {
            "?".to_string()
        } else {
            out
        }
    }

    fn systemd_failed_units(&self, pattern: &str) -> Result<Vec<String>, String> {
        let out = spira_config::bounded::bounded(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
            .args(["--user", "list-units", "--state=failed", "--no-legend", "--plain"])
            .arg(pattern)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
    }
    fn watchd_manifest(&self) -> Result<String, String> {
        // watchd is the Rust binary now (sp-07yxy's own concurrent landing) -- was
        // watchd.sh; carried into this port per the coordinator's instruction, sp-yyk47.
        let out = spira_config::bounded::bounded("watchd").arg("manifest").stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(_) => Err("watchd manifest failed".into()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn systemd_enabled_unit_files(&self, pattern: &str) -> Result<Vec<String>, String> {
        let out = spira_config::bounded::bounded(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
            .args(["--user", "list-unit-files", "--no-legend", "--state=enabled"])
            .arg(pattern)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()),
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim().is_empty() && String::from_utf8_lossy(&o.stderr).trim().is_empty() => Ok(Vec::new()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
    }
    fn systemd_installed_unit_execs(&self) -> Result<Vec<(String, String, String)>, String> {
        let sc = self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string());
        let out = spira_config::bounded::bounded(&sc)
            .args(["--user", "list-unit-files", "--no-legend", "--plain", "spira-*.service", "beads-push.service"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            // No unit matched: systemd answers exit 1 with no output at all — a fresh host
            // before install. That is an empty list, as systemd_enabled_unit_files already
            // treats it; any real failure prints its reason and still errors (sp-47myv).
            if text.trim().is_empty() {
                return Ok(Vec::new());
            }
            return Err(text.lines().next().unwrap_or("").to_string());
        }
        let mut rows = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = line.split_whitespace();
            let (Some(unit), Some(state)) = (f.next(), f.next()) else { continue };
            let show = spira_config::bounded::bounded(&sc).args(["--user", "show", unit, "-p", "ExecStart", "--value"]).stdin(Stdio::null()).output();
            let text = show.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
            let path = text.split("path=").nth(1).and_then(|r| r.split([' ', ';']).next()).unwrap_or("").to_string();
            rows.push((unit.to_string(), state.to_string(), path));
        }
        Ok(rows)
    }
    fn spira_unit(&self, kind: &str, subkind: &str) -> String {
        self.seam("spira_unit \"$1\" \"$2\"", &[kind, subkind])
    }

    fn file_age_secs(&self, p: &Path) -> Option<u64> {
        let meta = std::fs::metadata(p).ok()?;
        let modified = meta.modified().ok()?;
        let now = std::time::SystemTime::now();
        now.duration_since(modified).ok().map(|d| d.as_secs())
    }

    fn bd_version(&self) -> Option<String> {
        let out = spira_config::bounded::bounded(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
            .arg("version")
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        extract_semver(&text)
    }
    fn arch(&self) -> String {
        // `std::env::consts::ARCH` at compile time (x86_64/aarch64/...) — the same values
        // `uname -m` reports for the two architectures this check cares about, with no
        // subprocess and no new `deps.toml` entry for a program declared only for this.
        std::env::consts::ARCH.to_string()
    }
    fn spira_bin_purpose(&self, name: &str) -> String {
        self.seam("spira_bin_purpose \"$1\"", &[name])
    }

    fn concierge_stray_holders(&self, concierge_sh: &Path) -> Vec<String> {
        spira_config::bounded::bounded("bash")
            .arg(concierge_sh)
            .arg("_stray-holders")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn sccache_help(&self) -> Option<String> {
        // THE ABSOLUTE PATH, NEVER A BARE NAME (sp-xtdqi-3): `Command::new("sccache")` is
        // resolved by the OS against THIS PROCESS's own ambient PATH, not `self.env("PATH")`
        // (conf.sh's derived one) — the two differ exactly when a launcher execs doctor
        // under a trimmed `env -i ... PATH=...` that omits `~/.cargo/bin` (where sccache
        // actually lives; it is not part of a release's own `bin/`). `which` already
        // resolved the right path for the caller's own FAIL message; this call used to
        // throw that resolution away and search PATH a second time, blind, and lose.
        let bin = self.which("sccache").unwrap_or_else(|| PathBuf::from("sccache"));
        spira_config::bounded::bounded(bin)
            .arg("--help")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn sccache_show_stats(&self) -> Option<String> {
        // Carries the configured store's own vars (if any) so a server THIS CALL happens to
        // spawn (none was running) starts on the right backend — harmless when one is
        // already running, since sccache's client only ever queries an existing daemon's
        // socket and never re-applies a later invocation's environment to it.
        let bin = self.which("sccache").unwrap_or_else(|| PathBuf::from("sccache"));
        let mut cmd = spira_config::bounded::bounded(bin);
        cmd.arg("--show-stats").stdin(Stdio::null()).stderr(Stdio::null());
        if let Some(addr) = self.sccache_dav_addr() {
            let endpoint = if addr.contains("://") { addr } else { format!("http://{addr}") };
            cmd.env("SCCACHE_WEBDAV_ENDPOINT", endpoint);
            cmd.env("SCCACHE_WEBDAV_KEY_PREFIX", "/");
        }
        cmd.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// `SPIRA_SCCACHE_DAV_ADDR` is a registered key (spira/conf.d) with no registry
    /// default — the one source of config (per Ryan 2026-10-05), through
    /// `spira_config::process::cfg`, never the raw environment and never a second,
    /// crate-local document discovery.
    fn sccache_dav_addr(&self) -> Option<String> {
        spira_config::process::cfg("SPIRA_SCCACHE_DAV_ADDR").ok().filter(|v| !v.is_empty())
    }

    fn git_daemon_base_paths(&self, port: u16) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir("/proc") else { return Vec::new() };
        rd.flatten()
            .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
            .filter_map(|raw| daemon_base_path(&String::from_utf8_lossy(&raw).split('\0').map(String::from).collect::<Vec<_>>(), port))
            .collect()
    }

    fn out(&self, s: &str) {
        println!("{s}");
    }
}

/// `--base-path` of a `git daemon` argv serving `port`; None for any other process.
pub(crate) fn daemon_base_path(argv: &[String], port: u16) -> Option<String> {
    let is_git = argv.first().is_some_and(|a| a == "git" || a.ends_with("/git"));
    if !is_git || !argv.iter().any(|a| a == "daemon") {
        return None;
    }
    let want = format!("--port={port}");
    if !argv.iter().any(|a| *a == want) {
        return None;
    }
    argv.iter().find_map(|a| a.strip_prefix("--base-path=")).map(String::from)
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `argv[0]`, resolved to where it actually sits, with every symlink component left
/// exactly as invoked — see `Real::resolve_home`'s comment.
///
/// A bare name (no `/`) is NOT already the resolved path the way it is when a shell execs
/// a PATH-found command: bash rewrites its own `$0` to the full path PATH search landed on
/// before exec, but a non-shell caller — `env PATH=... doctor`, `posix_spawnp`, a test
/// harness's own `env -i PATH="$STUBBIN:$PATH" ...` — calls `execvp` directly, which
/// resolves the PATH search internally but passes argv[0] through unchanged: still the
/// bare string "doctor". Joining that onto the current directory (as if shell-relative)
/// lands on nothing real. Caught live by testenv's test-skew-local-release.sh exercising
/// the identical pattern in `skew` (sp-yyk47) — ported here for the same reason.
fn argv0_path() -> Option<PathBuf> {
    let arg0 = std::env::args_os().next()?;
    let p = PathBuf::from(&arg0);
    if p.components().count() > 1 {
        return if p.is_absolute() { Some(p) } else { Some(std::env::current_dir().ok()?.join(p)) };
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&arg0);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn has_file_within(path: &Path, name: &str, max_depth: u32) -> bool {
    fn walk(dir: &Path, name: &str, depth: u32) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else { return false };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() && p.file_name().map(|n| n == name).unwrap_or(false) {
                return true;
            }
            if depth > 0 && p.is_dir() && walk(&p, name, depth - 1) {
                return true;
            }
        }
        false
    }
    walk(path, name, max_depth.saturating_sub(1))
}

/// Tiny, fixed-shape JSON helpers — the same "not worth a crate" call `skew` makes for
/// `gh`'s JSON (DESIGN.md §6): `metadata.json` and a `bd list --json` array are both small,
/// known shapes.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let idx = text.find(&pat)?;
    let rest = &text[idx + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
fn json_number_field(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let idx = text.find(&pat)?;
    let rest = &text[idx + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let end = rest.find(|c: char| c == ',' || c == '}' || c.is_whitespace()).unwrap_or(rest.len());
    let num = rest[..end].trim();
    if num.is_empty() || num == "null" {
        None
    } else {
        Some(num.to_string())
    }
}
fn first_json_id(text: &str) -> Option<String> {
    json_string_field(text, "id")
}
fn extract_semver(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut j = i;
            let mut dots = 0;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                if bytes[j] == b'.' {
                    dots += 1;
                }
                j += 1;
            }
            if dots == 2 {
                return Some(text[i..j].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sccache 0.18.0's real `--help` output (captured live, built with `--features
    /// webdav`) — the exact text [`World::sccache_help`]'s caller (`check_sccache`) parses
    /// for the "Enabled features:" block. A fake script printing anything else would not
    /// prove the plumbing to the REAL binary is correct.
    const REAL_SCCACHE_018_HELP: &str = "Usage: sccache [OPTIONS] <--dist-auth|--debug-preprocessor-cache|--dist-status|--show-stats|--show-adv-stats|--start-server|--stop-server|--zero-stats|--package-toolchain <EXE> <OUT>|CMD>\n\nArguments:\n  [CMD]...  \n\nOptions:\n  -s, --show-stats                     show cache statistics\n      --show-adv-stats                 show advanced cache statistics\n      --start-server                   start background server\n      --debug-preprocessor-cache       show all preprocessor cache entries\n      --stop-server                    stop background server\n  -z, --zero-stats                     zero statistics counters\n      --dist-auth                      authenticate for distributed compilation\n      --dist-status                    show status of the distributed client\n      --package-toolchain <EXE> <OUT>  package toolchain for distributed compilation\n      --stats-format <FMT>             set output format of statistics [default: text] [possible\n                                       values: text, json]\n  -h, --help                           Print help\n  -V, --version                        Print version\n\nEnabled features:\n    S3:        false\n    Redis:     false\n    Memcached: false\n    GCS:       false\n    GHA:       false\n    Azure:     false\n    WebDAV:    true\n    OSS:       false\n    COS:       false\n";

    fn fake_sccache(dir: &Path, help: &str) -> PathBuf {
        let p = dir.join("sccache");
        let script = format!("#!/bin/sh\nif [ \"$1\" = '--help' ]; then\n  cat <<'SCCACHE_HELP_EOF'\n{help}SCCACHE_HELP_EOF\nfi\n");
        // `testkit::write_exe`, never a raw `fs::write` + `chmod` (sp-xtdqi-3): this
        // process never holds a write descriptor on the file it is about to exec, so a
        // concurrent test thread's own `Command::spawn` elsewhere in this binary cannot
        // fork over an open one and see this exec fail with ETXTBSY.
        testkit::write_exe(&p, &script);
        p
    }

    /// THE POSITIVE CONTROL for the PATH bug (sp-xtdqi-3): `self.env` carries the fake
    /// script's directory; the REAL ambient process PATH (whatever `cargo test` itself
    /// runs under) is never touched and need not contain it — proving `sccache_help`
    /// resolves through `self.which`/`self.env("PATH")`, not a bare `Command::new("sccache")`
    /// left to the OS's own ambient-PATH search, which is what silently returned `None` in
    /// production under a launcher's trimmed `env -i ... PATH=...`.
    #[test]
    fn sccache_help_resolves_through_self_env_path_not_the_bare_ambient_one() {
        // The exact shape of the production repro (`env -i ... PATH=$R/bin:/usr/bin:/bin
        // doctor`): no `~/.cargo/bin`, so a bare `Command::new("sccache")` finds nothing —
        // this process's OWN ambient PATH, inherited by `cargo test`'s shell, would
        // otherwise still contain the real cargo-installed sccache and hide the bug.
        let d = testkit::TempDir::new("doctor-real-sccache-help");
        let bin = fake_sccache(d.path(), REAL_SCCACHE_018_HELP);
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), d.path().display().to_string());
        let real = Real { home: PathBuf::from("/nonexistent-sp-xtdqi-3"), env };
        // `check_sccache` (below) reads `SPIRA_OPERATED`, a `RESOLVED_KEYS` name — through
        // `spira_config::process::cfg`, a process-global `OnceLock`. Same reasoning as the
        // `sccache_show_stats` test below: seed it with the identical real-home + complete
        // fixture so whichever test's setup wins the race leaves a valid resolution behind
        // for every later test in this binary, including `tests::
        // config_files_passes_when_every_registered_key_resolves`.
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let toml = spira_config::process::fixture_toml(d.path(), &[]);
        let _cfg_env = testkit::env(&[("PATH", Some("/usr/bin:/bin")), ("SPIRA_HOME", real_home.to_str()), ("SPIRA_TOML", toml.to_str())]);
        let help = real.sccache_help().expect("the fake script is reachable through self.env(\"PATH\")");
        assert!(help.contains("WebDAV:    true"), "{help}");
        // check_sccache (lib.rs) is the actual caller this bug broke: FAIL became OK only
        // once sccache_help could reach the binary this way.
        let lines = crate::check_sccache(&real);
        let levels: Vec<crate::Level> = lines.iter().map(|l| l.level).collect();
        assert_eq!(levels, vec![crate::Level::Ok], "{lines:?}");
        let _ = bin;
    }

    /// `--show-stats` resolves through the same path, and carries the store's env vars
    /// when one is configured.
    #[test]
    fn sccache_show_stats_resolves_through_self_env_path_and_carries_the_store_vars() {
        let d = testkit::TempDir::new("doctor-real-show-stats");
        let log = d.path().join("calls.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$SCCACHE_WEBDAV_ENDPOINT\" >> '{log}'\nif [ \"$1\" = '--show-stats' ]; then\n  printf 'Cache location                  webdav, name: , prefix: /\\n'\nfi\n",
            log = log.display(),
        );
        let p = d.path().join("sccache");
        testkit::write_exe(&p, &script);

        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), d.path().display().to_string());
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(home.join("conf.d/SPIRA_SCCACHE_DAV_ADDR"), conf_d_stub()).unwrap();
        let real = Real { home, env };
        // `sccache_show_stats` calls `self.sccache_dav_addr()`, which goes through
        // `spira_config::process::cfg` unconditionally now (per Ryan 2026-10-05) — its
        // resolution is a process-global `OnceLock`, so SOME test in this binary has to
        // seed it with a real, complete fixture before anything touches it, or whichever
        // test runs first wins with no config at all and poisons the rest (exactly what
        // broke `tests::config_files_passes_when_every_registered_key_resolves`). This
        // uses the identical `real_home` + complete-fixture shape that test uses, so
        // either one winning the race leaves the same, valid resolution behind.
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let toml = spira_config::process::fixture_toml(d.path(), &[]);
        let _cfg_env = testkit::env(&[("PATH", Some("/usr/bin:/bin")), ("SPIRA_HOME", real_home.to_str()), ("SPIRA_TOML", toml.to_str())]);
        let stats = real.sccache_show_stats().expect("reachable through self.env(\"PATH\")");
        assert!(stats.contains("webdav"), "{stats}");
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.starts_with("--show-stats"), "{calls:?}");
    }

    fn conf_d_stub() -> String {
        "TYPE=string\nGROUP=sccache\nDOC=test fixture\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT.\nSPIRA_CONF_DEFAULT_EOF\n".to_string()
    }
}
