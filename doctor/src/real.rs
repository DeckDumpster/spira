//! The production [`crate::ports::World`]. Locates `conf.sh` via an explicit `$SPIRA_HOME`
//! first (the deliberate override `_activated_release_cmd` pins to the release just
//! activated, sp-r15cf — see DESIGN.md §2), and only when nothing set it falls back to
//! `current_exe()`'s release-relative sibling `spira/`, the equivalent of doctor.sh's own
//! `$(dirname "${BASH_SOURCE[0]}")` for a bare interactive invocation. Everything conf.sh
//! derives is captured ONCE at startup, so every check reads a consistent snapshot rather than re-sourcing per call.

use crate::ports::{Placement, RootScan, RootWriter, StoreMeta, World};
use std::collections::BTreeMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct Real {
    pub home: PathBuf, // the spira/ directory conf.sh and lib.sh live in
    pub env: BTreeMap<String, String>,
    /// `spira-lc`, the declared release's.
    pub lc: String,
    /// `PATH`/`SPIRA_RELEASE` every conf.sh/lib.sh child runs under, from the declared release.
    pub child_env: Vec<(String, String)>,
}

impl Real {
    /// Requires `SPIRA_TOML`, resolves `home` from `SPIRA_HOME` or `$SPIRA_RELEASE/spira`, then
    /// captures conf.sh's derived environment once. A missing declaration is a refusal naming it.
    pub fn try_new() -> Result<Real, String> {
        crate::require_spec(spira_config::process::spec().ok().as_deref())?;
        let home = spira_config::resolve::locate_home_for_process().map_err(|e| format!("doctor: {e}"))?;
        if !home.join("conf.sh").is_file() {
            return Err(format!("doctor: {} holds no conf.sh", home.display()));
        }
        std::env::set_var("SPIRA_HOME", &home);
        let child_env = spira_config::release_env::child_path_env_for_process().map_err(|e| format!("doctor: {e}"))?;
        let mut env = Self::capture_env(&home, &child_env);
        env.entry("SPIRA_HOME".to_string()).or_insert_with(|| home.display().to_string());
        Ok(Real { home, env, lc: spira_config::release_env::tool("spira-lc")?, child_env })
    }

    pub fn new() -> Real {
        Real::try_new().unwrap_or_else(|e| panic!("{e}"))
    }

