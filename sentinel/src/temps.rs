//! Temp files that must not outlive the process however it exits (G8): the snapshots and
//! the ready cache live under $SPIRA_RUN, where a leaked 25 MB file per pass would pile up.
//! Removed by `cleanup()` on every normal return and by the SIGTERM/SIGINT/SIGHUP handler
//! (systemd's TimeoutStartSec kill).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static TEMPS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `mktemp "$dir/<stem>.XXXXXX"`: created exclusively, registered for removal.
pub fn create(dir: &Path, stem: &str) -> Option<PathBuf> {
    let pid = std::process::id();
    for n in 0..1000u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let suffix = format!(
            "{:06x}",
            (nanos ^ pid.rotate_left(11) ^ n.wrapping_mul(2_654_435_761)) & 0xff_ffff
        );
        let p = dir.join(format!("{stem}.{suffix}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(_) => {
                if let Ok(mut t) = TEMPS.lock() {
                    t.push(p.clone());
                }
                return Some(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

pub fn remove(p: &Path) {
    let _ = std::fs::remove_file(p);
    if let Ok(mut t) = TEMPS.lock() {
        t.retain(|x| x != p);
    }
}

/// Removes only the temps registered under `dir`, so an in-process pass cannot delete the
/// live files of another pass sharing the registry.
pub fn cleanup_under(dir: &Path) {
    if let Ok(mut t) = TEMPS.lock() {
        t.retain(|p| {
            let ours = p.parent() == Some(dir);
            if ours {
                let _ = std::fs::remove_file(p);
            }
            !ours
        });
    }
}

pub fn cleanup() {
    if let Ok(mut t) = TEMPS.lock() {
        for p in t.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    // try_lock: never deadlock inside a handler; a pass interrupted mid-registration loses
    // at most the file being registered.
    if let Ok(t) = TEMPS.try_lock() {
        for p in t.iter() {
            if let Ok(c) = std::ffi::CString::new(p.as_os_str().to_string_lossy().as_bytes()) {
                unsafe {
                    libc::unlink(c.as_ptr());
                }
            }
        }
    }
    unsafe {
        libc::_exit(128 + sig);
    }
}

pub fn install_handlers() {
    unsafe {
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(s, on_signal as usize as libc::sighandler_t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_files_are_unique_and_cleaned() {
        let d = testkit::TempDir::new("sentinel-temps");
        let a = create(&d, "list-snapshot").unwrap();
        let b = create(&d, "list-snapshot").unwrap();
        assert_ne!(a, b);
        assert!(a
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("list-snapshot."));
        remove(&a);
        assert!(!a.exists());
        assert!(TEMPS.lock().unwrap().contains(&b));
        remove(&b);
        assert!(!b.exists());
        assert!(!TEMPS.lock().unwrap().contains(&b));
        let _ = std::fs::remove_dir_all(&d);
    }
}
