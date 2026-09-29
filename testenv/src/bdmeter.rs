//! The bd meter (DESIGN.md §3.4 "bd wait", sp-cln99): how long each suite waits on the bead
//! store. `suite-timing` has carried `bd_calls`/`bd_ms` since testenv replaced
//! testenv-batch.sh, but nothing wrote the `SPIRA_BD_LOG` they are read from (D3 dropped the
//! bash shim, which was never on a PATH either), so all 12,031 rows before this read 0/0.
//!
//! `bd-meter` is installed per parallel suite as `$HOME/.local/bin/bd` and
//! `$HOME/.local/bin/bd-embedded` (symlinks to the artifact under test; conf.sh puts
//! `$HOME/.local/bin` ahead of the system dirs). Invoked under either name it runs the next
//! binary of that name on PATH, waits, appends `<wall_ms> <rc> <subcommand>` to
//! `$SPIRA_BD_LOG` (the line `timing::bd_totals` reads), and exits with the real status. It
//! changes nothing a suite can observe except `command -v bd`.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// The names the meter answers to: the standard binary and the embedded one testdb.sh uses.
pub const SHIMMED: [&str; 2] = ["bd", "bd-embedded"];

/// The directory under a suite's HOME that conf.sh puts on PATH ahead of the system dirs.
pub const BIN_UNDER_HOME: &str = ".local/bin";

/// One call's log line — the shape `timing::bd_totals` sums (first field, milliseconds).
pub fn log_line(wall_ms: u128, rc: i32, args: &[String]) -> String {
    let mut sub = "-";
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-C" {
            it.next();
        } else if !a.starts_with('-') {
            sub = a;
            break;
        }
    }
    format!("{wall_ms} {rc} {sub}\n")
}

/// The first `<dir>/<name>` on `path` that is an executable file and is not this meter
/// (compared canonically, so the symlink that invoked us is skipped however it was reached).
pub fn find_real(name: &str, path: &OsStr, me: &Path) -> Option<PathBuf> {
    let me = fs::canonicalize(me).unwrap_or_else(|_| me.to_path_buf());
    std::env::split_paths(path).find_map(|dir| {
        let cand = dir.join(name);
        let meta = fs::metadata(&cand).ok()?;
        use std::os::unix::fs::PermissionsExt;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
        (fs::canonicalize(&cand).ok()? != me).then_some(cand)
    })
}

/// `bd-meter --install <home>`: the per-suite HOME's directories (the systemd user dir the
/// suite runner always made, and the PATH dir) and one symlink per [`SHIMMED`] name to `exe`.
/// Idempotent: an existing link is replaced.
pub fn install(home: &Path, exe: &Path) -> std::io::Result<()> {
    fs::create_dir_all(home.join(".config/systemd/user"))?;
    let bin = home.join(BIN_UNDER_HOME);
    fs::create_dir_all(&bin)?;
    for n in SHIMMED {
        let link = bin.join(n);
        let _ = fs::remove_file(&link);
        symlink(exe, &link)?;
    }
    Ok(())
}

/// Append one line to the log; best-effort (a meter never fails the call it measures).
pub fn append(log: &Path, line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// An exit status as a shell reports it: the code, or 128 + the signal.
pub fn shell_status(st: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bdmeter-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn exe(p: &Path) {
        fs::write(p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn the_log_line_is_what_bd_totals_sums() {
        let args: Vec<String> = ["-C", "/db", "--json", "list"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            log_line(12, 0, &args),
            "12 0 list\n",
            "-C's directory is not the subcommand"
        );
        let args: Vec<String> = ["show", "sp-1"].iter().map(|s| s.to_string()).collect();
        let l = log_line(30, 1, &args);
        assert_eq!(l, "30 1 show\n");
        assert_eq!(crate::timing::bd_totals(&format!("{l}{l}")), (2, 60));
        assert_eq!(log_line(5, 0, &[]), "5 0 -\n");
    }

    #[test]
    fn install_links_both_names_and_the_real_one_is_found_past_them() {
        let d = tmp("install");
        let meter = d.join("bd-meter");
        exe(&meter);
        let home = d.join("home");
        install(&home, &meter).unwrap();
        install(&home, &meter).unwrap(); // idempotent
        assert!(home.join(".config/systemd/user").is_dir());
        let bin = home.join(BIN_UNDER_HOME);
        for n in SHIMMED {
            assert_eq!(fs::read_link(bin.join(n)).unwrap(), meter);
        }
        let real = d.join("usr-bin");
        fs::create_dir_all(&real).unwrap();
        exe(&real.join("bd"));
        let path = std::env::join_paths([&bin, &real]).unwrap();
        assert_eq!(
            find_real("bd", &path, &bin.join("bd")),
            Some(real.join("bd")),
            "the meter's own link is skipped"
        );
        assert_eq!(
            find_real("bd", &path, &meter),
            Some(real.join("bd")),
            "…however the meter was reached"
        );
        assert_eq!(find_real("bd-embedded", &path, &meter), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_non_executable_is_not_the_real_bd() {
        let d = tmp("noexec");
        fs::write(d.join("bd"), "").unwrap();
        let path = std::env::join_paths([&d]).unwrap();
        assert_eq!(
            find_real("bd", &path, Path::new("/nonexistent/meter")),
            None
        );
        let _ = fs::remove_dir_all(&d);
    }
}
