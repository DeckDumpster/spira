//! `testenv testdb` — server-mode test databases, one private Dolt sql-server per fixture,
//! started from a pre-initialised template (DESIGN-testdb.md). Replaces the shared
//! `dolt-beads-test.service` + init lock that serialised every server-mode suite (sp-v2lqd).
//! Also the embedded-mode fixture lifecycle (init, baseline snapshot, borrow, reset, drop),
//! moved out of `testdb.sh`'s bash so the script becomes a call-site shim (DESIGN-testdb.md
//! §6, sp-k2oyn).

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_ROOT: &str = "/var/tmp/spira-testdb";
/// The database every template (and so every fixture) holds; unique per server, so fixed.
pub const DATABASE: &str = "sptest";
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const PORT_ATTEMPTS: u32 = 5;

const USAGE: &str = "usage: testenv testdb template --bd B --dolt D [--root R]\n       testenv testdb up --tag T --bd B --dolt D [--root R] [--owner PID]\n       testenv testdb reset --fixture F\n       testenv testdb down --fixture F\n       testenv testdb reap --fixture F --owner PID\n       testenv testdb embedded-check --bd B\n       testenv testdb embedded-up --tag T --bd B\n       testenv testdb embedded-borrow --baseline BASE\n       testenv testdb embedded-reset --dir D --baseline BASE\n       testenv testdb embedded-down [--dir D] [--baseline BASE] [--bin BIN]";

// ---- pure pieces ---------------------------------------------------------------------

/// Resolve an executable: a name with `/` is taken as a path, else searched on `path_var`.
pub fn resolve_exe(name: &str, path_var: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return fs::canonicalize(name).ok();
    }
    path_var
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .and_then(|p| fs::canonicalize(p).ok())
}

/// The `bd` a fixture's callers should run, as an absolute path **without** resolving links:
/// the first `name` on `path_var` (a name with `/` is made absolute). Not canonicalised, so
/// inside a metered suite it stays the meter's link and the calls stay metered. conf.sh keys
/// its `bd migrate schema` cache on `stat "$SPIRA_BD"`; a bare `bd` never stats, so every
/// conf.sh source re-ran the check (DESIGN-testdb.md §2.4, sp-34ru2).
pub fn locate_exe(name: &str, path_var: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = Path::new(name);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(p)
        };
        return abs.is_file().then_some(abs);
    }
    path_var
        .split(':')
        .filter(|d| !d.is_empty() && Path::new(d).is_absolute())
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// `bd` inside a parallel suite is the bd meter (bdmeter.rs): a resolved executable that is
/// the meter stands for the real `name` behind it on `path_var`, so the template is keyed on
/// and built with the real binary, and a suite finds the template setup built
/// (DESIGN-testdb.md §2.4). Anything else, or a meter with nothing behind it, is kept.
pub fn see_through_meter(resolved: PathBuf, name: &str, path_var: &str) -> PathBuf {
    if resolved.file_name() != Some(std::ffi::OsStr::new(crate::bdmeter::METER_EXE)) {
        return resolved;
    }
    let base = Path::new(name)
        .file_name()
        .and_then(|b| b.to_str())
        .unwrap_or(name);
    crate::bdmeter::find_real(base, std::ffi::OsStr::new(path_var), &resolved)
        .and_then(|p| fs::canonicalize(p).ok())
        .unwrap_or(resolved)
}

/// Identity of one executable for the template key: canonical path, size, mtime.
pub fn exe_identity(p: &Path) -> io::Result<String> {
    let m = fs::metadata(p)?;
    Ok(format!("{}\0{}\0{}", p.display(), m.len(), m.mtime()))
}

/// The template key: a new `bd` or `dolt` binary gets a new template.
pub fn template_key(bd_identity: &str, dolt_identity: &str) -> String {
    let mut h = Sha256::new();
    h.update(bd_identity.as_bytes());
    h.update(b"\n");
    h.update(dolt_identity.as_bytes());
    let d = h.finalize();
    d.iter().take(8).fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
}

/// A fixture's server config. `data_dir` is absolute so the server's cwd does not matter.
pub fn render_config(port: u16, data_dir: &Path) -> String {
    format!(
        "log_level: warning\n\
         listener:\n  host: 127.0.0.1\n  port: {port}\n  max_connections: 1000\n  read_timeout_millis: 1800000\n  write_timeout_millis: 1800000\n\
         data_dir: \"{}\"\n\
         behavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n",
        data_dir.display()
    )
}

/// Point a workspace's `.beads` at `port`: bd 1.2 reads `dolt-server.port` first and
/// `metadata.json`'s `dolt_server_port` as the fallback; both are rewritten.
pub fn point_beads_at(beads: &Path, port: u16) -> io::Result<()> {
    let meta = beads.join("metadata.json");
    let text = fs::read_to_string(&meta)?;
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", meta.display()),
        )
    })?;
    let obj = v.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not an object", meta.display()),
        )
    })?;
    obj.insert("dolt_server_port".into(), serde_json::json!(port));
    write_atomic(&meta, &serde_json::to_string_pretty(&v).unwrap_or_default())?;
    write_atomic(&beads.join("dolt-server.port"), &port.to_string())
}