    fn capture_env(home: &Path, child_env: &[(String, String)]) -> BTreeMap<String, String> {
        let conf = home.join("conf.sh");
        let script = format!(". \"{}\" >/dev/null 2>&1; env -0", conf.display());
        let mut cmd = spira_config::bounded::bounded("bash");
        cmd.arg("-c").arg(script).envs(child_env.iter().cloned());
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
            ". \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; {body}",
            self.home.display(),
            self.home.display()
        );
        let out = spira_config::bounded::bounded("bash").arg("-c").arg(script).arg("--").args(args).envs(self.child_env.iter().cloned()).stdin(Stdio::null()).stderr(Stdio::null()).output();
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
    "SPIRA_RUNES_OVERLAY",
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
        let Ok(release) = spira_config::release_env::tool("release") else { return String::new() };
        // batch-job: release runs for as long as its work does
        Command::new(release).arg("status").stdin(Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
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
    fn read_file(&self, p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }
    fn bd_list(&self, timeout_secs: u64) -> Result<String, String> {
        let out = Command::new("timeout")
            .arg(timeout_secs.to_string())
            .arg(&self.lc)
            .args(["content", "list", "--limit", "1", "--json"])
            .envs(self.child_env.iter().cloned())
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
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
        spira_config::bounded::bounded(self.env("SPIRA_DOLT_BIN").unwrap_or_else(|| "dolt".to_string())) // STORE_DOOR_CONSIDERED="dolt's own global config file, read for one key; no database and no bead"
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

    fn dolt_root_passwordless(&self, host: &str, port: u16) -> Option<bool> {
        if !self.tcp_connect(host, port) {
            return None;
        }
        let out = spira_config::bounded::bounded(self.env("SPIRA_DOLT_BIN").unwrap_or_else(|| "dolt".to_string())) // STORE_DOOR_CONSIDERED="probes whether the server lets root in with no password: a login check on the server itself, reads no bead"
            .args(["--host", host, "--port", &port.to_string(), "--user", "root", "--no-tls", "sql", "-q", "SELECT 1"])
            .env("DOLT_CLI_PASSWORD", "")
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if out.status.success() {
            return Some(true);
        }
        let text = String::from_utf8_lossy(&out.stderr).to_lowercase() + &String::from_utf8_lossy(&out.stdout).to_lowercase();
        text.contains("access denied").then_some(false)
    }

    fn bd_first_id(&self) -> Option<String> {
        let out = spira_config::bounded::bounded(&self.lc)
            .args(["content", "list", "--limit", "1", "--json"])
            .envs(self.child_env.iter().cloned())
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        first_json_id(&text)
    }
    fn bump_write_event_try(&self, id: &str, etype: &str, actor: &str) -> bool {
        let script = format!(
            ". \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; _bump_write_event_try \"$1\" \"$2\" \"$3\"",
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
            Ok(o) if o.status.success() => Ok(loaded_failed_units(&String::from_utf8_lossy(&o.stdout))),
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
            Ok(o) if o.status.success() => Ok(loaded_failed_units(&String::from_utf8_lossy(&o.stdout))),
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim().is_empty() && String::from_utf8_lossy(&o.stderr).trim().is_empty() => Ok(Vec::new()),
            Ok(o) => Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Err(e.to_string()),
        }
    }
    fn systemd_installed_unit_execs(&self) -> Result<Vec<(String, String, String)>, String> {
        let sc = self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string());
        let out = spira_config::bounded::bounded(&sc)
            .args(["--user", "list-unit-files", "--no-legend", "--plain", "spira-*.service"])
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
        let v = spira_config::process::cfg("SPIRA_SCCACHE_DAV_ADDR").ok().filter(|v| !v.is_empty())?;
        spira_config::hostaddr::resolve_hostport(&v).ok()
    }

    fn dwarf_build_bases(&self) -> Vec<PathBuf> {
        spira_config::build_dir::configured_bases()
    }

    fn on_tmpfs(&self, p: &Path) -> bool {
        spira_config::build::on_tmpfs(p)
    }

    fn placement_roots(&self) -> Vec<(String, Option<PathBuf>)> {
        let mut roots = vec![("podman graphroot".to_string(), self.podman_graphroot())];
        roots.extend(placement_roots_from(|k| spira_config::process::cfg(k).ok(), spira_config::build_dir::configured_bases()));
        roots
    }

    fn placement_of(&self, p: &Path) -> Placement {
        let mounts = std::fs::read_to_string(Path::new(system_paths::PROC_SELF).join("mounts")).unwrap_or_default();
        judge_placement(p, Path::new(system_paths::ROOT), &mounts)
    }

    fn root_writers(&self) -> RootScan {
        scan_root_writers(Path::new(system_paths::PROC), Path::new(system_paths::ROOT), std::process::id())
    }

    fn root_targets(&self) -> Vec<PathBuf> {
        let mut parents = vec![PathBuf::from(system_paths::VAR_TMP)];
        parents.extend(self.env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from));
        parents.iter().flat_map(|p| target_roots_in(p, Path::new(system_paths::ROOT))).collect()
    }

    fn git_daemon_base_paths(&self, port: u16) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(system_paths::PROC) else { return Vec::new() };
        rd.flatten()
            .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
            .filter_map(|raw| daemon_base_path(&String::from_utf8_lossy(&raw).split('\0').map(String::from).collect::<Vec<_>>(), port))
            .collect()
    }

    fn git_hooks_path(&self, repo: &Path) -> Option<String> {
        let o = std::process::Command::new("timeout").arg("5").arg("git").arg("-C").arg(repo).args(["config", "--get", "core.hooksPath"]).output().ok()?;
        let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !v.is_empty()).then_some(v)
    }

    fn deps_provided_by_spira_env(&self) -> Vec<String> {
        self.seam("spira-config deps provided spira-env", &[]).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
    }

    fn spira_env_installed(&self) -> Option<String> {
        let home = self.env("HOME").filter(|h| !h.is_empty())?;
        std::fs::canonicalize(Path::new(&home).join(spira_config::SPIRA_ENV_REL)).ok().map(|p| p.display().to_string())
    }

    fn spira_env_locked(&self) -> Result<String, String> {
        let root = self.home.parent().ok_or("no release root above SPIRA_HOME")?;
        let target = format!("path:{}#spira-env.outPath", root.display());
        let o = spira_config::bounded::bounded("nix")
            .args(["--extra-experimental-features", "nix-command flakes", "eval", "--raw", &target])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("nix eval failed").to_string())
        }
    }

    fn tool_version_line(&self, path: &Path) -> Option<String> {
        let o = spira_config::bounded::bounded(path).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_string)
    }

    fn submission_status(&self) -> Option<String> {
        let bin = self.which("spira-submission")?;
        let o = spira_config::bounded::bounded(&bin).arg("status").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
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
    if !argv.contains(&want) {
        return None;
    }
    argv.iter().find_map(|a| a.strip_prefix("--base-path=")).map(String::from)
}

