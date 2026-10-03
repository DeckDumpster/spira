//! Shared `/proc` reads for `layout` and `rebuild`. No subprocess spawned for any of
//! these — `pgrep`/`ps` get slow and drop requests under load, and a caller of the bash
//! equivalent (`proc_start`, reached through `ps -o etimes=`) once respawned its subject
//! 2212 times in 70 minutes on failed probes alone when `ps` stopped answering in time. A
//! direct `/proc` read cannot be starved the same way a forked `ps` can.

use std::fs;

/// argv of `pid`, or `None` if it cannot be read (process gone, or no permission).
pub fn cmdline(pid: i32) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}

/// The parent pid of `pid`, from field 4 of `/proc/<pid>/stat`. `None` if unreadable.
/// Field 2 (`comm`) is parenthesised and may itself contain spaces or parens, so this scans
/// for the LAST `)` before splitting on whitespace, exactly as the kernel's own docs say a
/// robust reader must.
pub fn ppid(pid: i32) -> Option<i32> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = s.rfind(')')?;
    let rest = s.get(close + 1..)?;
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok() // ppid
}

/// Every pid currently in `/proc`.
pub fn all_pids() -> Vec<i32> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir("/proc") {
        for ent in rd.flatten() {
            if let Some(name) = ent.file_name().to_str() {
                if let Ok(pid) = name.parse::<i32>() {
                    out.push(pid);
                }
            }
        }
    }
    out
}

/// The immediate children of `pid` (what `pgrep -P <pid>` prints), from a fresh `/proc` scan.
pub fn children_of(pid: i32) -> Vec<i32> {
    all_pids().into_iter().filter(|&p| ppid(p) == Some(pid)).collect()
}

/// How many currently-running processes have `root` anywhere in their ancestor chain. The
/// IO (a full `/proc` walk for every pid's ppid) lives here; the walk itself is
/// `crate::tmux::descendants_of`, shared with that module's own (synthetic-table) tests.
pub fn descendant_count(root: i32) -> usize {
    let pairs: Vec<(u32, u32)> = all_pids()
        .into_iter()
        .filter_map(|p| ppid(p).map(|pp| (p as u32, pp as u32)))
        .collect();
    crate::tmux::descendants_of(root as u32, &pairs)
}

/// The inode of the LISTENING unix socket bound to `path`. The IO (reading `/proc/net/unix`)
/// lives here; the parse itself is `crate::tmux::find_listening_inode`, shared so the parse
/// logic is tested once and used identically by every caller that needs it with different
/// input (a real read here, a synthetic table in that module's own tests).
fn listening_socket_inode(path: &str) -> Option<String> {
    let net_unix = fs::read_to_string("/proc/net/unix").ok()?;
    crate::tmux::find_listening_inode(&net_unix, path)
}

/// The pid holding the LISTENING unix socket at `path` — by inode, never by pattern.
/// `pgrep -f tmux` matches this program's own argv and every attached client; a `pkill -f`
/// on that basis once killed the shell that invoked it
/// (law: never pgrep/pkill -f on the pattern alone). Exactly one process can hold a
/// listening socket's inode open, found by scanning every pid's open fds for it.
pub fn listening_socket_holder(path: &str) -> Option<i32> {
    let inode = listening_socket_inode(path)?;
    let needle = format!("socket:[{inode}]");
    for pid in all_pids() {
        let fd_dir = format!("/proc/{pid}/fd");
        let Ok(rd) = fs::read_dir(&fd_dir) else { continue };
        for ent in rd.flatten() {
            if let Ok(target) = fs::read_link(ent.path()) {
                if target.to_string_lossy() == needle {
                    return Some(pid);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_of_pid_1_is_readable_or_permission_denied_not_a_panic() {
        // pid 1 always exists on a real kernel; this just proves the function does not
        // panic either way (permission may or may not be granted in a test sandbox).
        let _ = cmdline(1);
    }

    #[test]
    fn ppid_of_pid_1_is_zero_or_none() {
        // pid 1's parent is PID 0 (not a real process) or unreadable — either way, never a
        // crash, and never confused for a real ancestor by descendant_count's root check.
        let p = ppid(1);
        assert!(p.is_none() || p == Some(0));
    }

    #[test]
    fn descendant_count_of_current_pid_includes_no_self() {
        // Counted from a private subtree: this test process shares its pid with concurrent
        // tests that spawn children, so it cannot be the root.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 3 & wait"])
            .spawn()
            .unwrap();
        let root = child.id() as i32;
        let mut n = 0;
        for _ in 0..200 {
            n = descendant_count(root);
            if n >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(n, 1, "root itself must not be counted, only the sleep");
    }
}
