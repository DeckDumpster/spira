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
    pub fn new() -> Real {
        let home = Self::resolve_home();
        let env = Self::capture_env(&home);
        Real { home, env }
    }

    /// `argv[0]`'s directory, never `current_exe()`'s. `current_exe()` canonicalizes every
    /// symlink; testenv's own release staging (and the gate's fixture release) link
    /// `bin/<tool>` to wherever cargo actually built it and `spira/` to the real checkout —
    /// two unrelated directories once resolved, so `current_exe()`-based release-relative
    /// resolution always missed silently there (empty output, exit code standing in for a
    /// verdict). `argv[0]` is exactly the path PATH search resolved to, unresolved further
    /// — bash's own `$0`/`${BASH_SOURCE[0]}` never re-resolved it either.
    fn resolve_home() -> PathBuf {
        if let Ok(h) = std::env::var("SPIRA_HOME") {
            if !h.is_empty() {
                let p = PathBuf::from(&h);
                if p.join("conf.sh").is_file() {
                    return p;
                }
            }
        }
        if let Some(exe) = argv0_path() {
            if let Some(bin_dir) = exe.parent() {
                if let Some(release_dir) = bin_dir.parent() {
                    let candidate = release_dir.join("spira");
                    if candidate.join("conf.sh").is_file() {
                        return candidate;
                    }
                }
            }
        }
        std::env::var("SPIRA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
    }

    fn capture_env(home: &Path) -> BTreeMap<String, String> {
        let conf = home.join("conf.sh");
        let script = format!("export SPIRA_DOCTOR=1; . \"{}\" >/dev/null 2>&1; env -0", conf.display());
        let out = Command::new("bash").arg("-c").arg(script).stdin(Stdio::null()).stderr(Stdio::null()).output();
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
    /// stdout only, trimmed of a trailing newline. The same one-shot seam `skew` uses.
    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(
            "export SPIRA_DOCTOR=1; . \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; {body}",
            self.home.display(),
            self.home.display()
        );
        let out = Command::new("bash").arg("-c").arg(script).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }
}

impl Default for Real {
    fn default() -> Self {
        Real::new()
    }
}

impl World for Real {
    fn env(&self, k: &str) -> Option<String> {
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
        Command::new("release").arg("status").stdin(Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn spira_config_validate(&self, toml_path: &Path) -> Result<(), String> {
        let out = Command::new("spira-config").arg("validate").arg(toml_path).stdin(Stdio::null()).output();
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
        Command::new("spira-config")
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
        let out = Command::new("overrides.sh").arg("doctor").stdin(Stdio::null()).output();
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
        let out = Command::new("gate").arg("--home").arg(home).arg("--definition").arg(name).stdin(Stdio::null()).output();
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
        Command::new(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
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

    fn bd_first_id(&self, db: &Path) -> Option<String> {
        let out = Command::new(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
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
        Command::new("bash").arg("-c").arg(script).arg("--").arg(id).arg(etype).arg(actor).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
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
        let out = Command::new(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
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
        let out = Command::new("watchd.sh").arg("manifest").stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(_) => Err("watchd.sh manifest failed".into()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn systemd_enabled_unit_files(&self, pattern: &str) -> Result<Vec<String>, String> {
        let out = Command::new(self.env("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string()))
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
        let out = Command::new(self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string()))
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
        Command::new("bash")
            .arg(concierge_sh)
            .arg("_stray-holders")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn out(&self, s: &str) {
        println!("{s}");
    }
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
