//! The ports against the host (DESIGN-suites.md §4, §5).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::ports::*;

/// host-check.sh's wall (DESIGN-suites.md §6 D5).
pub const HOST_CHECK_WALL: Duration = Duration::from_secs(30);

pub struct Real {
    /// The PATH this run was given; host-check.sh is looked up on it at each call.
    path: String,
}

impl Real {
    pub fn new(env: &dyn Fn(&str) -> Option<String>) -> Real {
        let path = env("PATH").unwrap_or_default();
        Real { path }
    }
}

/// Wait for `child` up to `wall`; kill it past the wall. Some(exit code) when it exited.
fn wait_wall(child: &mut Child, wall: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) if start.elapsed() < wall => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl Clock for Real {
    fn now(&self) -> u64 {
        crate::batch::now_epoch()
    }
}

impl Emit for Real {
    fn out(&self, line: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
    fn err(&self, line: &str) {
        eprintln!("{line}");
    }
}

impl HostCheck for Real {
    fn count(&self, flag: &str) -> Option<String> {
        // host-check.sh on the launcher's PATH (sp-gypjk); absent or not executable is None.
        let script = crate::util::which_in(&self.path, "host-check.sh")?;
        let envs = spira_config::release_env::child_path_env_for_process().ok()?;
        // batch-job: child is spawned or exec-replaced, not awaited under a deadline
        let mut child = Command::new("bash")
            .envs(envs)
            .arg(&script)
            .arg(flag)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut so = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let rc = wait_wall(&mut child, HOST_CHECK_WALL);
        let out = reader.join().unwrap_or_default();
        (rc == Some(0))
            .then(|| out.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
    }
}
