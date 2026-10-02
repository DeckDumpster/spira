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

/// The meter's own executable name in an artifact set (what its links resolve to).
pub const METER_EXE: &str = "bd-meter";

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
    find_real_after(name, path, me, None)
}

/// [`find_real`], searching only the PATH entries **after** `invoked_from` — the directory
/// the meter was invoked through by path (`/…/home/.local/bin/bd`), when that directory is on
/// PATH. A wrapper ahead of it on PATH that execs the meter by path (loom's counting shim
/// execs `$SPIRA_BD`, an absolute path since sp-34ru2) would otherwise be taken for the real
/// `bd`, and the two would exec each other until the process table refused (EAGAIN).
pub fn find_real_after(
    name: &str,
    path: &OsStr,
    me: &Path,
    invoked_from: Option<&Path>,
) -> Option<PathBuf> {
    let me = fs::canonicalize(me).unwrap_or_else(|_| me.to_path_buf());
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    let start = invoked_from
        .and_then(|d| fs::canonicalize(d).ok())
        .and_then(|d| {
            dirs.iter()
                .position(|e| fs::canonicalize(e).ok().as_deref() == Some(d.as_path()))
        })
        .map_or(0, |i| i + 1);
    dirs[start.min(dirs.len())..].iter().find_map(|dir| {
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

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("bdmeter-{tag}"))
    }

    fn exe(p: &Path) {
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of): the same
        // ETXTBSY race a concurrent fork elsewhere in this binary can trigger.
        testkit::write_exe(p, "#!/bin/sh\n");
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
    fn invoked_by_path_the_meter_looks_only_past_its_own_directory() {
        // PATH = <wrapper>:<suite home bin>:<real>. The wrapper (loom's counting shim) execs
        // the meter by its absolute path; the meter must not take the wrapper for the real bd.
        let d = tmp("after");
        let meter = d.join("bd-meter");
        exe(&meter);
        let home = d.join("home");
        install(&home, &meter).unwrap();
        let bin = home.join(BIN_UNDER_HOME);
        let wrapper = d.join("wrapper");
        let real = d.join("real");
        fs::create_dir_all(&wrapper).unwrap();
        fs::create_dir_all(&real).unwrap();
        exe(&wrapper.join("bd"));
        exe(&real.join("bd"));
        let path = std::env::join_paths([&wrapper, &bin, &real]).unwrap();
        assert_eq!(
            find_real("bd", &path, &meter),
            Some(wrapper.join("bd")),
            "plant: without the invoking directory the wrapper is taken — the loop"
        );
        assert_eq!(
            find_real_after("bd", &path, &meter, Some(&bin)),
            Some(real.join("bd"))
        );
        // Invoked from a directory not on PATH: the whole PATH, the meter skipped.
        assert_eq!(
            find_real_after("bd", &path, &meter, Some(&d)),
            Some(wrapper.join("bd"))
        );
        // Nothing past the meter's directory: not found, never a wrapper behind it.
        let path = std::env::join_paths([&wrapper, &bin]).unwrap();
        assert_eq!(find_real_after("bd", &path, &meter, Some(&bin)), None);
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
