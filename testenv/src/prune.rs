//! testenv cleans up after itself: the images its builds supersede, and the cargo target
//! volumes its unit tier fills. Without this the container store keeps every image ever
//! built and every intermediate step of each, and the unit target grows with every tree
//! that ever ran, until the disk under the store is full.
//!
//! Everything removed is found through podman's own records (image and volume listings,
//! container listings) and removed by id, never by a name pattern alone. Nothing a
//! container uses is removed, whatever state that container is in, and when podman cannot
//! say which containers exist nothing is removed at all. Every removal is logged with its
//! size.

use crate::container::{human, UNIT_TARGET_VOLUME};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// The repository every testenv image is tagged under.
pub const IMAGE_REPO: &str = "localhost/spira-testenv";

pub struct Store<'a> {
    /// `podman <args>`: rc and stdout.
    pub podman: &'a dyn Fn(&[String]) -> (i32, String),
    pub log: &'a dyn Fn(&str),
}

#[derive(Deserialize)]
struct Image {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Names", default)]
    names: Option<Vec<String>>,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "Containers", default)]
    containers: u64,
    #[serde(rename = "Created", default)]
    created: i64,
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "ImageID", default)]
    image_id: String,
}

#[derive(Deserialize)]
struct Volume {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Mountpoint", default)]
    mountpoint: String,
    #[serde(rename = "MountCount", default)]
    mount_count: u64,
}

