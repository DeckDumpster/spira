//! The one process the service starts: the Concierge's wake verb, one try that delivers or refuses at once.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The release's own `concierge.sh`; never the repo map's checkout, which is a working tree.
pub fn concierge_script(env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let script = spira_config::resolve::checkout_root(env)?.join("concierge.sh");
    if script.is_file() {
        Ok(script)
    } else {
        Err(format!("{} is not a file", script.display()))
    }
}

/// [`concierge_wake`] on the release's script.
pub fn release_wake(env: &BTreeMap<String, String>, line: &str, deadline: Duration) -> Result<String, String> {
    concierge_wake(&concierge_script(env)?, line, deadline)
}

/// [`release_wake`] for this process.
pub fn release_wake_for_process(line: &str, deadline: Duration) -> Result<String, String> {
    release_wake(&std::env::vars().collect(), line, deadline)
}

/// Runs the wake and kills its whole process group once `deadline` passes, so no wake outlives its effect.
pub fn concierge_wake(concierge: &Path, line: &str, deadline: Duration) -> Result<String, String> {
    // FORK-EDGE: concierge.sh wake types into
    // the pane with tmux send-keys, which nothing built here may do; it stays the operator tool's.
    let mut child = Command::new(concierge) // batch-job: one try: one settle, then deliver or refuse
        .arg("wake")
        .arg(line)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", concierge.display()))?;
    let mut pipe = child.stderr.take().ok_or("no stderr pipe")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() >= deadline {
            let pid = i32::try_from(child.id()).unwrap_or(0);
            if pid > 0 {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} wake exceeded its {deadline:?} deadline and was killed", concierge.display()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if status.success() {
        return Ok("woke".into());
    }
    Err(rx.recv_timeout(Duration::from_millis(200)).unwrap_or_default().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_env(dir: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([("SPIRA_HOME".to_string(), dir.join("rel/spira").display().to_string())])
    }

    #[test_id(tfs3kuc)]
    #[test]
    fn the_script_is_the_releases_not_the_repo_maps_home_row() {
        let dir = testkit::TempDir::new("wake-script");
        std::fs::create_dir_all(dir.join("rel/spira")).unwrap();
        std::fs::create_dir_all(dir.join("checkout")).unwrap();
        std::fs::write(dir.join("checkout/concierge.sh"), "").unwrap();
        std::fs::write(dir.join("rel/concierge.sh"), "").unwrap();
        assert_eq!(concierge_script(&release_env(&dir)).unwrap(), dir.join("rel/concierge.sh"));
    }

    #[test_id(t3ttj7w)]
    #[test]
    fn a_release_without_the_script_fails_naming_the_path() {
        let dir = testkit::TempDir::new("wake-noscript");
        std::fs::create_dir_all(dir.join("rel/spira")).unwrap();
        std::fs::create_dir_all(dir.join("checkout")).unwrap();
        std::fs::write(dir.join("checkout/concierge.sh"), "").unwrap();
        let err = concierge_script(&release_env(&dir)).unwrap_err();
        assert!(err.contains(&dir.join("rel/concierge.sh").display().to_string()), "{err}");
    }

    #[test_id(tjfbqog)]
    #[test]
    fn the_release_script_runs_and_the_home_row_script_never_does() {
        let dir = testkit::TempDir::new("wake-run");
        std::fs::create_dir_all(dir.join("rel/spira")).unwrap();
        std::fs::create_dir_all(dir.join("checkout")).unwrap();
        let (home_marker, rel_marker) = (dir.join("home-ran"), dir.join("rel-ran"));
        testkit::write_exe(dir.join("checkout/concierge.sh"), &format!("#!/bin/sh\ntouch {}\n", home_marker.display()));
        testkit::write_exe(dir.join("rel/concierge.sh"), &format!("#!/bin/sh\ntouch {}\n", rel_marker.display()));
        release_wake(&release_env(&dir), "line", Duration::from_secs(10)).unwrap();
        assert!(rel_marker.exists() && !home_marker.exists());
    }

    #[test_id(tpa9zvj)]
    #[test]
    fn a_wake_that_outlives_its_deadline_is_killed_and_reported_failed() {
        let dir = testkit::TempDir::new("wake-deadline");
        let script = dir.join("concierge.sh");
        let pid_file = dir.join("pid");
        testkit::write_exe(&script, &format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", pid_file.display()));
        let r = concierge_wake(&script, "line", Duration::from_millis(500));
        assert!(r.is_err());
        let pid: i32 = std::fs::read_to_string(&pid_file).expect("the script started").trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the wake outlived its deadline");
    }
}
