//! The capabilities the service may hold: CAP_BPF and CAP_PERFMON while it loads, none after.

pub const CAP_PERFMON: u32 = 38;
pub const CAP_BPF: u32 = 39;

const NAMES: [&str; 41] = [
    "CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_DAC_READ_SEARCH", "CAP_FOWNER", "CAP_FSETID", "CAP_KILL", "CAP_SETGID", "CAP_SETUID", "CAP_SETPCAP",
    "CAP_LINUX_IMMUTABLE", "CAP_NET_BIND_SERVICE", "CAP_NET_BROADCAST", "CAP_NET_ADMIN", "CAP_NET_RAW", "CAP_IPC_LOCK", "CAP_IPC_OWNER",
    "CAP_SYS_MODULE", "CAP_SYS_RAWIO", "CAP_SYS_CHROOT", "CAP_SYS_PTRACE", "CAP_SYS_PACCT", "CAP_SYS_ADMIN", "CAP_SYS_BOOT", "CAP_SYS_NICE",
    "CAP_SYS_RESOURCE", "CAP_SYS_TIME", "CAP_SYS_TTY_CONFIG", "CAP_MKNOD", "CAP_LEASE", "CAP_AUDIT_WRITE", "CAP_AUDIT_CONTROL", "CAP_SETFCAP",
    "CAP_MAC_OVERRIDE", "CAP_MAC_ADMIN", "CAP_SYSLOG", "CAP_WAKE_ALARM", "CAP_BLOCK_SUSPEND", "CAP_AUDIT_READ", "CAP_PERFMON", "CAP_BPF",
    "CAP_CHECKPOINT_RESTORE",
];

fn name(bit: u32) -> String {
    NAMES.get(bit as usize).map_or_else(|| format!("CAP_{bit}"), |n| (*n).to_string())
}

/// `CapEff` of a `/proc/<pid>/status` text.
pub fn effective(status: &str) -> Result<u64, String> {
    let hex = status.lines().find_map(|l| l.strip_prefix("CapEff:")).ok_or("no CapEff line in /proc/self/status")?.trim();
    u64::from_str_radix(hex, 16).map_err(|e| format!("CapEff {hex:?} is not hex: {e}"))
}

/// The refusal to start: a missing CAP_BPF or CAP_PERFMON, or any capability beyond those two.
pub fn check(status: &str, prod_root: &str) -> Result<(), String> {
    let eff = effective(status)?;
    let held = |bit: u32| eff >> bit & 1 == 1;
    let missing: Vec<String> = [CAP_BPF, CAP_PERFMON].into_iter().filter(|&b| !held(b)).map(name).collect();
    if !missing.is_empty() {
        return Err(format!("proc-starts: missing {}; grant it once: sudo {prod_root}/bin/spira-install --proc-starts", missing.join(" and ")));
    }
    let extra: Vec<String> = (0..64).filter(|&b| held(b) && b != CAP_BPF && b != CAP_PERFMON).map(name).collect();
    if !extra.is_empty() {
        return Err(format!("proc-starts: holds more than CAP_BPF and CAP_PERFMON: {}; refusing to run with them", extra.join(", ")));
    }
    Ok(())
}

/// After [`drop_all`]: `CapEff` must be 0.
pub fn check_dropped(status: &str) -> Result<(), String> {
    match effective(status)? {
        0 => Ok(()),
        eff => Err(format!("proc-starts: still holds CapEff {eff:016x} after dropping every capability; refusing to run")),
    }
}

#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Clears the ambient set, then permitted, effective and inheritable.
pub fn drop_all() -> Result<(), String> {
    // SAFETY: prctl with PR_CAP_AMBIENT_CLEAR_ALL reads no pointer.
    if unsafe { libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) } != 0 {
        return Err(format!("cannot clear ambient capabilities: {}", std::io::Error::last_os_error()));
    }
    let mut header = Header { version: CAPABILITY_VERSION_3, pid: 0 };
    let data = [Data { effective: 0, permitted: 0, inheritable: 0 }; 2];
    // SAFETY: header and data are the layouts capset(2) reads for version 3: one header, two data words.
    if unsafe { libc::syscall(libc::SYS_capset, &mut header as *mut Header, data.as_ptr()) } != 0 {
        return Err(format!("cannot drop capabilities: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(eff: u64) -> String {
        format!("Name:\tproc-starts\nCapInh:\t0000000000000000\nCapPrm:\t{eff:016x}\nCapEff:\t{eff:016x}\nCapBnd:\t000000c000000000\n")
    }

    const ROOT: &str = "/rel/current"; // not a path: the release root the message names

    #[test_id(tlkm3cg)]
    #[test]
    fn exactly_perfmon_and_bpf_passes() {
        assert_eq!(check(&status(1 << 38 | 1 << 39), ROOT), Ok(()));
    }

    #[test_id(ts31rn9)]
    #[test]
    fn a_missing_cap_bpf_is_named_with_the_command_that_grants_it() {
        let e = check(&status(1 << 38), ROOT).unwrap_err();
        assert!(e.contains("CAP_BPF") && !e.contains("CAP_PERFMON"), "{e}");
        assert!(e.contains("grant it once: sudo /rel/current/bin/spira-install --proc-starts"), "{e}");
    }

    #[test_id(t9w1ohb)]
    #[test]
    fn both_missing_names_both_and_still_one_grant_line() {
        let e = check(&status(0), ROOT).unwrap_err();
        assert!(e.contains("CAP_BPF and CAP_PERFMON"), "{e}");
        assert_eq!(e.matches("spira-install --proc-starts").count(), 1, "{e}");
    }

    #[test]
    fn a_third_capability_is_refused_by_name() {
        let e = check(&status(1 << 38 | 1 << 39 | 1 << 21), ROOT).unwrap_err();
        assert!(e.ends_with("holds more than CAP_BPF and CAP_PERFMON: CAP_SYS_ADMIN; refusing to run with them"), "{e}");
    }

    #[test_id(ti1x2on)]
    #[test]
    fn nothing_held_after_the_drop_passes_and_anything_held_does_not() {
        assert_eq!(check_dropped(&status(0)), Ok(()));
        assert!(check_dropped(&status(1 << 39)).is_err());
    }
}
