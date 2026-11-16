//! The cut hold: `<queue_dir>/<repo>/cut-hold`, a record that no new round is to be cut for the
//! repo. `queue hand-land` places it so the gap between an open round's end and its own landing
//! is not filled by the batcher's next cut. A hold whose holder died, or whose `until` passed,
//! holds nothing: a crashed verb never stops cutting for good.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// What round-vm prints when the tree a pass built is not the tree the host sent; the queue and
/// the batcher match on it to tell a moved base from a fault.
pub const TREE_MISMATCH: &str = "refusing binaries: tree sha";

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct CutHold {
    pub owner: String,
    pub reason: String,
    pub pid: u32,
    pub since: u64,
    pub until: u64,
}

pub fn path(queue_dir: &Path, repo: &str) -> PathBuf {
    queue_dir.join(repo).join("cut-hold")
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn read(p: &Path) -> Option<CutHold> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

/// The hold in force at `now`; one whose process is gone or whose time is up is not in force.
pub fn held(queue_dir: &Path, repo: &str, now: u64) -> Option<CutHold> {
    held_if(queue_dir, repo, now, alive)
}

fn held_if(queue_dir: &Path, repo: &str, now: u64, alive: impl Fn(u32) -> bool) -> Option<CutHold> {
    read(&path(queue_dir, repo)).filter(|h| now < h.until && alive(h.pid))
}

/// Places the hold, or names the one already in force. Creation is exclusive, so two holders
/// cannot both believe they hold it.
pub fn place(queue_dir: &Path, repo: &str, hold: &CutHold) -> Result<(), String> {
    let p = path(queue_dir, repo);
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let body = serde_json::to_string(hold).map_err(|e| e.to_string())?;
    for _ in 0..2 {
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                use std::io::Write;
                return f.write_all(body.as_bytes()).map_err(|e| format!("{}: {e}", p.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(h) = held(queue_dir, repo, hold.since) {
                    return Err(format!("cut already held by {} ({}) until {}", h.owner, h.reason, h.until));
                }
                let _ = fs::remove_file(&p);
            }
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    Err(format!("{}: could not take the hold", p.display()))
}

/// Lifts the hold if `pid` placed it.
pub fn lift(queue_dir: &Path, repo: &str, pid: u32) {
    let p = path(queue_dir, repo);
    if read(&p).is_some_and(|h| h.pid == pid) {
        let _ = fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold(pid: u32, until: u64) -> CutHold {
        CutHold { owner: "hand-land sp-x".into(), reason: "landing".into(), pid, since: 100, until }
    }

    fn dir() -> testkit::TempDir {
        testkit::TempDir::new("cut-hold")
    }

    #[test]
    fn a_placed_hold_is_in_force_until_it_expires_or_its_holder_dies() {
        let t = dir();
        let d = t.path().to_path_buf();
        place(&d, "r", &hold(7, 500)).unwrap();
        assert!(held_if(&d, "r", 200, |_| true).is_some());
        assert!(held_if(&d, "r", 500, |_| true).is_none(), "time is up");
        assert!(held_if(&d, "r", 200, |_| false).is_none(), "holder is gone");
        assert!(held_if(&d, "other", 200, |_| true).is_none(), "another repo is not held");
    }

    #[test]
    fn a_second_hold_is_refused_naming_the_first_and_a_dead_one_is_replaced() {
        let t = dir();
        let d = t.path().to_path_buf();
        let me = std::process::id();
        place(&d, "r", &hold(me, 500)).unwrap();
        let e = place(&d, "r", &hold(me, 500)).unwrap_err();
        assert!(e.contains("hand-land sp-x"), "{e}");
        lift(&d, "r", me + 1);
        assert!(path(&d, "r").exists(), "only the placer lifts it");
        lift(&d, "r", me);
        assert!(!path(&d, "r").exists());
        fs::write(path(&d, "r"), serde_json::to_string(&hold(u32::MAX - 1, 500)).unwrap()).unwrap();
        place(&d, "r", &hold(me, 500)).unwrap();
    }
}