/// A volume testenv's unit tier mounts as its cargo target: the unit target volume's own
/// name, or that name followed by `-` and whatever keys it.
pub fn is_unit_target_volume(name: &str) -> bool {
    name == UNIT_TARGET_VOLUME
        || name
            .strip_prefix(UNIT_TARGET_VOLUME)
            .is_some_and(|rest| rest.starts_with('-') && rest.len() > 1)
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

impl Store<'_> {
    fn json<T: for<'de> Deserialize<'de>>(&self, args: &[&str]) -> Option<Vec<T>> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (rc, out) = (self.podman)(&args);
        // podman prints `[]` for an empty listing: no output at all is not an answer.
        if rc != 0 || out.trim().is_empty() {
            return None;
        }
        serde_json::from_str::<Option<Vec<T>>>(&out)
            .ok()
            .map(Option::unwrap_or_default)
    }

    fn ok(&self, args: &[&str]) -> bool {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        (self.podman)(&args).0 == 0
    }

    /// Every image id some container, in any state, was created from. None when podman
    /// cannot say: the caller then removes nothing.
    fn images_in_use(&self) -> Option<BTreeSet<String>> {
        let cs: Vec<Container> = self.json(&["ps", "-a", "--format", "json"])?;
        Some(
            cs.into_iter()
                .map(|c| c.image_id)
                .filter(|i| !i.is_empty())
                .collect(),
        )
    }

    fn image_in_use(img: &Image, used: &BTreeSet<String>) -> bool {
        img.containers > 0
            || used
                .iter()
                .any(|u| u == &img.id || (u.len() >= 12 && img.id.starts_with(u.as_str())))
    }

    /// Removes `img` by id. podman refuses an id that carries more than one name (a pulled
    /// image is also tagged under the registry's ref), so such an image is untagged first —
    /// only ever after the caller has found no container using it. podman also deletes the
    /// untagged parents nothing else needs, and says so on stdout; the count is logged.
    fn remove_image(&self, img: &Image, what: &str) -> bool {
        if img.names.as_ref().is_some_and(|n| n.len() > 1) && !self.ok(&["untag", &img.id]) {
            (self.log)(&format!(
                "testenv: prune: podman refused to untag {what} image {} ({}) — kept",
                short(&img.id),
                human(img.size)
            ));
            return false;
        }
        let args: Vec<String> = ["rmi", &img.id].iter().map(|a| a.to_string()).collect();
        let (rc, out) = (self.podman)(&args);
        if rc == 0 {
            let deleted = out.lines().filter(|l| l.starts_with("Deleted:")).count();
            let parents = match deleted {
                0 | 1 => String::new(),
                n => format!(", with {} untagged parent image(s) only it used", n - 1),
            };
            (self.log)(&format!(
                "testenv: prune: removed {what} image {} ({}{parents})",
                short(&img.id),
                human(img.size)
            ));
            true
        } else {
            (self.log)(&format!(
                "testenv: prune: podman refused to remove {what} image {} ({}) — kept",
                short(&img.id),
                human(img.size)
            ));
            false
        }
    }

    /// After `current` (a `localhost/spira-testenv:<tag>` ref) was built or pulled: remove
    /// every testenv image except `current` and the newest other one, then the dangling
    /// images created at or after `since` (a build's own start, in epoch seconds, so a
    /// build removes its own leftovers and never another build's in flight; None = every
    /// dangling image), skipping anything a container uses. Returns the bytes removed.
    pub fn prune_images(&self, current: &str, since: Option<i64>) -> u64 {
        let Some(used) = self.images_in_use() else {
            (self.log)("testenv: prune: podman cannot list containers — removing no image");
            return 0;
        };
        let reference = format!("reference={IMAGE_REPO}");
        let Some(mut tagged) =
            self.json::<Image>(&["images", "--format", "json", "--filter", &reference])
        else {
            (self.log)("testenv: prune: podman cannot list testenv images — removing no image");
            return 0;
        };
        let names = |i: &Image| i.names.clone().unwrap_or_default();
        let Some(cur) = tagged
            .iter()
            .position(|i| names(i).iter().any(|n| n == current))
        else {
            (self.log)(&format!(
                "testenv: prune: {current} is not in the store's records — removing no image"
            ));
            return 0;
        };
        let cur_id = tagged.remove(cur).id;
        tagged.retain(|i| i.id != cur_id);
        tagged.sort_by_key(|i| std::cmp::Reverse(i.created));
        let mut freed = 0;
        for (n, img) in tagged.iter().enumerate() {
            let label = names(img).join(",");
            if n == 0 {
                (self.log)(&format!(
                    "testenv: prune: kept {label} ({}) — the predecessor of {current}",
                    short(&img.id)
                ));
            } else if Self::image_in_use(img, &used) {
                (self.log)(&format!(
                    "testenv: prune: kept {label} ({}) — a container uses it",
                    short(&img.id)
                ));
            } else if self.remove_image(img, &format!("superseded {label}")) {
                freed += img.size;
            }
        }
        let Some(dangling) =
            self.json::<Image>(&["images", "--format", "json", "--filter", "dangling=true"])
        else {
            (self.log)("testenv: prune: podman cannot list dangling images — removing none");
            return freed;
        };
        let mine = |i: &&Image| since.is_none_or(|t| i.created >= t);
        for img in dangling
            .iter()
            .filter(mine)
            .filter(|i| !Self::image_in_use(i, &used))
        {
            if self.remove_image(img, "dangling") {
                freed += img.size;
            }
        }
        (self.log)(&format!("testenv: prune: freed {} of images", human(freed)));
        freed
    }

    /// The unit tier's target volumes hold at most `cap` bytes between them: above it, the
    /// least recently used volume no container uses is removed, until the rest fit (podman
    /// recreates an absent named volume, empty, on the next mount). 0 disables the cap.
    /// Returns the bytes removed.
    pub fn sweep_target_volumes(&self, cap: u64) -> u64 {
        self.sweep_target_volumes_with(cap, &fs_used, &measure)
    }

    /// [`sweep_target_volumes`](Self::sweep_target_volumes) with the filesystem probe and the
    /// walk injected. MEASURED ONLY WHEN IT COULD MATTER (sp-ipwlic, over-broad-scopes #13):
    /// the volumes cannot hold more than the filesystems they live on use, so when that
    /// `statvfs` bound is within the cap nothing is walked — the walk read every file of
    /// gigabytes of cargo targets on every unit-tier start. A filesystem that cannot be probed
    /// leaves no bound, and every volume is measured as before.
    pub fn sweep_target_volumes_with(&self, cap: u64, fs_used: &dyn Fn(&Path) -> Option<(u64, u64)>, measure: &dyn Fn(&Path) -> (u64, u64)) -> u64 {
        if cap == 0 {
            return 0;
        }
        let Some(all) = self.json::<Volume>(&["volume", "ls", "--format", "json"]) else {
            (self.log)("testenv: sweep: podman cannot list volumes — sweeping none");
            return 0;
        };
        let all: Vec<Volume> = all.into_iter().filter(|v| is_unit_target_volume(&v.name)).collect();
        let mut used: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
        let bounded = all.iter().all(|v| match fs_used(Path::new(&v.mountpoint)) {
            Some((dev, bytes)) => {
                used.insert(dev, bytes);
                true
            }
            None => false,
        });
        if bounded && used.values().sum::<u64>() <= cap {
            return 0;
        }
        let mut vols: Vec<(Volume, u64, u64)> = all
            .into_iter()
            .map(|v| {
                let (bytes, last) = measure(Path::new(&v.mountpoint));
                (v, bytes, last)
            })
            .collect();
        let mut total: u64 = vols.iter().map(|v| v.1).sum();
        if total <= cap {
            return 0;
        }
        (self.log)(&format!(
            "testenv: sweep: unit target volumes hold {}, over the {} cap",
            human(total),
            human(cap)
        ));
        vols.sort_by_key(|v| v.2);
        let mut freed = 0;
        for (v, bytes, _) in &vols {
            if total <= cap {
                break;
            }
            if v.mount_count > 0 || self.volume_users(&v.name).is_none_or(|n| n > 0) {
                (self.log)(&format!(
                    "testenv: sweep: kept volume {} ({}) — a container uses it",
                    v.name,
                    human(*bytes)
                ));
                continue;
            }
            if self.ok(&["volume", "rm", &v.name]) {
                (self.log)(&format!(
                    "testenv: sweep: removed volume {} ({})",
                    v.name,
                    human(*bytes)
                ));
                total = total.saturating_sub(*bytes);
                freed += bytes;
            } else {
                (self.log)(&format!(
                    "testenv: sweep: podman refused to remove volume {} ({}) — kept",
                    v.name,
                    human(*bytes)
                ));
            }
        }
        if total > cap {
            (self.log)(&format!(
                "testenv: sweep: still {} after the sweep, over the {} cap — what is left is in use",
                human(total),
                human(cap)
            ));
        }
        freed
    }

    /// How many containers, in any state, mount `volume`; None when podman cannot say.
    fn volume_users(&self, volume: &str) -> Option<usize> {
        let filter = format!("volume={volume}");
        let args: Vec<String> = ["ps", "-a", "--filter", &filter, "--format", "{{.ID}}"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        let (rc, out) = (self.podman)(&args);
        (rc == 0).then(|| out.lines().filter(|l| !l.trim().is_empty()).count())
    }
}

