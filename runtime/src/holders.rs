//! The processes using a directory tree as their working directory, read from `/proc`.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub comm: String,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (pid {})", self.comm, self.pid)
    }
}

/// Every process under `proc_dir` whose working directory is `tree` or inside it, less `spare`.
pub fn holders(proc_dir: &Path, tree: &Path, spare: &[u32]) -> Vec<Holder> {
    let Ok(rd) = std::fs::read_dir(proc_dir) else { return vec![] };
    let mut out: Vec<Holder> = rd
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_string_lossy().parse().ok()?;
            if spare.contains(&pid) {
                return None;
            }
            let cwd: PathBuf = std::fs::read_link(e.path().join("cwd")).ok()?;
            if !cwd.starts_with(tree) {
                return None;
            }
            let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
            Some(Holder { pid, comm: comm.trim().to_string() })
        })
        .collect();
    out.sort_by_key(|h| h.pid);
    out
}

fn signal(sig: &str, pid: u32) {
    let _ = std::process::Command::new("timeout")
        .args(["5", "kill", sig, &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status();
}

/// Ends every holder of `tree`: TERM, then KILL for those still there after `grace`. `Err`
/// names a holder that outlived the KILL, so the caller does not free a tree still in use.
pub fn stop_holders(proc_dir: &Path, tree: &Path, spare: &[u32], grace: Duration) -> Result<Vec<Holder>, String> {
    let stopped = holders(proc_dir, tree, spare);
    let wait = |until: Duration| {
        let step = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        while waited < until && !holders(proc_dir, tree, spare).is_empty() {
            std::thread::sleep(step);
            waited += step;
        }
    };
    for h in &stopped {
        signal("-TERM", h.pid);
    }
    wait(grace);
    for h in holders(proc_dir, tree, spare) {
        signal("-KILL", h.pid);
    }
    wait(grace);
    let left = holders(proc_dir, tree, spare);
    if left.is_empty() {
        Ok(stopped)
    } else {
        Err(format!("{} still running after its stop", left.iter().map(Holder::to_string).collect::<Vec<_>>().join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn scratch(name: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(name);
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        d
    }

    #[test]
    fn a_process_working_inside_the_tree_is_a_holder_and_one_outside_is_not() {
        let dir = scratch("holders-find");
        let tree = dir.path().to_path_buf();
        let mut inside = Command::new("sleep").arg("30").current_dir(tree.join("sub")).spawn().unwrap();
        let mut outside = Command::new("sleep").arg("30").current_dir("/").spawn().unwrap();
        let found: Vec<u32> = holders(Path::new("/proc"), &tree, &[]).iter().map(|h| h.pid).collect();
        let spared: Vec<u32> = holders(Path::new("/proc"), &tree, &[inside.id()]).iter().map(|h| h.pid).collect();
        inside.kill().unwrap();
        outside.kill().unwrap();
        let _ = inside.wait();
        let _ = outside.wait();
        assert_eq!(found, [inside.id()]);
        assert!(spared.is_empty());
    }

    #[test]
    fn stopping_the_holders_ends_a_process_that_ignores_term() {
        let dir = scratch("holders-stop");
        let tree = dir.path().to_path_buf();
        let child = Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .current_dir(&tree)
            .spawn()
            .unwrap();
        let pid = child.id();
        let reaper = std::thread::spawn(move || {
            let mut c = child;
            c.wait()
        });
        let stopped = stop_holders(Path::new("/proc"), &tree, &[], Duration::from_millis(300)).unwrap();
        let _ = reaper.join();
        assert!(stopped.iter().any(|h| h.pid == pid), "{stopped:?}");
        assert!(holders(Path::new("/proc"), &tree, &[]).is_empty());
    }
}