/// Names from `list-units --state=failed --plain`; a unit whose file is gone lists as
/// `not-found` and is not a failure of anything installed.
pub fn loaded_failed_units(listing: &str) -> Vec<String> {
    listing.lines().filter_map(|l| {
        let mut cols = l.split_whitespace();
        let name = cols.next()?;
        (cols.next() != Some("not-found")).then(|| name.to_string())
    }).collect()
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
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
impl Real {
    fn podman_graphroot(&self) -> Option<PathBuf> {
        let bin = self.which("podman")?;
        let mut cmd = spira_config::bounded::bounded(bin);
        cmd.args(["info", "--format", "{{.Store.GraphRoot}}"]).stdin(Stdio::null()).stderr(Stdio::null());
        let o = cmd.output().ok().filter(|o| o.status.success())?;
        let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!v.is_empty()).then(|| PathBuf::from(v))
    }
}

/// The config-keyed heavy-write roots, one per key; an unset key is a root with no path.
pub fn placement_roots_from(cfg: impl Fn(&str) -> Option<String>, dwarf_bases: Vec<PathBuf>) -> Vec<(String, Option<PathBuf>)> {
    let path = |key: &str| cfg(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut roots: Vec<(String, Option<PathBuf>)> = [
        "SPIRA_TESTENV_SCRATCH_ROOT",
        "SPIRA_TESTENV_SANDBOX_TARGET_ROOT",
        "SPIRA_GATE_TARGET_ROOT",
        "SPIRA_CARGO_HOME",
        "SPIRA_SCCACHE_DAV_ROOT",
        "SPIRA_DOLT_DATA",
    ]
    .iter()
    .map(|k| (k.to_string(), path(k)))
    .collect();
    roots.push(("SPIRA_LC_STORE_PATH (directory)".into(), path("SPIRA_LC_STORE_PATH").and_then(|p| p.parent().map(Path::to_path_buf))));
    roots.extend(dwarf_bases.into_iter().map(|b| (format!("SPIRA_DWARF_BUILD_BASE {}", b.display()), Some(b))));
    roots
}

/// Follows `p`'s symlinks and compares its device with `root`'s. `mounts` is `/proc/self/mounts`
/// text, used only to name the filesystem of a path that is on root.
pub fn judge_placement(p: &Path, root: &Path, mounts: &str) -> Placement {
    let canonical = match p.canonicalize() {
        Ok(c) => c,
        Err(e) => return Placement::Unresolved(format!("{}: {e}", p.display())),
    };
    match same_device(&canonical, root) {
        None => return Placement::Unresolved(format!("cannot stat {} or {}", canonical.display(), root.display())),
        Some(false) => return Placement::Elsewhere,
        Some(true) => {}
    }
    let source = mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?, f.next()?))
        })
        .filter(|(_, mp)| canonical.starts_with(mp))
        .max_by_key(|(_, mp)| mp.len())
        .map(|(src, _)| src.to_string())
        .unwrap_or_else(|| "unknown".into());
    Placement::OnRoot { canonical, source }
}

/// Whether `p` (symlinks followed) is on `root`'s device; `None` when either cannot be stat'd.
pub fn same_device(p: &Path, root: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    Some(std::fs::metadata(p).ok()?.dev() == std::fs::metadata(root).ok()?.dev())
}

/// `O_ACCMODE` of an `fdinfo` `flags:` line (octal) is `O_WRONLY` or `O_RDWR`.
pub fn fdinfo_writable(fdinfo: &str) -> bool {
    fdinfo
        .lines()
        .find_map(|l| l.strip_prefix("flags:"))
        .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        .is_some_and(|f| matches!(f & 0o3, 1 | 2))
}

/// The two environ keys a build is recognised by; every other key is dropped unread.
fn build_env_of(environ: &[u8]) -> (bool, Option<String>) {
    let (mut wrapper, mut target) = (false, None);
    for kv in environ.split(|b| *b == 0) {
        let Some(at) = kv.iter().position(|b| *b == b'=') else { continue };
        match &kv[..at] {
            b"RUSTC_WRAPPER" => wrapper = true,
            b"CARGO_TARGET_DIR" if at + 1 < kv.len() => target = Some(String::from_utf8_lossy(&kv[at + 1..]).into_owned()),
            _ => {}
        }
    }
    (wrapper, target)
}

/// A build env key, or a container scope in the cgroup path.
pub fn is_build(environ: &[u8], cgroup: &str) -> bool {
    let (wrapper, target) = build_env_of(environ);
    wrapper || target.is_some() || cgroup_path(cgroup).split('/').any(|c| c.starts_with("libpod-") && c.ends_with(".scope"))
}

