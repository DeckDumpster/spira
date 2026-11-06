//! A portable directory watch for the `mail.arrived` source: which watched directories gained a
//! file since the last wait. One trait, two backends: inotify on Linux (close-write and
//! moved-to, the two ways a maildir delivery lands a file in `new/`), and a listing comparison
//! everywhere else. The listing backend is correct anywhere a directory can be read; kqueue can
//! replace it on the BSDs and macOS behind the same trait without the source changing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Directories watched for new files.
pub trait Watch: Send {
    /// Starts watching `dir`. A directory that does not exist is refused, naming it.
    fn add(&mut self, dir: &Path) -> Result<(), String>;

    /// Waits up to `timeout` and returns each watched directory that gained a file, with the
    /// names of the files that arrived, in directory order. An empty answer is a quiet wait.
    fn wait(&mut self, timeout: Duration) -> Result<Vec<(PathBuf, Vec<String>)>, String>;
}

/// How long one quiet step of a wait sleeps before it looks again.
const STEP: Duration = Duration::from_millis(250);

/// The listing backend: each wait compares the directories' file names with the last look.
#[derive(Debug, Default)]
pub struct ListingWatch {
    dirs: BTreeMap<PathBuf, BTreeSet<String>>,
}

fn names(dir: &Path) -> Result<BTreeSet<String>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(rd.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect())
}

impl ListingWatch {
    fn changed(&mut self) -> Vec<(PathBuf, Vec<String>)> {
        let mut out = vec![];
        for (dir, seen) in self.dirs.iter_mut() {
            let Ok(now) = names(dir) else { continue };
            let fresh: Vec<String> = now.difference(seen).cloned().collect();
            *seen = now;
            if !fresh.is_empty() {
                out.push((dir.clone(), fresh));
            }
        }
        out
    }
}

impl Watch for ListingWatch {
    fn add(&mut self, dir: &Path) -> Result<(), String> {
        let now = names(dir)?;
        self.dirs.insert(dir.to_path_buf(), now);
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
        let mut left = timeout;
        loop {
            let out = self.changed();
            if !out.is_empty() || left.is_zero() {
                return Ok(out);
            }
            let step = left.min(STEP);
            std::thread::sleep(step); // clock-safe: paces a source's own thread between looks at the directories
            left = left.saturating_sub(step);
        }
    }
}

/// The inotify backend.
#[cfg(target_os = "linux")]
pub struct InotifyWatch {
    fd: inotify::Inotify,
    dirs: BTreeMap<i32, PathBuf>,
    buf: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl InotifyWatch {
    pub fn new() -> Result<Self, String> {
        Ok(InotifyWatch { fd: inotify::Inotify::init().map_err(|e| format!("inotify: {e}"))?, dirs: BTreeMap::new(), buf: vec![0; 64 * 1024] })
    }

    /// Every event already queued, by directory; `WouldBlock` is none.
    fn drain(&mut self) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
        let mut by_dir: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        loop {
            let events = match self.fd.read_events(&mut self.buf) {
                Ok(ev) => ev,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(format!("inotify read: {e}")),
            };
            let mut any = false;
            for ev in events {
                any = true;
                let Some(dir) = self.dirs.get(&ev.wd.get_watch_descriptor_id()) else { continue };
                let name = ev.name.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                by_dir.entry(dir.clone()).or_default().push(name);
            }
            if !any {
                break;
            }
        }
        Ok(by_dir.into_iter().collect())
    }
}

#[cfg(target_os = "linux")]
impl Watch for InotifyWatch {
    fn add(&mut self, dir: &Path) -> Result<(), String> {
        if !dir.is_dir() {
            return Err(format!("{}: not a directory", dir.display()));
        }
        let wd = self.fd.watches().add(dir, inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO).map_err(|e| format!("inotify watch {}: {e}", dir.display()))?;
        self.dirs.insert(wd.get_watch_descriptor_id(), dir.to_path_buf());
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
        let mut left = timeout;
        loop {
            let out = self.drain()?;
            if !out.is_empty() || left.is_zero() {
                return Ok(out);
            }
            let step = left.min(STEP);
            std::thread::sleep(step); // clock-safe: paces a source's own thread between non-blocking inotify reads
            left = left.saturating_sub(step);
        }
    }
}

/// The platform's best watch: inotify on Linux, the listing backend elsewhere.
pub fn platform() -> Result<Box<dyn Watch>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(InotifyWatch::new()?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Box::new(ListingWatch::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(mut w: Box<dyn Watch>) {
        let dir = testkit::TempDir::new("watch");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("old"), "x").unwrap();
        w.add(&a).unwrap();
        w.add(&b).unwrap();
        assert!(w.add(&dir.path().join("missing")).is_err(), "a missing directory is refused");
        assert_eq!(w.wait(Duration::ZERO).unwrap(), vec![], "a file there before the watch is not news");
        std::fs::write(b.join("m1"), "x").unwrap();
        let got = w.wait(Duration::from_secs(2)).unwrap();
        assert_eq!(got, vec![(b.clone(), vec!["m1".to_string()])]);
        assert_eq!(w.wait(Duration::ZERO).unwrap(), vec![], "each arrival is reported once");
    }

    #[test]
    fn the_listing_watch_reports_each_new_file_once_by_directory() {
        exercise(Box::new(ListingWatch::default()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_inotify_watch_reports_each_new_file_once_by_directory() {
        exercise(Box::new(InotifyWatch::new().unwrap()));
    }
}
