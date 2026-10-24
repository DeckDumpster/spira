use regex::Regex;
use std::io::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PLAYWRIGHT_VERSION: &str = "1.64.1";
const AUTHORIZE_JS: &str = include_str!("../authorize.mjs");
const URL_WAIT: Duration = Duration::from_secs(60);
const EXIT_WAIT: Duration = Duration::from_secs(90);

pub struct Tools {
    pub broker: String,
    pub claude: String,
    pub podman: String,
}

impl Default for Tools {
    fn default() -> Self {
        Tools { broker: "broker".into(), claude: "claude".into(), podman: "podman".into() }
    }
}

pub fn login_url(base: &str, code: &str) -> Result<String, String> {
    let base = base.trim();
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err(format!("login URL base {base:?} is not an http(s) URL"));
    }
    Ok(format!("{base}{code}"))
}

fn origin(url: &str) -> Result<String, String> {
    let after = url.find("://").map(|i| i + 3).ok_or("no scheme")?;
    let end = url[after..].find('/').map_or(url.len(), |i| after + i);
    Ok(url[..end].to_string())
}

pub fn mask(text: &str) -> String {
    let esc = Regex::new(r"\x1b\][^\x07]*\x07|\x1b\[[0-9;?]*[A-Za-z]").unwrap();
    let long = Regex::new(r"[A-Za-z0-9_\-]{20,}").unwrap();
    long.replace_all(&esc.replace_all(text, ""), "<x>").into_owned()
}

fn mint_code(t: &Tools) -> Result<String, String> {
    let out = Command::new(&t.broker)
        .arg("identity-login-code")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", t.broker))?;
    if !out.status.success() {
        return Err(format!("{} identity-login-code refused ({})", t.broker, out.status));
    }
    let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if code.is_empty() || code.contains(char::is_whitespace) {
        return Err("the broker returned no single-token login code".into());
    }
    Ok(code)
}

struct Pty {
    master: OwnedFd,
}

impl Pty {
    fn spawn(mut cmd: Command) -> Result<(Pty, std::process::Child), String> {
        let (mut m, mut s) = (0, 0);
        if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) } != 0 {
            return Err(format!("openpty: {}", std::io::Error::last_os_error()));
        }
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        let dup = |fd: &OwnedFd| fd.try_clone().map_err(|e| format!("dup: {e}"));
        cmd.stdin(Stdio::from(dup(&slave)?)).stdout(Stdio::from(dup(&slave)?)).stderr(Stdio::from(slave));
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
        Ok((Pty { master }, child))
    }

    fn read_until(&self, buf: &mut String, wait: Duration, until: Option<&Regex>) {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            let mut p = libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut p, 1, 500) } <= 0 {
                continue;
            }
            let mut chunk = [0u8; 4096];
            let n = unsafe { libc::read(self.master.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
            if n <= 0 {
                return;
            }
            buf.push_str(&String::from_utf8_lossy(&chunk[..n as usize]));
            if until.is_some_and(|re| re.is_match(buf)) {
                return;
            }
        }
    }

    fn send(&self, line: &str) {
        let bytes = format!("{line}\r");
        unsafe { libc::write(self.master.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    }
}

fn browser_step(t: &Tools, login: &str, auth: &str, id_origin: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("pilgrim-login-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let guard = Cleanup(dir.clone());
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("authorize.mjs"), AUTHORIZE_JS).map_err(|e| e.to_string())?;
    let v = PLAYWRIGHT_VERSION;
    let inner = format!(
        "cd /tmp && npm install --no-audit --no-fund --silent playwright@{v} >&2 && timeout 230 node /w/authorize.mjs"
    );
    let mut child = Command::new(&t.podman)
        .args(["run", "--rm", "-i", "--network", "host", "-v"])
        .arg(format!("{}:/w:ro", dir.display()))
        .args(["-e", "PLAYWRIGHT_MODULE=/tmp/node_modules/playwright", "-e"])
        .arg(format!("ID_ORIGIN={id_origin}"))
        .arg(format!("mcr.microsoft.com/playwright:v{v}-noble"))
        .args(["sh", "-c", &inner])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", t.podman))?;
    child.stdin.take().unwrap().write_all(format!("{login}\n{auth}\n").as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    drop(guard);
    eprint!("{}", mask(&String::from_utf8_lossy(&out.stderr)));
    let redirect = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || redirect.is_empty() {
        return Err("the browser step did not return a redirect".into());
    }
    Ok(redirect)
}

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn clear_needs_auth(home: &Path, name: &str) -> bool {
    let cache = home.join(".claude").join("mcp-needs-auth-cache.json");
    let Ok(text) = std::fs::read_to_string(&cache) else { return false };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let Some(obj) = v.as_object_mut() else { return false };
    if obj.remove(name).is_none() {
        return false;
    }
    std::fs::write(&cache, v.to_string()).is_ok()
}

pub fn run(t: &Tools, name: &str, project: &Path, base: &str) -> Result<(), String> {
    let code = mint_code(t)?;
    let login = login_url(base, &code)?;
    let id_origin = origin(base)?;
    println!("pilgrim-login: login code minted from the broker");

    let mut cmd = Command::new(&t.claude);
    cmd.args(["mcp", "login", "--no-browser", name]).current_dir(project);
    let (pty, mut child) = Pty::spawn(cmd)?;
    let url_re = Regex::new(r"https://[^\s\x07\x1b]+/authorize[^\s\x07\x1b]*").unwrap();
    let mut buf = String::new();
    pty.read_until(&mut buf, URL_WAIT, Some(&url_re));
    let Some(auth) = url_re.find(&buf).map(|m| m.as_str().to_string()) else {
        let _ = child.kill();
        return Err(format!("claude printed no authorization URL:\n{}", mask(&buf[buf.len().saturating_sub(800)..])));
    };
    println!("pilgrim-login: authorization URL received");

    let redirect = match browser_step(t, &login, &auth, &id_origin) {
        Ok(r) => r,
        Err(e) => {
            let _ = child.kill();
            return Err(e);
        }
    };
    pty.send(&redirect);
    pty.read_until(&mut buf, EXIT_WAIT, None);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            _ if Instant::now() > deadline => {
                let _ = child.kill();
                break None;
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let tail = mask(&buf[buf.len().saturating_sub(700)..]);
    match status {
        Some(s) if s.success() => {
            println!("pilgrim-login: claude mcp login succeeded\n{tail}");
            if let Some(home) = std::env::var_os("HOME") {
                if clear_needs_auth(Path::new(&home), name) {
                    println!("pilgrim-login: cleared the cached needs-auth for {name}");
                }
            }
            Ok(())
        }
        other => Err(format!("claude mcp login did not succeed ({other:?})\n{tail}")),
    }
}
