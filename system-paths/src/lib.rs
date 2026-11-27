//! Every real absolute system path the code names. Contract: DESIGN.md.

/// The root directory.
pub const ROOT: &str = "/";
/// The proc filesystem root.
pub const PROC: &str = "/proc";
/// The calling process's proc directory.
pub const PROC_SELF: &str = "/proc/self";
/// Kernel tunables under proc.
pub const PROC_SYS: &str = "/proc/sys";
/// The PSI directory.
pub const PROC_PRESSURE: &str = "/proc/pressure";
/// The sysfs root.
pub const SYS: &str = "/sys";
/// The cgroup v2 mount.
pub const SYS_FS_CGROUP: &str = "/sys/fs/cgroup";
/// The device directory.
pub const DEV: &str = "/dev";
/// The null device.
pub const DEV_NULL: &str = "/dev/null";
/// The kernel random source.
pub const DEV_URANDOM: &str = "/dev/urandom";
/// The process's standard input as a path.
pub const DEV_STDIN: &str = "/dev/stdin";
/// The system temp directory.
pub const TMP: &str = "/tmp";
/// The persistent temp directory.
pub const VAR_TMP: &str = "/var/tmp";
/// The system cache directory.
pub const VAR_CACHE: &str = "/var/cache";
/// The system configuration directory.
pub const ETC: &str = "/etc";
/// The user database.
pub const ETC_PASSWD: &str = "/etc/passwd";
/// The operator's system unit directory.
pub const ETC_SYSTEMD_SYSTEM: &str = "/etc/systemd/system";
/// The distribution's system unit directory.
pub const LIB_SYSTEMD_SYSTEM: &str = "/lib/systemd/system";
/// The distribution's system unit directory under /usr.
pub const USR_LIB_SYSTEMD_SYSTEM: &str = "/usr/lib/systemd/system";
/// The runtime-state directory.
pub const RUN: &str = "/run";
/// The per-user runtime parent directory.
pub const RUN_USER: &str = "/run/user";
/// The marker a podman container carries.
pub const RUN_CONTAINERENV: &str = "/run/.containerenv";
/// The marker a docker container carries.
pub const DOCKERENV: &str = "/.dockerenv";
/// The home-directory parent.
pub const HOME: &str = "/home";
/// Root's home directory.
pub const ROOT_HOME: &str = "/root";
/// The system binary directory.
pub const BIN: &str = "/bin";
/// The distribution binary directory.
pub const USR_BIN: &str = "/usr/bin";
/// The locally installed binary directory.
pub const USR_LOCAL_BIN: &str = "/usr/local/bin";
/// The POSIX shell.
pub const BIN_SH: &str = "/bin/sh";
/// Bash under /bin.
pub const BIN_BASH: &str = "/bin/bash";
/// Bash under /usr/bin.
pub const USR_BIN_BASH: &str = "/usr/bin/bash";
/// The true binary.
pub const BIN_TRUE: &str = "/bin/true";
/// The false binary.
pub const BIN_FALSE: &str = "/bin/false";
/// The echo binary.
pub const BIN_ECHO: &str = "/bin/echo";
/// The env binary.
pub const USR_BIN_ENV: &str = "/usr/bin/env";
/// The true binary under /usr/bin.
pub const USR_BIN_TRUE: &str = "/usr/bin/true";
/// The tmux binary.
pub const USR_BIN_TMUX: &str = "/usr/bin/tmux";
/// The shell of an account that cannot log in.
pub const USR_SBIN_NOLOGIN: &str = "/usr/sbin/nologin";
/// A path that never exists.
pub const NONEXISTENT: &str = "/nonexistent";
/// The PATH of a spawned child on the host.
pub const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// The two-entry PATH of a minimal child.
pub const SYSTEM_PATH_MIN: &str = "/usr/bin:/bin";
/// A login shell's PATH.
pub const LOGIN_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// The I/O pressure-stall file.
pub const PSI_IO: &str = "/proc/pressure/io";
/// The CPU pressure-stall file.
pub const PSI_CPU: &str = "/proc/pressure/cpu";
/// The system lifecycle controller's socket.
pub const LC_SYSTEM_SOCKET: &str = "/run/spira-lc/sock";
/// The dovecot unit drop-in the mail host carries.
pub const DOVECOT_DROPIN: &str = "/etc/systemd/system/dovecot.service.d/spira-tailnet.conf";
/// A unit condition path that never exists, so the store stays frozen.
pub const FROZEN_CONDITION: &str = "/nonexistent/spira-store-frozen-thaw-with-world-thaw-store";

