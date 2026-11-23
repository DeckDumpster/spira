//! A portable directory watch for the `mail.arrived` source: which watched directories gained a
//! file since the last wait. One trait, two backends: inotify on Linux (close-write and
//! moved-to, the two ways a maildir delivery lands a file in `new/`), and a listing comparison
//! everywhere else. The listing backend is correct anywhere a directory can be read; kqueue can
//! replace it on the BSDs and macOS behind the same trait without the source changing.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::sources::on_pool;

/// Each watched directory that gained files, with their names, in directory order.
pub type Changes = Vec<(PathBuf, Vec<String>)>;

pub type Changed<'a> = Pin<Box<dyn Future<Output = Result<Changes, String>> + Send + 'a>>;

/// Directories watched for new files.
pub trait Watch: Send {
    /// Starts watching `dir`. A directory that does not exist is refused, naming it.
    fn add(&mut self, dir: &Path) -> Result<(), String>;

    /// Resolves with each watched directory that gained a file since the last answer, with the
    /// names of the files that arrived, in directory order. Never resolves with an empty answer.
    fn changed(&mut self) -> Changed<'_>;
}

/// How long the listing backend waits between looks at the directories.
const STEP: Duration = Duration::from_millis(250);

type Seen = BTreeMap<PathBuf, BTreeSet<String>>;

/// The listing backend: each look compares the directories' file names with the last look.
#[derive(Debug, Default)]
pub struct ListingWatch {
    dirs: Arc<Mutex<Seen>>,
}

fn names(dir: &Path) -> Result<BTreeSet<String>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(rd.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect())
}

fn look(dirs: &Mutex<Seen>) -> Changes {
    let mut out = vec![];
    for (dir, seen) in dirs.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        let Ok(now) = names(dir) else { continue };
        let fresh: Vec<String> = now.difference(seen).cloned().collect();
        *seen = now;
        if !fresh.is_empty() {
            out.push((dir.clone(), fresh));
        }
    }
    out
}

impl Watch for ListingWatch {
    fn add(&mut self, dir: &Path) -> Result<(), String> {
        let now = names(dir)?;
        self.dirs.lock().unwrap_or_else(|e| e.into_inner()).insert(dir.to_path_buf(), now);
        Ok(())
    }

    fn changed(&mut self) -> Changed<'_> {
        Box::pin(async move {
            loop {
                let dirs = self.dirs.clone();
                let out = on_pool("mail.listing", Arc::new(move || look(&dirs))).await.map_err(|e| e.to_string())?;
                if !out.is_empty() {
                    return Ok(out);
                }
                tokio::time::sleep(STEP).await; // clock-safe: paces the listing backend's looks on the source's own task
            }
        })
    }
}

/// The inotify backend: the descriptor is registered with the runtime's reactor, so a quiet
/// mailbox costs no wake-ups.
#[cfg(target_os = "linux")]
pub struct InotifyWatch {
    fd: tokio::io::unix::AsyncFd<inotify::Inotify>,
    dirs: BTreeMap<i32, PathBuf>,
    buf: Vec<u8>,
}

/// Every event already queued on `fd`, by directory; ends at `WouldBlock`.
#[cfg(target_os = "linux")]
fn drain(fd: &mut inotify::Inotify, buf: &mut [u8], dirs: &BTreeMap<i32, PathBuf>) -> Result<Changes, String> {
    let mut by_dir: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    loop {
        let events = match fd.read_events(buf) {
            Ok(ev) => ev,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => return Err(format!("inotify read: {e}")),
        };
        let mut any = false;
        for ev in events {
            any = true;
            let Some(dir) = dirs.get(&ev.wd.get_watch_descriptor_id()) else { continue };
            let name = ev.name.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            by_dir.entry(dir.clone()).or_default().push(name);
        }
        if !any {
            break;
        }
    }
    Ok(by_dir.into_iter().collect())
}

#[cfg(target_os = "linux")]
impl InotifyWatch {
    /// Must be called on the runtime that will await [`Watch::changed`].
    pub fn new() -> Result<Self, String> {
        let fd = inotify::Inotify::init().map_err(|e| format!("inotify: {e}"))?;
        let fd = tokio::io::unix::AsyncFd::new(fd).map_err(|e| format!("inotify: {e}"))?;
        Ok(InotifyWatch { fd, dirs: BTreeMap::new(), buf: vec![0; 64 * 1024] })
    }
}

#[cfg(target_os = "linux")]
impl Watch for InotifyWatch {
    fn add(&mut self, dir: &Path) -> Result<(), String> {
        if !dir.is_dir() {
            return Err(format!("{}: not a directory", dir.display()));
        }
        let wd = self.fd.get_ref().watches().add(dir, inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO).map_err(|e| format!("inotify watch {}: {e}", dir.display()))?;
        self.dirs.insert(wd.get_watch_descriptor_id(), dir.to_path_buf());
        Ok(())
    }

    fn changed(&mut self) -> Changed<'_> {
        Box::pin(async move {
            loop {
                let mut ready = self.fd.readable_mut().await.map_err(|e| format!("inotify: {e}"))?;
                let out = drain(ready.get_inner_mut(), &mut self.buf, &self.dirs)?;
                ready.clear_ready();
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        })
    }
}

/// The platform's best watch: inotify on Linux, the listing backend elsewhere. Called on the
/// runtime that will await it.
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

    async fn quiet(w: &mut dyn Watch) -> bool {
        tokio::time::timeout(Duration::from_millis(400), w.changed()).await.is_err()
    }

    async fn exercise(mut w: Box<dyn Watch>) {
        let dir = testkit::TempDir::new("watch");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("old"), "x").unwrap();
        w.add(&a).unwrap();
        w.add(&b).unwrap();
        assert!(w.add(&dir.path().join("missing")).is_err(), "a missing directory is refused");
        assert!(quiet(&mut *w).await, "a file there before the watch is not news");
        std::fs::write(b.join("m1"), "x").unwrap();
        let got = w.changed().await.unwrap();
        assert_eq!(got, vec![(b.clone(), vec!["m1".to_string()])]);
        assert!(quiet(&mut *w).await, "each arrival is reported once");
    }

    #[test_id(t0x01hn)]
    #[tokio::test]
    async fn the_listing_watch_reports_each_new_file_once_by_directory() {
        exercise(Box::new(ListingWatch::default())).await;
    }

    #[cfg(target_os = "linux")]
    #[test_id(t53q3zm)]
    #[tokio::test]
    async fn the_inotify_watch_reports_each_new_file_once_by_directory() {
        exercise(Box::new(InotifyWatch::new().unwrap())).await;
    }
}