/// Is `cmdline` (NUL-separated) the sql-server that `config` configures?
pub fn cmdline_is_server(cmdline: &[u8], config: &Path) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    let want = config.as_os_str().as_encoded_bytes();
    args.iter().any(|a| *a == b"sql-server") && args.iter().any(|a| *a == want)
}

/// `State:` from /proc/<pid>/status text; `Z`/`X` read as dead.
pub fn status_is_dead(status: &str) -> bool {
    status
        .lines()
        .find_map(|l| l.strip_prefix("State:"))
        .map(|s| matches!(s.trim_start().chars().next(), Some('Z') | Some('X')))
        .unwrap_or(true)
}

/// utime + stime ticks from /proc/<pid>/stat text (fields 14, 15; comm may hold spaces).
pub fn stat_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After `)`: state is field 3, so utime (14) is index 11 and stime (15) index 12.
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

pub fn report(pairs: &[(&str, String)]) -> String {
    pairs.iter().fold(String::new(), |mut s, (k, v)| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{k}={v}\n"));
        s
    })
}

// ---- small IO helpers ----------------------------------------------------------------

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn write_atomic(p: &Path, text: &str) -> io::Result<()> {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, p)
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// `cp -a` semantics for the small trees involved (directories, files, symlinks).
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)
    } else if m.is_dir() {
        fs::create_dir_all(dst)?;
        fs::set_permissions(dst, m.permissions())?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn remove_tree(p: &Path) {
    let _ = fs::remove_dir_all(p);
}

fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => !status_is_dead(&s),
        Err(_) => false,
    }
}

/// Ours = alive and still the sql-server for `config` (never signal a recycled pid).
fn pid_is_our_server(pid: u32, config: &Path) -> bool {
    pid_alive(pid)
        && fs::read(format!("/proc/{pid}/cmdline"))
            .map(|c| cmdline_is_server(&c, config))
            .unwrap_or(false)
}

