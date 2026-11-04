//! A filesystem's size, free space and use, from `statvfs(3)` — what `df --output=avail,pcent,size
//! -BM` printed, without the `df` process.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Space available to an unprivileged writer, MiB rounded up (df -BM).
    pub avail_mib: i64,
    pub size_mib: i64,
    /// df's Use%: used ÷ (used + available), rounded up.
    pub used_pct: i64,
}

const MIB: u128 = 1 << 20;

/// df's arithmetic on raw block counts of `frsize` bytes.
pub fn from_counts(blocks: u64, bfree: u64, bavail: u64, frsize: u64) -> Usage {
    let mib = |n: u64| (n as u128 * frsize as u128).div_ceil(MIB) as i64;
    let used = blocks.saturating_sub(bfree) as u128;
    let denom = used + bavail as u128;
    let used_pct = if denom == 0 { 0 } else { (used * 100).div_ceil(denom) as i64 };
    Usage { avail_mib: mib(bavail), size_mib: mib(blocks), used_pct }
}

/// `None` when the path cannot be read.
pub fn usage(path: &str) -> Option<Usage> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(from_counts(st.f_blocks as u64, st.f_bfree as u64, st.f_bavail as u64, st.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_is_rounded_up_against_used_plus_available_as_df_does() {
        // 1000 blocks of 1 MiB, 400 free of which 300 available to users: used 600 of 900.
        let u = from_counts(1000, 400, 300, 1 << 20);
        assert_eq!(u, Usage { avail_mib: 300, size_mib: 1000, used_pct: 67 });
        assert_eq!(from_counts(0, 0, 0, 4096).used_pct, 0);
        assert_eq!(from_counts(3, 3, 3, 4096).avail_mib, 1, "a partial MiB rounds up");
    }

    #[test]
    fn the_root_filesystem_reads_and_a_missing_path_does_not() {
        let u = usage("/").expect("statvfs /");
        assert!(u.size_mib > 0 && (0..=100).contains(&u.used_pct));
        assert!(usage("/does/not/exist").is_none());
    }
}
