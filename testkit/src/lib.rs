//! Test scaffolding shared across the workspace. Contract: DESIGN.md.

use std::ffi::OsStr;
use std::io::Write;
use std::ops::Deref;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh scratch directory under the system temp dir, removed with everything under it
/// when dropped — including when a failing test unwinds. The only way test code in this
/// workspace gets scratch space (spira-lint `tmp-leak`, sp-qgfdi).
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// `<temp_dir>/<tag>-<pid>-<n>`, created empty, its path canonical. `<n>` is
    /// process-wide, so two calls never share a directory.
    pub fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("mkdir {}: {e}", d.display()));
        TempDir(d.canonicalize().unwrap_or(d))
    }

    /// The directory. It lives exactly as long as this value.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for TempDir {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `body` to `path` as an executable (mode 0755) that a test will then exec,
/// WITHOUT THIS PROCESS EVER HOLDING A WRITE DESCRIPTOR ON IT.
///
/// `fs::write` followed by an exec fails with ETXTBSY whenever another test thread forks
/// while the write descriptor is open. The child carries a copy of it until it execs, and
/// the kernel refuses to exec a file anyone has open for writing. A child process writes
/// the file here instead, so no fork in this process can inherit a descriptor on it.
pub fn write_exe(path: impl AsRef<Path>, body: &str) {
    let path = path.as_ref();
    // /bin/sh by absolute path, and /bin/cat INSIDE the script also by absolute path: another
    // test in the same binary may have set the process PATH to a directory with neither in it
    // (release/src/tests.rs does, under its own ENV_LOCK) — sp-e7fe2 fixed the outer `sh` but
    // left the inner `cat` a bare word still resolved through that same mutated PATH, so the
    // race it was meant to remove kept firing, just one level deeper ("sh: 1: cat: not found",
    // sp-tuupa).
    let mut c = Command::new("/bin/sh")
        .arg("-c")
        .arg("/bin/cat > \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh to write an executable");
    c.stdin
        .take()
        .expect("its stdin")
        .write_all(body.as_bytes())
        .expect("the executable's body");
    assert!(c.wait().expect("the writer").success(), "could not write {}", path.display());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 755 {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> TempDir {
        TempDir::new(&format!("testkit-{tag}"))
    }


    #[test]
    fn a_temp_dir_is_fresh_distinct_and_gone_after_drop() {
        let a = TempDir::new("testkit-td");
        let b = TempDir::new("testkit-td");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && std::fs::read_dir(&a).unwrap().next().is_none());
        std::fs::create_dir_all(a.join("x/y")).unwrap();
        std::fs::write(a.join("x/y/f"), "z").unwrap();
        let p = a.to_path_buf();
        drop(a);
        assert!(!p.exists(), "{} survived its drop", p.display());
        assert!(b.is_dir());
    }

    #[test]
    fn a_panicking_test_still_removes_its_dir() {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = std::thread::spawn(move || {
            let d = TempDir::new("testkit-panic");
            tx.send(d.to_path_buf()).unwrap();
            panic!("the test fails");
        })
        .join();
        assert!(r.is_err());
        let p = rx.recv().unwrap();
        assert!(!p.exists(), "{} survived an unwinding test", p.display());
    }

    #[test]
    fn the_file_holds_exactly_the_body_and_is_executable() {
        let d = dir("body");
        let p = d.join("x");
        let body = "#!/bin/sh\nprintf '%s' \"a b\"\n# no trailing newline after this";
        write_exe(&p, body);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        let out = Command::new(&p).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a b");
    }

    #[test]
    fn a_rewrite_replaces_the_old_body() {
        let d = dir("rewrite");
        let p = d.join("x");
        write_exe(&p, "#!/bin/sh\necho one\n");
        write_exe(&p, "#!/bin/sh\necho two\n");
        assert_eq!(String::from_utf8_lossy(&Command::new(&p).output().unwrap().stdout), "two\n");
    }

    /// THE RACE ITSELF. Eight threads each write a program and exec it at once, a hundred
    /// times over, while every one of them is forking. With `fs::write` in place of
    /// write_exe this fails within a few rounds; here every exec must succeed.
    #[test]
    fn concurrent_writers_and_forkers_never_see_text_file_busy() {
        let d = dir("race");
        let dp = d.to_path_buf();
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let d = dp.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let p = d.join(format!("p{t}-{i}"));
                        write_exe(&p, "#!/bin/sh\nexit 0\n");
                        let st = Command::new(&p).status().unwrap_or_else(|e| panic!("exec {}: {e}", p.display()));
                        assert!(st.success());
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
    }
}