fn signal(pid: u32, sig: i32) {
    // SAFETY: plain kill(2) on a pid we have just identified via /proc.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

/// Detach a child: own session, and mark every inherited fd >= 3 close-on-exec so the
/// child never holds a caller's `$(...)` pipe or a suite's lock fd.
fn detach(cmd: &mut Command) {
    // SAFETY: only async-signal-safe calls (setsid, syscall, fcntl) between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            let r = libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if r != 0 {
                for fd in 3..4096 {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
}

// ---- the server ----------------------------------------------------------------------

/// Is a MySQL server answering on `addr`: the connection is accepted AND the server sends
/// its protocol-10 greeting. A bare TCP connect is not readiness — the kernel completes the
/// handshake for a listening socket before the server calls accept (sp-t26yx). Bounded per
/// attempt; the caller's overall [`READY_TIMEOUT`] is unchanged.
pub fn greets(addr: &SocketAddr) -> bool {
    use std::io::Read;
    let Ok(mut s) = TcpStream::connect_timeout(addr, Duration::from_millis(200)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let mut head = [0u8; 5];
    s.read_exact(&mut head).is_ok() && is_greeting(&head)
}

/// The first five bytes of a MySQL packet stream: a 3-byte length, sequence 0, then the
/// payload's first byte, 0x0a for a protocol-10 handshake (0xff is an error packet — e.g.
/// too many connections — which is not ready).
pub fn is_greeting(head: &[u8; 5]) -> bool {
    let len = u32::from(head[0]) | u32::from(head[1]) << 8 | u32::from(head[2]) << 16;
    len > 0 && head[3] == 0 && head[4] == 0x0a
}

/// Is `p` (or its nearest existing ancestor) on a tmpfs? A test database is thrown away, so
/// its fsyncs are pure cost; on a disk-backed root they queue behind the host's writeback
/// and a `bd init` overran bd's 10 s read timeout under load (sp-t26yx).
pub fn on_tmpfs(p: &Path) -> bool {
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    let mut cur = Some(p);
    while let Some(d) = cur {
        if d.exists() {
            let Ok(c) = std::ffi::CString::new(d.as_os_str().as_encoded_bytes()) else {
                return false;
            };
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

/// Start `dolt sql-server` for `data` on `port`; returns its pid once it greets.
fn start_server(dolt: &Path, data: &Path, port: u16, log: &Path) -> Result<u32, String> {
    let cfg = data.join("config.yaml");
    fs::write(&cfg, render_config(port, data))
        .map_err(|e| format!("write {}: {e}", cfg.display()))?;
    let out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("open {}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(dolt);
    cmd.arg("sql-server")
        .arg("--config")
        .arg(&cfg)
        .current_dir(data)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    detach(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", dolt.display()))?;
    let pid = child.id();
    drop(child); // never waited here: the server outlives this process
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let t0 = Instant::now();
    while t0.elapsed() < READY_TIMEOUT {
        if !pid_alive(pid) {
            return Err(format!(
                "dolt sql-server on port {port} exited before accepting"
            ));
        }
        if greets(&addr) && pid_alive(pid) {
            return Ok(pid);
        }
        sleep(Duration::from_millis(20));
    }
    signal(pid, libc::SIGKILL);
    Err(format!(
        "dolt sql-server on port {port} not ready after {}s",
        READY_TIMEOUT.as_secs()
    ))
}

/// Start on `prefer` when given, else (or when that fails) on fresh free ports.
fn start_server_any(
    dolt: &Path,
    data: &Path,
    prefer: Option<u16>,
    log: &Path,
) -> Result<(u32, u16), String> {
    let mut last = String::new();
    for i in 0..PORT_ATTEMPTS {
        let port = match (i, prefer) {
            (0, Some(p)) => p,
            _ => free_port().map_err(|e| format!("no free port: {e}"))?,
        };
        match start_server(dolt, data, port, log) {
            Ok(pid) => return Ok((pid, port)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// SIGTERM (Dolt flushes its working set), wait, then SIGKILL. Only a pid we can identify.
fn stop_server(pid: u32, config: &Path) {
    if !pid_is_our_server(pid, config) {
        return;
    }
    signal(pid, libc::SIGTERM);
    let t0 = Instant::now();
    while t0.elapsed() < STOP_TIMEOUT {
        if !pid_alive(pid) {
            return;
        }
        sleep(Duration::from_millis(10));
    }
    if pid_is_our_server(pid, config) {
        signal(pid, libc::SIGKILL);
    }
}

fn clk_tck() -> u64 {
    // SAFETY: sysconf is always safe to call.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 {
        t as u64
    } else {
        100
    }
}

fn server_cpu_ms(pid: u32) -> Option<u64> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(stat_cpu_ticks(&s)? * 1000 / clk_tck())
}

// ---- template ------------------------------------------------------------------------

pub struct Tools {
    pub bd: PathBuf,
    pub dolt: PathBuf,
}

impl Tools {
    pub fn resolve(bd: &str, dolt: &str) -> Result<Tools, String> {
        let path = std::env::var("PATH").unwrap_or_default();
        let bd = resolve_exe(bd, &path)
            .map(|p| see_through_meter(p, bd, &path))
            .ok_or_else(|| format!("bd not found: {bd}"))?;
        let dolt = resolve_exe(dolt, &path).ok_or_else(|| format!("dolt not found: {dolt}"))?;
        Ok(Tools { bd, dolt })
    }

    pub fn key(&self) -> Result<String, String> {
        let b = exe_identity(&self.bd).map_err(|e| format!("{}: {e}", self.bd.display()))?;
        let d = exe_identity(&self.dolt).map_err(|e| format!("{}: {e}", self.dolt.display()))?;
        Ok(template_key(&b, &d))
    }
}

struct Flock(#[allow(dead_code)] fs::File);

impl Flock {
    fn exclusive(p: &Path) -> io::Result<Flock> {
        let f = fs::OpenOptions::new().create(true).append(true).open(p)?;
        use std::os::fd::AsRawFd;
        // SAFETY: flock on an fd we own for the life of the guard.
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Flock(f))
    }
}

/// The template for `tools` under `root`, building it once if absent.
pub fn ensure_template(root: &Path, tools: &Tools) -> Result<PathBuf, String> {
    let key = tools.key()?;
    let dir = root.join(format!("template-{key}"));
    if dir.join("READY").is_file() {
        return Ok(dir);
    }
    fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {e}", root.display()))?;
    let _lock = Flock::exclusive(&root.join(format!("template-{key}.lock")))
        .map_err(|e| format!("lock template: {e}"))?;
    if dir.join("READY").is_file() {
        return Ok(dir);
    }
    let tmp = root.join(format!("template-{key}.build-{}", std::process::id()));
    remove_tree(&tmp);
    let r = build_template(&tmp, tools);
    if let Err(e) = r {
        remove_tree(&tmp);
        return Err(e);
    }
    remove_tree(&dir);
    fs::rename(&tmp, &dir).map_err(|e| format!("rename template: {e}"))?;
    Ok(dir)
}

fn build_template(tmp: &Path, tools: &Tools) -> Result<(), String> {
    let data = tmp.join("data");
    let ws = tmp.join("ws");
    for d in [&data, &ws] {
        fs::create_dir_all(d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
    }
    let log = tmp.join("server.log");
    let (pid, port) = start_server_any(&tools.dolt, &data, None, &log)?;
    let cfg = data.join("config.yaml");
    let out = Command::new("env")
        .arg("-i")
        .arg(format!(
            "PATH={}",
            std::env::var("PATH").unwrap_or_default()
        ))
        .arg(format!(
            "HOME={}",
            std::env::var("HOME").unwrap_or_default()
        ))
        .arg("TERM=dumb")
        .arg("BD_NON_INTERACTIVE=1")
        .arg(&tools.bd)
        .args([
            "init",
            "--non-interactive",
            "--prefix",
            "sp",
            "--skip-agents",
            "--skip-hooks",
            "--server",
            "--server-host",
            "127.0.0.1",
            "--server-port",
        ])
        .arg(port.to_string())
        .args(["--database", DATABASE, "--external", "-q"])
        .current_dir(&ws)
        .stdin(Stdio::null())
        .output();
    stop_server(pid, &cfg);
    let out = out.map_err(|e| format!("spawn {}: {e}", tools.bd.display()))?;
    if !out.status.success() {
        return Err(format!(
            "bd init (server) failed (rc={}):\n{}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if !ws.join(".beads/metadata.json").is_file() {
        return Err("bd init left no .beads/metadata.json".into());
    }
    let _ = fs::remove_file(&log);
    fs::write(tmp.join("READY"), "").map_err(|e| e.to_string())
}

// ---- fixtures ------------------------------------------------------------------------

pub struct Up {
    pub name: String,
    pub fixture: PathBuf,
    pub ws: PathBuf,
    pub port: u16,
    pub pid: u32,
    pub ms: u128,
}

fn fresh_from_template(fx: &Path, tpl: &Path) -> Result<(), String> {
    let data = fx.join("data");
    let beads = fx.join("ws/.beads");
    remove_tree(&data);
    remove_tree(&beads);
    fs::create_dir_all(fx.join("ws")).map_err(|e| e.to_string())?;
    copy_tree(&tpl.join("data"), &data).map_err(|e| format!("copy template data: {e}"))?;
    copy_tree(&tpl.join("ws/.beads"), &beads).map_err(|e| format!("copy template .beads: {e}"))
}

fn launch(fx: &Path, prefer: Option<u16>) -> Result<(u32, u16), String> {
    let dolt = PathBuf::from(read_trim(&fx.join("dolt")).ok_or("fixture has no dolt record")?);
    let (pid, port) = start_server_any(&dolt, &fx.join("data"), prefer, &fx.join("server.log"))?;
    point_beads_at(&fx.join("ws/.beads"), port).map_err(|e| format!("point .beads: {e}"))?;
    fs::write(fx.join("server.pid"), pid.to_string()).map_err(|e| e.to_string())?;
    fs::write(fx.join("server.port"), port.to_string()).map_err(|e| e.to_string())?;
    Ok((pid, port))
}

pub fn up(
    root: &Path,
    tpl: &Path,
    tools: &Tools,
    tag: &str,
    owner: u32,
    self_exe: Option<&Path>,
) -> Result<Up, String> {
    let t0 = Instant::now();
    let secs = now_ms() / 1000;
    let name = format!("sptest_{tag}_{secs}_{}", std::process::id());
    let fx = root.join(format!("fx-{name}"));
    fs::create_dir_all(&fx).map_err(|e| format!("mkdir {}: {e}", fx.display()))?;
    let r = (|| {
        fs::write(fx.join("template"), tpl.display().to_string()).map_err(|e| e.to_string())?;
        fs::write(fx.join("dolt"), tools.dolt.display().to_string()).map_err(|e| e.to_string())?;
        fs::write(fx.join("resets"), "0").map_err(|e| e.to_string())?;
        fs::write(fx.join("started_ms"), now_ms().to_string()).map_err(|e| e.to_string())?;
        fresh_from_template(&fx, tpl)?;
        launch(&fx, None)
    })();
    let (pid, port) = match r {
        Ok(v) => v,
        Err(e) => {
            let _ = down(&fx);
            return Err(e);
        }
    };
    if owner > 0 {
        if let Some(exe) = self_exe {
            let mut cmd = Command::new(exe);
            cmd.args(["testdb", "reap", "--fixture"])
                .arg(&fx)
                .args(["--owner", &owner.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            detach(&mut cmd);
            if let Err(e) = cmd.spawn() {
                let _ = down(&fx);
                return Err(format!("spawn reaper: {e}"));
            }
        }
    }
    Ok(Up {
        name,
        ws: fx.join("ws"),
        fixture: fx,
        port,
        pid,
        ms: t0.elapsed().as_millis(),
    })
}

fn fixture_pid(fx: &Path) -> Option<u32> {
    read_trim(&fx.join("server.pid"))?.parse().ok()
}

pub fn reset(fx: &Path) -> Result<(u32, u16, u128), String> {
    let t0 = Instant::now();
    if !fx.join("ws").is_dir() {
        return Err(format!("no fixture at {}", fx.display()));
    }
    let cfg = fx.join("data/config.yaml");
    if let Some(pid) = fixture_pid(fx) {
        stop_server(pid, &cfg);
    }
    let tpl =
        PathBuf::from(read_trim(&fx.join("template")).ok_or("fixture has no template record")?);
    fresh_from_template(fx, &tpl)?;
    let prefer = read_trim(&fx.join("server.port")).and_then(|p| p.parse().ok());
    let (pid, port) = launch(fx, prefer)?;
    let n: u64 = read_trim(&fx.join("resets"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let _ = fs::write(fx.join("resets"), (n + 1).to_string());
    Ok((pid, port, t0.elapsed().as_millis()))
}

pub struct Down {
    pub cpu_ms: Option<u64>,
    pub life_ms: Option<u128>,
    pub resets: Option<u64>,
}

/// Stop the server and remove the fixture. A missing fixture is success.
pub fn down(fx: &Path) -> Result<Down, String> {
    if !fx.exists() {
        return Ok(Down {
            cpu_ms: None,
            life_ms: None,
            resets: None,
        });
    }
    let _ = fs::write(fx.join("DOWN"), "");
    let cfg = fx.join("data/config.yaml");
    let mut cpu_ms = None;
    if let Some(pid) = fixture_pid(fx) {
        if pid_is_our_server(pid, &cfg) {
            cpu_ms = server_cpu_ms(pid);
        }
        stop_server(pid, &cfg);
    }
    let life_ms = read_trim(&fx.join("started_ms"))
        .and_then(|s| s.parse::<u128>().ok())
        .map(|s| now_ms().saturating_sub(s));
    let resets = read_trim(&fx.join("resets")).and_then(|s| s.parse().ok());
    // Rename first so a concurrent reader never sees a half-removed fixture.
    let gone = fx.with_extension(format!("del-{}", std::process::id()));
    if fs::rename(fx, &gone).is_ok() {
        remove_tree(&gone);
    } else {
        remove_tree(fx);
    }
    Ok(Down {
        cpu_ms,
        life_ms,
        resets,
    })
}

/// The watchdog: take the fixture down when its owner dies; exit when it is gone.
pub fn reap(fx: &Path, owner: u32, poll: Duration) {
    if owner == 0 {
        return;
    }
    loop {
        if !fx.exists() || fx.join("DOWN").exists() {
            return;
        }
        if !pid_alive(owner) {
            let _ = down(fx);
            return;
        }
        sleep(poll);
    }
}

// ---- embedded fixtures (sp-k2oyn, DESIGN-testdb.md §6) --------------------------------
//
// A private tmpdir fixture on bd's embedded Dolt engine: no server, cleanup is rm -rf.
// This is the logic `testdb.sh`'s embedded branch of `testdb_up`/`testdb_reset`/
// `testdb_drop` used to carry directly; the bash file now marshals arguments to these
// subcommands and exports the KV report, exactly as it already did for server mode
// (§2.3). Sourcing still has to happen in bash — only sourcing can set variables in the
// calling shell — so the file itself is not removed, only what it does inline.

fn unique_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "testenv-testdb-{tag}-{}-{}-{n}",
        std::process::id(),
        now_ms()
    ))
}

/// `env -i PATH HOME TERM=dumb BD_NON_INTERACTIVE=1 <bd> init --non-interactive --prefix sp
/// --skip-agents --skip-hooks -q` in `cwd` — the same clean-environment init `testdb.sh`
/// always used (embedded and the server template both use it). `bd` is passed as given
/// (a bare name is resolved by `env`'s own PATH search, exactly as a bare command in bash
/// would be); the caller resolves an absolute path separately for reporting.
fn embedded_bd_init(bd: &str, cwd: &Path) -> Result<(), String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let out = Command::new("env")
        .arg("-i")
        .arg(format!("PATH={path}"))
        .arg(format!("HOME={home}"))
        .arg("TERM=dumb")
        .arg("BD_NON_INTERACTIVE=1")
        .arg(bd)
        .args([
            "init",
            "--non-interactive",
            "--prefix",
            "sp",
            "--skip-agents",
            "--skip-hooks",
            "-q",
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("spawn {bd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "bd init failed (rc={}):\n{}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// Does bd's embedded engine work on this host? A throwaway `bd init`, diagnostics
/// discarded — this is a probe (testdb.sh memoises the boolean for its own session), not a
/// fault report. False on anything that stops it: `bd` not found, init refuses, no /tmp.
pub fn embedded_check(bd: &str) -> bool {
    let dir = unique_dir("check");
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let ok = embedded_bd_init(bd, &dir).is_ok();
    remove_tree(&dir);
    ok
}

#[derive(Debug)]
pub struct EmbeddedUp {
    pub name: String,
    pub dir: PathBuf,
    pub baseline: PathBuf,
    pub bin: Option<PathBuf>,
    /// What the caller should export as `SPIRA_BD`/`TESTDB_BD`: the absolute path `bd`
    /// resolved to, or `bd` itself if that lookup fails despite init succeeding (matches
    /// testdb.sh's `${_bd_real:-$TESTDB_BD}` fallback).
    pub bd: String,
}

/// A fresh embedded fixture: `bd init` into a private dir, a baseline snapshot of the
/// `.beads` it wrote (so `reset` never re-pays init's cost), and a PATH shim directory
/// holding a `bd` symlink (so a suite's bare `bd` calls resolve to the same binary this
/// fixture was built with, surviving conf.sh's PATH rebuild via SPIRA_PATH — call site).
pub fn embedded_up(bd: &str, tag: &str) -> Result<EmbeddedUp, String> {
    let dir = unique_dir("fx");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    if let Err(e) = embedded_bd_init(bd, &dir) {
        remove_tree(&dir);
        // The dir is named so a caller (and a test) can confirm it is really gone,
        // not just that this call said so.
        return Err(format!("{e}\n(fixture dir removed: {})", dir.display()));
    }
    if !dir.join(".beads").is_dir() {
        remove_tree(&dir);
        return Err(format!(
            "bd init left no .beads directory\n(fixture dir removed: {})",
            dir.display()
        ));
    }

    let baseline = unique_dir("bl");
    if let Err(e) = fs::create_dir_all(&baseline) {
        remove_tree(&dir);
        return Err(format!("mkdir {}: {e}", baseline.display()));
    }
    if let Err(e) = copy_tree(&dir.join(".beads"), &baseline.join(".beads")) {
        remove_tree(&dir);
        remove_tree(&baseline);
        return Err(format!("baseline snapshot: {e}"));
    }
    if !baseline.join(".beads").is_dir() {
        remove_tree(&dir);
        remove_tree(&baseline);
        return Err("baseline snapshot is empty".into());
    }

    let path = std::env::var("PATH").unwrap_or_default();
    let bd_abs = locate_exe(bd, &path);
    let bin = bd_abs.as_ref().and_then(|abs| {
        let b = unique_dir("bin");
        if fs::create_dir_all(&b).is_err() {
            return None;
        }
        if std::os::unix::fs::symlink(abs, b.join("bd")).is_ok() {
            Some(b)
        } else {
            remove_tree(&b);
            None
        }
    });

    let secs = now_ms() / 1000;
    Ok(EmbeddedUp {
        name: format!("sptest_{tag}_{secs}_{}", std::process::id()),
        dir,
        baseline,
        bin,
        bd: bd_abs
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| bd.to_string()),
    })
}

#[derive(Debug)]
pub struct EmbeddedBorrow {
    pub dir: PathBuf,
}

/// A borrower's private copy of a shared embedded fixture's baseline `.beads` — never a
/// reset against the database other borrowers are reading (that misuse is what sp-v2lqd
/// removed from server mode; embedded mode never shared a server, but it did share a
/// baseline directory borrowers copy from, never write to).
pub fn embedded_borrow(baseline: &Path) -> Result<EmbeddedBorrow, String> {
    let dir = unique_dir("priv");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    if let Err(e) = copy_tree(&baseline.join(".beads"), &dir.join(".beads")) {
        remove_tree(&dir);
        return Err(format!("could not copy baseline: {e}"));
    }
    if !dir.join(".beads").is_dir() {
        remove_tree(&dir);
        return Err("shared fixture baseline is empty — fixture collapsed".into());
    }
    Ok(EmbeddedBorrow { dir })
}

/// Back to exactly the baseline: copy to a fresh sibling, rename the old `.beads` aside,
/// rename the new one into place. Never `rm -rf` then `cp` — on an overlay2 filesystem
/// `rm -rf` can leave `.beads` partially removed (a kernel whiteout/readdir race), and a
/// `cp` into a directory that still exists nests instead of replacing it, compounding on
/// every later reset (sp-i0vz5). `rename(2)` is atomic and does not recurse.
pub fn embedded_reset(dir: &Path, baseline: &Path) -> Result<(), String> {
    let src = baseline.join(".beads");
    if !src.is_dir() {
        return Err(format!("baseline has no .beads: {}", baseline.display()));
    }
    let new = dir.join(".beads.new");
    let old = dir.join(".beads.old");
    remove_tree(&new);
    remove_tree(&old);
    if let Err(e) = copy_tree(&src, &new) {
        remove_tree(&new);
        return Err(format!("copy baseline: {e}"));
    }
    let cur = dir.join(".beads");
    let _ = fs::rename(&cur, &old); // no-op when .beads is absent, as in bash
    fs::rename(&new, &cur).map_err(|e| format!("rename into place: {e}"))?;
    remove_tree(&old); // best-effort; the live database is already correct either way
    Ok(())
}

/// Detach `rm -rf p` into its own session so a slow overlay2 delete (whiteout unlinks
/// measured at ~19s) never blocks the caller past a Podman exec timeout — the same reason
/// `testdb_drop` backgrounded this with a bare `&` before it moved here.
fn background_rm(p: &Path) {
    let mut cmd = Command::new("rm");
    cmd.arg("-rf")
        .arg(p)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    let _ = cmd.spawn();
}

/// Remove every embedded leftover (fixture dir, baseline snapshot, PATH shim dir). Rename
/// each aside first so a concurrent reader never sees a half-removed directory, then
/// background the actual delete. A missing or empty path is skipped, not an error — this
/// is teardown, called from an EXIT trap, and it must not itself fail the suite.
pub fn embedded_down(paths: &[PathBuf]) {
    for p in paths {
        if p.as_os_str().is_empty() || !p.exists() {
            continue;
        }
        let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(format!(".del-{}", std::process::id()));
        let gone = p.with_file_name(name);
        let target = if fs::rename(p, &gone).is_ok() {
            gone
        } else {
            p.clone()
        };
        background_rm(&target);
    }
}

// ---- CLI -----------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub cmd: String,
    pub tag: Option<String>,
    pub bd: Option<String>,
    pub dolt: Option<String>,
    pub root: Option<String>,
    pub owner: Option<u32>,
    pub fixture: Option<String>,
    pub dir: Option<String>,
    pub baseline: Option<String>,
    pub bin: Option<String>,
}

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    a.cmd = it.next().cloned().ok_or(USAGE)?;
    if !matches!(
        a.cmd.as_str(),
        "template"
            | "up"
            | "reset"
            | "down"
            | "reap"
            | "embedded-check"
            | "embedded-up"
            | "embedded-borrow"
            | "embedded-reset"
            | "embedded-down"
    ) {
        return Err(format!("testdb: unknown command: {}\n{USAGE}", a.cmd));
    }
    while let Some(k) = it.next() {
        let mut v = || {
            it.next()
                .cloned()
                .ok_or(format!("testdb: {k} needs a value"))
        };
        match k.as_str() {
            "--tag" => a.tag = Some(v()?),
            "--bd" => a.bd = Some(v()?),
            "--dolt" => a.dolt = Some(v()?),
            "--root" => a.root = Some(v()?),
            "--fixture" => a.fixture = Some(v()?),
            "--dir" => a.dir = Some(v()?),
            "--baseline" => a.baseline = Some(v()?),
            "--bin" => a.bin = Some(v()?),
            "--owner" => {
                let s = v()?;
                a.owner = Some(
                    s.parse()
                        .map_err(|_| format!("testdb: --owner: not a pid: {s}"))?,
                );
            }
            _ => return Err(format!("testdb: unknown argument: {k}\n{USAGE}")),
        }
    }
    let need = |o: &Option<String>, n: &str| {
        o.as_ref()
            .map(|_| ())
            .ok_or(format!("testdb {}: {n} is required", a.cmd))
    };
    match a.cmd.as_str() {
        "template" => {
            need(&a.bd, "--bd")?;
            need(&a.dolt, "--dolt")?;
        }
        "up" => {
            need(&a.tag, "--tag")?;
            need(&a.bd, "--bd")?;
            need(&a.dolt, "--dolt")?;
            if let Some(t) = &a.tag {
                if t.is_empty()
                    || !t
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(format!("testdb up: --tag must be [A-Za-z0-9_-]+: {t}"));
                }
            }
        }
        "reset" | "down" => need(&a.fixture, "--fixture")?,
        "reap" => {
            need(&a.fixture, "--fixture")?;
            if a.owner.is_none() {
                return Err("testdb reap: --owner is required".into());
            }
        }
        "embedded-check" => need(&a.bd, "--bd")?,
        "embedded-up" => {
            need(&a.tag, "--tag")?;
            need(&a.bd, "--bd")?;
        }
        "embedded-borrow" => need(&a.baseline, "--baseline")?,
        "embedded-reset" => {
            need(&a.dir, "--dir")?;
            need(&a.baseline, "--baseline")?;
        }
        // embedded-down: every path is optional — a missing one is a no-op, so a call
        // site that already zeroed a var (e.g. server mode's TESTDB_DIR="") stays cheap.
        _ => {}
    }
    Ok(a)
}

fn root_of(a: &Args) -> PathBuf {
    PathBuf::from(
        a.root
            .clone()
            .or_else(|| std::env::var("TESTDB_ROOT").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| DEFAULT_ROOT.to_string()),
    )
}

fn emit(text: &str) {
    let mut o = io::stdout().lock();
    let _ = o.write_all(text.as_bytes());
    let _ = o.flush();
}

pub fn main(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("{m}");
            return 2;
        }
    };
    let fail = |m: String| {
        eprintln!("testdb {}: {m}", a.cmd);
        1
    };
    match a.cmd.as_str() {
        "template" | "up" => {
            let tools = match Tools::resolve(
                a.bd.as_deref().unwrap_or("bd"),
                a.dolt.as_deref().unwrap_or("dolt"),
            ) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let root = root_of(&a);
            if std::env::var("TESTDB_REQUIRE_TMPFS").as_deref() == Ok("1") && !on_tmpfs(&root) {
                return fail(format!(
                    "root {} is not on a tmpfs (TESTDB_REQUIRE_TMPFS=1) — refusing to put a throwaway database on a disk it would fsync to",
                    root.display()
                ));
            }
            let tpl = match ensure_template(&root, &tools) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            if a.cmd == "template" {
                emit(&report(&[("TESTDB_TEMPLATE", tpl.display().to_string())]));
                return 0;
            }
            let exe = std::env::current_exe().ok();
            match up(
                &root,
                &tpl,
                &tools,
                a.tag.as_deref().unwrap_or("x"),
                a.owner.unwrap_or(0),
                exe.as_deref(),
            ) {
                Ok(u) => {
                    let bd_name = a.bd.as_deref().unwrap_or("bd");
                    let path = std::env::var("PATH").unwrap_or_default();
                    let bd = locate_exe(bd_name, &path)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| bd_name.to_string());
                    emit(&report(&[
                        ("TESTDB_NAME", u.name),
                        ("TESTDB_DIR", u.ws.display().to_string()),
                        ("TESTDB_FIXTURE", u.fixture.display().to_string()),
                        ("TESTDB_SERVER_PORT", u.port.to_string()),
                        ("TESTDB_SERVER_PID", u.pid.to_string()),
                        ("TESTDB_UP_MS", u.ms.to_string()),
                        ("TESTDB_BD", bd),
                    ]));
                    0
                }
                Err(e) => fail(e),
            }
        }
        "reset" => match reset(Path::new(a.fixture.as_deref().unwrap_or(""))) {
            Ok((pid, port, ms)) => {
                emit(&report(&[
                    ("TESTDB_SERVER_PORT", port.to_string()),
                    ("TESTDB_SERVER_PID", pid.to_string()),
                    ("TESTDB_RESET_MS", ms.to_string()),
                ]));
                0
            }
            Err(e) => fail(e),
        },
        "down" => match down(Path::new(a.fixture.as_deref().unwrap_or(""))) {
            Ok(d) => {
                let s = |v: Option<String>| v.unwrap_or_else(|| "-".into());
                emit(&report(&[
                    ("TESTDB_SERVER_CPU_MS", s(d.cpu_ms.map(|v| v.to_string()))),
                    ("TESTDB_LIFE_MS", s(d.life_ms.map(|v| v.to_string()))),
                    ("TESTDB_RESETS", s(d.resets.map(|v| v.to_string()))),
                ]));
                0
            }
            Err(e) => fail(e),
        },
        "reap" => {
            reap(
                Path::new(a.fixture.as_deref().unwrap_or("")),
                a.owner.unwrap_or(0),
                Duration::from_secs(1),
            );
            0
        }
        "embedded-check" => {
            if embedded_check(a.bd.as_deref().unwrap_or("")) {
                0
            } else {
                1
            }
        }
        "embedded-up" => match embedded_up(a.bd.as_deref().unwrap_or(""), a.tag.as_deref().unwrap_or("x"))
        {
            Ok(u) => {
                emit(&report(&[
                    ("TESTDB_NAME", u.name),
                    ("TESTDB_DIR", u.dir.display().to_string()),
                    ("TESTDB_BASELINE", u.baseline.display().to_string()),
                    (
                        "TESTDB_BIN",
                        u.bin.map(|p| p.display().to_string()).unwrap_or_default(),
                    ),
                    ("TESTDB_BD", u.bd),
                ]));
                0
            }
            Err(e) => fail(e),
        },
        "embedded-borrow" => {
            match embedded_borrow(Path::new(a.baseline.as_deref().unwrap_or(""))) {
                Ok(b) => {
                    emit(&report(&[(
                        "TESTDB_PRIVATE_DIR",
                        b.dir.display().to_string(),
                    )]));
                    0
                }
                Err(e) => fail(e),
            }
        }
        "embedded-reset" => match embedded_reset(
            Path::new(a.dir.as_deref().unwrap_or("")),
            Path::new(a.baseline.as_deref().unwrap_or("")),
        ) {
            Ok(()) => 0,
            Err(e) => fail(e),
        },
        "embedded-down" => {
            let paths: Vec<PathBuf> = [&a.dir, &a.baseline, &a.bin]
                .into_iter()
                .filter_map(|o| o.clone())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            embedded_down(&paths);
            0
        }
        _ => 2,
    }
}

#[cfg(test)]
mod tests;