fn cgroup_path(cgroup: &str) -> &str {
    cgroup.lines().find_map(|l| l.strip_prefix("0::")).unwrap_or("").trim()
}

/// The last component of the `0::` cgroup path.
pub fn cgroup_unit(cgroup: &str) -> String {
    cgroup_path(cgroup).rsplit('/').next().unwrap_or("").to_string()
}

pub fn scan_root_writers(proc: &Path, root: &Path, own: u32) -> RootScan {
    let Ok(rd) = std::fs::read_dir(proc) else { return RootScan::Blind(format!("cannot read {}", proc.display())) };
    let (mut readable, mut out) = (0usize, Vec::new());
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()).filter(|p| *p != own) else { continue };
        let dir = e.path();
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else { continue };
        readable += 1;
        let environ = std::fs::read(dir.join("environ")).unwrap_or_default();
        let cgroup = std::fs::read_to_string(dir.join("cgroup")).unwrap_or_default();
        let mut paths: Vec<PathBuf> = fds
            .flatten()
            .filter(|f| std::fs::read_to_string(dir.join("fdinfo").join(f.file_name())).is_ok_and(|i| fdinfo_writable(&i)))
            .filter_map(|f| std::fs::read_link(f.path()).ok())
            .filter(|l| l.is_absolute())
            .collect();
        paths.extend(build_env_of(&environ).1.map(PathBuf::from));
        paths.sort();
        paths.dedup();
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default().trim().to_string();
        let (unit, build) = (cgroup_unit(&cgroup), is_build(&environ, &cgroup));
        out.extend(
            paths
                .into_iter()
                .filter(|p| same_device(p, root) == Some(true))
                .map(|path| RootWriter { pid, comm: comm.clone(), unit: unit.clone(), path, build }),
        );
    }
    if readable == 0 {
        return RootScan::Blind(format!("no {}/<pid>/fd directory could be read", proc.display()));
    }
    RootScan::Seen(out)
}

pub fn target_roots_in(parent: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(parent) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("CACHEDIR.TAG").is_file() && same_device(p, root) == Some(true)).collect();
    v.sort();
    v
}

#[cfg(test)]
mod failed_listing_tests {
    use super::{loaded_failed_units, Real};
    use crate::World;

    #[test_id(t51vuv0)]
    #[test]
    fn a_retired_unit_listed_not_found_is_not_a_failed_unit() {
        let listing = "spira-vm-pool-section-prod.service not-found failed failed spira vm pool\nspira-sentinel-prod.service loaded failed failed Sentinel\n";
        assert_eq!(loaded_failed_units(listing), vec!["spira-sentinel-prod.service".to_string()]);
    }

    #[test_id(tc0f0z1)]
    #[test]
    fn the_failed_unit_probe_drops_a_retired_unit_systemd_still_lists() {
        let tmp = testkit::TempDir::new("doctor-failed-probe");
        let dir = tmp.path().to_path_buf();
        let fake = dir.join("systemctl");
        testkit::write_exe(&fake, "#!/bin/sh\nprintf '%s\\n' 'spira-vm-pool-section-prod.service not-found failed failed gone' 'spira-sentinel-prod.service loaded failed failed Sentinel'\n");
        let mut env = std::collections::BTreeMap::new();
        env.insert("SPIRA_SYSTEMCTL".to_string(), fake.display().to_string());
        let w = Real { home: dir.clone(), env, lc: String::new(), child_env: vec![] };
        let got = w.systemd_failed_units("spira-*").unwrap();
        assert_eq!(got, vec!["spira-sentinel-prod.service".to_string()]);
    }
}

#[cfg(test)]
mod door_tests {
    use super::*;
    use crate::ports::World;

    #[test_id(tg093k3)]
    #[test]
    fn the_store_reads_go_through_spira_lc_and_nothing_else() {
        let tmp = testkit::TempDir::new("doctor-door");
        let log = tmp.path().join("calls");
        let lc = tmp.path().join("lc");
        testkit::write_exe(
            &lc,
            &format!("#!/bin/sh\necho \"$*\" >> '{}'\necho '[{{\"id\":\"sp-9\"}}]'\n", log.display()),
        );
        let real = Real { home: tmp.path().to_path_buf(), env: BTreeMap::new(), lc: lc.to_string_lossy().into_owned(), child_env: Vec::new() };
        assert!(real.bd_list(5).is_ok());
        assert_eq!(real.bd_first_id().as_deref(), Some("sp-9"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().collect::<Vec<_>>(), ["content list --limit 1 --json", "content list --limit 1 --json"]);
    }
}
