//! Space actually available to this user on a path: the smaller of the filesystem's free
//! space and the user's quota headroom. A per-user quota binds before statvfs does.

use std::os::unix::io::AsRawFd;
use std::path::Path;

const MIB: u64 = 1024 * 1024;

/// A user's block quota on one filesystem, in bytes. `limit` is the hard limit (0 = none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub limit: u64,
    pub used: u64,
}

pub fn statvfs_mib(p: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs into a zeroed struct we own, on a NUL-terminated path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64) / MIB)
}

/// The calling user's block quota on `p`'s filesystem, via quotactl_fd (Linux 5.14+, the only
/// call that reaches a tmpfs quota). None: no quota, unsupported, or unreadable.
pub fn user_quota(p: &Path) -> Option<Quota> {
    #[repr(C)]
    #[derive(Default)]
    struct Dqblk {
        bhardlimit: u64,
        bsoftlimit: u64,
        curspace: u64,
        ihardlimit: u64,
        isoftlimit: u64,
        curinodes: u64,
        btime: u64,
        itime: u64,
        valid: u32,
    }
    const SYS_QUOTACTL_FD: libc::c_long = 443;
    const Q_GETQUOTA: u32 = 0x80_0007;
    const USRQUOTA: u32 = 0;
    let f = std::fs::File::open(p).ok()?;
    let mut d = Dqblk::default();
    // SAFETY: quotactl_fd reads a descriptor we own and fills the dqblk we pass.
    let rc = unsafe {
        libc::syscall(
            SYS_QUOTACTL_FD,
            f.as_raw_fd(),
            (Q_GETQUOTA << 8) | USRQUOTA,
            libc::getuid(),
            &mut d as *mut Dqblk,
        )
    };
    if rc != 0 || d.bhardlimit == 0 {
        return None;
    }
    Some(Quota { limit: d.bhardlimit.saturating_mul(1024), used: d.curspace })
}

pub fn combine_mib(fs_free_mib: Option<u64>, quota: Option<Quota>) -> Option<u64> {
    let headroom = quota.map(|q| q.limit.saturating_sub(q.used) / MIB);
    match (fs_free_mib, headroom) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// MiB this user can still write under `p`.
pub fn avail_mib(p: &Path) -> Option<u64> {
    combine_mib(statvfs_mib(p), user_quota(p))
}

/// True when build output carries a write failure from a full disk or an exhausted quota.
pub fn is_space_failure(output: &str) -> bool {
    ["Disk quota exceeded", "No space left on device", "EDQUOT", "ENOSPC"]
        .iter()
        .any(|m| output.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_headroom_below_fs_free_wins() {
        let q = Quota { limit: 20 * 1024 * MIB, used: 18 * 1024 * MIB };
        assert_eq!(combine_mib(Some(6200), Some(q)), Some(2048));
    }

    #[test]
    fn fs_free_wins_when_smaller_and_no_quota_means_fs_free() {
        let q = Quota { limit: 20 * 1024 * MIB, used: MIB };
        assert_eq!(combine_mib(Some(100), Some(q)), Some(100));
        assert_eq!(combine_mib(Some(6200), None), Some(6200));
        assert_eq!(combine_mib(None, None), None);
    }

    #[test]
    fn over_quota_is_zero_headroom() {
        let q = Quota { limit: MIB, used: 5 * MIB };
        assert_eq!(combine_mib(Some(9000), Some(q)), Some(0));
    }

    #[test]
    fn space_failures_are_recognised_and_ordinary_output_is_not() {
        assert!(is_space_failure("error: write: Disk quota exceeded (os error 122)"));
        assert!(is_space_failure("No space left on device"));
        assert!(!is_space_failure("Compiling x v0.1.0\nFinished release"));
    }

    #[test]
    fn real_probe_answers_on_a_real_path() {
        assert!(avail_mib(Path::new("/tmp")).is_some());
    }
}