/// [`guest::VAR_CACHE_SPIRA_FOREIGN_TARGET`] as a literal, for `concat!` into a script.
#[macro_export]
macro_rules! foreign_cache_target {
    () => {
        "/var/cache/spira-foreign/target"
    };
}

/// [`guest::VAR_CACHE_SPIRA_FOREIGN_CARGO`] as a literal, for `concat!` into a script.
#[macro_export]
macro_rules! foreign_cache_cargo {
    () => {
        "/var/cache/spira-foreign/cargo"
    };
}

/// Paths absolute inside a container or VM this code builds.
pub mod guest {
    /// The checkout mount in a container.
    pub const WORKSPACE: &str = "/workspace";
    /// The work mount in a guest.
    pub const WORK: &str = "/work";
    /// The source mount in a guest.
    pub const SRC: &str = "/src";
    /// The screenshot mount.
    pub const SHOTS: &str = "/shots";
    /// The screenshot-script mount.
    pub const SPIRA_SHOTS: &str = "/spira-shots";
    /// The cargo home in a container.
    pub const VAR_SPIRA_CARGO: &str = "/var/spira/cargo";
    /// The cargo target dir in a container.
    pub const VAR_SPIRA_CARGO_TARGET: &str = "/var/spira/cargo/target";
    /// The throwaway-instance root in a container.
    pub const TMP_SPIRA_TESTDB: &str = "/tmp/spira-testdb";
    /// The testdb template cache in a container.
    pub const VAR_CACHE_SPIRA_TESTDB_TEMPLATES: &str = "/var/cache/spira-testdb-templates";
    /// The foreign-build target cache.
    pub const VAR_CACHE_SPIRA_FOREIGN_TARGET: &str = crate::foreign_cache_target!();
    /// The foreign-build cargo cache.
    pub const VAR_CACHE_SPIRA_FOREIGN_CARGO: &str = crate::foreign_cache_cargo!();
    /// The sandbox's tree mount.
    pub const TMP_SANDBOX_TREE: &str = "/tmp/spira-sandbox-tree";
    /// The sandbox's target mount.
    pub const TMP_SANDBOX_TARGET: &str = "/tmp/spira-sandbox-target";
    /// The default testdb root on a host that keeps /tmp small.
    pub const VAR_TMP_SPIRA_TESTDB: &str = "/var/tmp/spira-testdb";
    /// The runtime dir of the container's user.
    pub const RUN_USER_1001: &str = "/run/user/1001";
    /// The tool PATH baked into the image.
    pub const IMAGE_TOOL_PATH: &str = "/usr/local/cargo/bin:/usr/local/sbin:/usr/sbin:/sbin";
    /// The template build directory.
    pub const ROOT_TEMPLATE_WORK: &str = "/root/template-work";
    /// The root block device of a round VM.
    pub const ROOT_DEVICE: &str = "/dev/sda1";
    /// The testenv runner copied into a container.
    pub const TESTENV_RUNNER: &str = "/workspace/target/.testenv-runner/testenv";
    /// The hypervisor's configuration mount.
    pub const ETC_PVE: &str = "/etc/pve";
    /// The firewall's configuration directory.
    pub const ETC_UFW: &str = "/etc/ufw";
}