/// The filesystem holding `path` (its device) and the bytes in use on it, from `statvfs`;
/// `None` when either cannot be read.
pub fn fs_used(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let dev = fs::metadata(path).ok()?.dev();
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs writes into the zeroed struct we own; `c` outlives the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    Some((dev, (st.f_blocks as u64).saturating_sub(st.f_bfree as u64).saturating_mul(frsize)))
}

/// The disk `dir` takes (allocated blocks, a hard-linked file counted once: cargo links
/// each binary from `deps/` to the profile root) and the newest modification time in it
/// (seconds since the epoch), not following symlinks. A missing directory is empty.
pub fn measure(dir: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let mut bytes = 0;
    let mut newest = 0;
    let mut seen = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(meta) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            newest = newest.max(mtime);
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
                bytes += meta.blocks() * 512;
            }
        }
    }
    (bytes, newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Over-broad-scopes #13 (sp-ipwlic): the target volumes are walked only when the
    /// filesystems they live on use more than the cap; under it, or with the cap off, nothing
    /// is walked; a filesystem that cannot be probed is walked as before.
    #[test]
    fn target_volumes_are_measured_only_when_their_filesystem_could_exceed_the_cap() {
        let listing = format!(
            "[{{\"Name\":\"{UNIT_TARGET_VOLUME}-a\",\"Mountpoint\":\"/v/a\",\"MountCount\":1}},{{\"Name\":\"{UNIT_TARGET_VOLUME}-b\",\"Mountpoint\":\"/v/b\",\"MountCount\":1}},{{\"Name\":\"other\",\"Mountpoint\":\"/v/o\"}}]"
        );
        let podman = |a: &[String]| if a[0] == "volume" && a[1] == "ls" { (0, listing.clone()) } else { (0, String::new()) };
        let log = |_: &str| {};
        let st = Store { podman: &podman, log: &log };
        let walked = std::cell::RefCell::new(Vec::<String>::new());
        let walk = |p: &Path| {
            walked.borrow_mut().push(p.display().to_string());
            (10, 0)
        };
        assert_eq!(st.sweep_target_volumes_with(100, &|_| Some((1, 50)), &walk), 0);
        assert!(walked.borrow().is_empty(), "the filesystem uses 50 of a 100 cap: nothing walked");
        st.sweep_target_volumes_with(0, &|_| None, &walk);
        assert!(walked.borrow().is_empty(), "the cap is off");
        st.sweep_target_volumes_with(100, &|_| Some((1, 500)), &walk);
        assert_eq!(*walked.borrow(), ["/v/a", "/v/b"], "over the bound: each unit volume is measured");
        walked.borrow_mut().clear();
        st.sweep_target_volumes_with(100, &|p| (p != Path::new("/v/b")).then_some((1, 5)), &walk);
        assert_eq!(walked.borrow().len(), 2, "an unprobed filesystem leaves no bound");
    }

    #[test]
    fn the_unit_target_volume_and_its_keyed_variants_are_swept_and_nothing_else() {
        assert!(is_unit_target_volume(UNIT_TARGET_VOLUME));
        assert!(is_unit_target_volume(&format!("{UNIT_TARGET_VOLUME}-3f2a")));
        assert!(!is_unit_target_volume(&format!("{UNIT_TARGET_VOLUME}-")));
        assert!(!is_unit_target_volume(&format!("{UNIT_TARGET_VOLUME}x")));
        assert!(!is_unit_target_volume("spira-testenv-cargo-reg"));
    }
}
