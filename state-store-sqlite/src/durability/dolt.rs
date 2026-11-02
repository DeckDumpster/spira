//! A dolt store's off-box copy, put-only and incremental. `DOLT_BACKUP('sync-url')` keeps a local
//! backup directory of content-addressed table files plus one mutable `manifest` naming the set
//! the head needs. Each run uploads the named files the ledger has not seen, under
//! `<db>/files/<name>`, then the manifest under a key no run reuses, `<db>/manifests/<stamp>-<head>`.
//! A restore is the newest manifest and the files it names (law-off-box-backups-are-private-and-write-only).
//!
//! `state` holds the run's own bookkeeping:
//!
//! - `ledger`: `<file> <bytes>` per table file uploaded, advanced only after the manifest naming
//!   it is uploaded, so a run that dies midway re-sends its files and the previous manifest
//!   stays whole;
//! - `manifests/<stamp>-<head>`: the local copy of each manifest exactly as uploaded, which the
//!   drill restores from.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::put::Put;
use super::ship::{names, write_durably};

pub const FILES: &str = "files";
pub const MANIFESTS: &str = "manifests";
pub const LEDGER: &str = "ledger";
const MANIFEST: &str = "manifest";
/// Local manifests kept; older ones live off-box only.
pub const KEEP_MANIFESTS: usize = 2;
/// Table-file suffixes dolt writes beside the bare name the manifest records.
const SUFFIXES: [&str; 2] = ["", ".darc"];

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

/// The table files a dolt v5 manifest names
/// (`5:<format>:<lock>:<root>:<gc gen>:<name>:<chunks>...`), in its order.
pub fn table_files(manifest: &str) -> Result<Vec<String>, String> {
    let fields: Vec<&str> = manifest.trim().split(':').collect();
    if fields.first() != Some(&"5") || fields.len() < 5 || !(fields.len() - 5).is_multiple_of(2) {
        return Err(format!("not a dolt v5 manifest: {}", manifest.trim()));
    }
    let names: Vec<String> = fields[5..].chunks(2).map(|p| p[0].to_string()).collect();
    if let Some(bad) = names.iter().find(|n| !safe(n)) {
        return Err(format!(
            "the manifest names a table file {bad:?}, not a content hash"
        ));
    }
    Ok(names)
}

fn safe(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Each name the manifest records, resolved to the file in `dir` that holds it.
fn resolve(dir: &Path, manifest: &str) -> Result<Vec<(String, u64)>, String> {
    table_files(manifest)?
        .into_iter()
        .map(|n| {
            SUFFIXES
                .iter()
                .map(|s| format!("{n}{s}"))
                .find_map(|f| fs::metadata(dir.join(&f)).ok().map(|m| (f, m.len())))
                .ok_or_else(|| {
                    format!(
                        "{}: the manifest names {n} and no file holds it",
                        dir.display()
                    )
                })
        })
        .collect()
}

fn read_ledger(state: &Path) -> Result<BTreeMap<String, u64>, String> {
    let path = state.join(LEDGER);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(io(&path)(e)),
    };
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.split_once(' ')
                .and_then(|(f, b)| Some((f.to_string(), b.parse().ok()?)))
                .ok_or_else(|| format!("{}: unreadable line {l:?}", path.display()))
        })
        .collect()
}

/// The key a run's manifest is uploaded under, below `<db>/manifests/`.
pub fn manifest_name(stamp: &str, head: &str) -> Result<String, String> {
    if !safe(stamp) || !safe(head) {
        return Err(format!(
            "stamp {stamp:?} and head {head:?} must be letters and digits"
        ));
    }
    Ok(format!("{stamp}-{head}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shipped {
    /// The manifest's key under the destination.
    pub key: String,
    /// The table files this run uploaded, in the manifest's order.
    pub uploaded: Vec<String>,
    /// Bytes uploaded, the manifest included.
    pub bytes: u64,
}

/// One run: every table file `sync/manifest` names that the ledger lacks, then that manifest
/// under `<db>/manifests/<stamp>-<head>`, then the ledger and the local manifest copy.
pub fn ship(
    put: &dyn Put,
    sync: &Path,
    state: &Path,
    db: &str,
    stamp: &str,
    head: &str,
) -> Result<Shipped, String> {
    if db.is_empty()
        || !db
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("database name {db:?} is not a key segment"));
    }
    let name = manifest_name(stamp, head)?;
    // "Newest" is the greatest key, so a stamp that does not sort after the last one would
    // make an older manifest the restore's, or overwrite a key.
    if let Ok(last) = newest_manifest(state) {
        let last = last.file_name().map(|n| n.to_string_lossy().into_owned());
        let last_stamp = last
            .as_deref()
            .and_then(|n| n.split_once('-'))
            .map(|(s, _)| s);
        if last_stamp.is_some_and(|l| stamp <= l) {
            return Err(format!(
                "stamp {stamp} does not sort after the newest uploaded manifest {}: the clock went back or two runs shared a stamp",
                last.unwrap_or_default()
            ));
        }
    }
    let manifest_path = sync.join(MANIFEST);
    let manifest = fs::read(&manifest_path).map_err(io(&manifest_path))?;
    let text = String::from_utf8(manifest.clone())
        .map_err(|_| format!("{}: not text", manifest_path.display()))?;
    let set = resolve(sync, &text)?;
    let mut ledger = read_ledger(state)?;
    let mut shipped = Shipped {
        key: format!("{db}/{MANIFESTS}/{name}"),
        uploaded: Vec::new(),
        bytes: 0,
    };
    for (file, len) in &set {
        match ledger.get(file) {
            Some(had) if had == len => continue,
            Some(had) => {
                return Err(format!(
                    "{file} is {len} bytes and was uploaded at {had}: a content-addressed table file changed"
                ))
            }
            None => {}
        }
        put.put(&sync.join(file), &format!("{db}/{FILES}/{file}"))?;
        shipped.uploaded.push(file.clone());
        shipped.bytes += len;
    }
    let pending = state.join(format!("{MANIFESTS}.pending"));
    write_durably(&pending, &manifest)?;
    let sent = put.put(&pending, &shipped.key);
    if let Err(e) = sent {
        let _ = fs::remove_file(&pending);
        return Err(e);
    }
    shipped.bytes += manifest.len() as u64;
    ledger.extend(set);
    let lines: String = ledger.iter().map(|(f, b)| format!("{f} {b}\n")).collect();
    write_durably(&state.join(LEDGER), lines.as_bytes())?;
    let local = state.join(MANIFESTS);
    fs::create_dir_all(&local).map_err(io(&local))?;
    fs::rename(&pending, local.join(&name)).map_err(io(&local))?;
    let kept = names(&local)?;
    for old in kept.iter().take(kept.len().saturating_sub(KEEP_MANIFESTS)) {
        fs::remove_file(local.join(old)).map_err(io(&local.join(old)))?;
    }
    Ok(shipped)
}

/// The newest local manifest copy, by name (`<stamp>-<head>` sorts by stamp).
pub fn newest_manifest(state: &Path) -> Result<PathBuf, String> {
    let dir = state.join(MANIFESTS);
    names(&dir)?.pop().map(|n| dir.join(n)).ok_or_else(|| {
        format!(
            "no manifest in {}: nothing has been uploaded",
            dir.display()
        )
    })
}

/// The head a manifest copy was uploaded at: its name's suffix after the stamp.
pub fn manifest_head(manifest: &Path) -> Option<String> {
    let name = manifest.file_name()?.to_str()?;
    name.split_once('-').map(|(_, h)| h.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembled {
    pub head: String,
    pub files: usize,
    /// The table files' total size: what a restore of this set copies.
    pub bytes: u64,
}

/// A fresh restore source in `out`, exactly as an off-box restore lays one down: `manifest` and
/// a link to each file it names in `files`, nothing else. `files` is the local backup directory
/// for the drill, or the downloaded `<db>/files/` for an operator restore.
pub fn assemble(manifest: &Path, files: &Path, out: &Path) -> Result<Assembled, String> {
    let head = manifest_head(manifest)
        .ok_or_else(|| format!("{}: not a <stamp>-<head> manifest", manifest.display()))?;
    let text = fs::read_to_string(manifest).map_err(io(manifest))?;
    let set = resolve(files, &text)?;
    if out.exists() {
        return Err(format!("{} exists", out.display()));
    }
    fs::create_dir_all(out).map_err(io(out))?;
    fs::copy(manifest, out.join(MANIFEST)).map_err(io(out))?;
    let files = fs::canonicalize(files).map_err(io(files))?;
    for (f, _) in &set {
        std::os::unix::fs::symlink(files.join(f), out.join(f)).map_err(io(&out.join(f)))?;
    }
    Ok(Assembled {
        head,
        files: set.len(),
        bytes: set.iter().map(|(_, b)| b).sum(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durability::put::{Dest, Uploader};
    use runtime::ports::SysProc;
    use std::sync::Arc;
    use std::time::Duration;
    use testkit::TempDir;

    const LOCK: &str = "i2fqd6p4njli01nkvtcqjt1u1fb05ngd";
    const ROOT: &str = "t3q0fttlelv0ja442502htmaqjnpds83";
    const GEN: &str = "00000000000000000000000000000000";

    /// A dolt backup directory as `DOLT_BACKUP('sync-url')` leaves it: `files` as table files,
    /// a manifest naming them, and dolt's own `LOCK` and `oldgen/`.
    fn sync_to(dir: &Path, files: &[(&str, &str)]) {
        fs::create_dir_all(dir.join("oldgen")).unwrap();
        fs::write(dir.join("LOCK"), "").unwrap();
        let mut m = format!("5:__DOLT__:{LOCK}:{ROOT}:{GEN}");
        for (name, body) in files {
            fs::write(dir.join(format!("{name}.darc")), body).unwrap();
            m.push_str(&format!(":{name}:1"));
        }
        fs::write(dir.join(MANIFEST), m).unwrap();
    }

    fn uploader(dest: &Path) -> Uploader {
        Uploader {
            dest: Dest::File(dest.into()),
            creds: None,
            proc: Arc::new(SysProc),
            deadline: Duration::from_secs(5),
        }
    }

    fn all_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for sub in [FILES, MANIFESTS] {
            out.extend(
                names(&dir.join("d0").join(sub))
                    .unwrap()
                    .into_iter()
                    .map(|n| format!("{sub}/{n}")),
            );
        }
        out
    }

    /// Uploads through `inner` and dies at the first manifest, as a run killed between its
    /// file PUTs and its manifest PUT.
    struct DiesAtManifest(Uploader);
    impl Put for DiesAtManifest {
        fn put(&self, file: &Path, name: &str) -> Result<(), String> {
            if name.contains("/manifests/") {
                return Err("killed".into());
            }
            self.0.put(file, name)
        }
    }

    #[test]
    fn a_second_run_after_one_commit_uploads_only_the_new_table_files_and_one_manifest() {
        let t = TempDir::new("dolt-ship-incremental");
        let (sync, state, dest) = (
            t.path().join("sync"),
            t.path().join("state"),
            t.path().join("dest"),
        );
        let put = uploader(&dest);
        sync_to(&sync, &[("aaaa", "first"), ("bbbb", "second")]);
        let one = ship(&put, &sync, &state, "d0", "20261102T010000000Z", "h1").unwrap();
        assert_eq!(one.uploaded, ["aaaa.darc", "bbbb.darc"]);
        assert_eq!(one.key, "d0/manifests/20261102T010000000Z-h1");

        sync_to(
            &sync,
            &[("cccc", "third"), ("aaaa", "first"), ("bbbb", "second")],
        );
        let two = ship(&put, &sync, &state, "d0", "20261102T020000000Z", "h2").unwrap();
        assert_eq!(two.uploaded, ["cccc.darc"]);
        let manifest_len = fs::metadata(sync.join(MANIFEST)).unwrap().len();
        assert_eq!(two.bytes, "third".len() as u64 + manifest_len);
        assert_eq!(
            all_under(&dest),
            [
                "files/aaaa.darc",
                "files/bbbb.darc",
                "files/cccc.darc",
                "manifests/20261102T010000000Z-h1",
                "manifests/20261102T020000000Z-h2"
            ]
        );
        assert_eq!(
            fs::read(dest.join("d0/manifests/20261102T020000000Z-h2")).unwrap(),
            fs::read(sync.join(MANIFEST)).unwrap()
        );
        assert!(
            !dest.join("d0/files/LOCK").exists(),
            "dolt's own LOCK is not a table file"
        );
    }

    #[test]
    fn a_crash_between_the_file_puts_and_the_manifest_put_leaves_the_previous_manifest_restorable()
    {
        let t = TempDir::new("dolt-ship-crash");
        let (sync, state, dest) = (
            t.path().join("sync"),
            t.path().join("state"),
            t.path().join("dest"),
        );
        sync_to(&sync, &[("aaaa", "first")]);
        ship(
            &uploader(&dest),
            &sync,
            &state,
            "d0",
            "20261102T010000000Z",
            "h1",
        )
        .unwrap();
        let ledger = fs::read_to_string(state.join(LEDGER)).unwrap();

        sync_to(&sync, &[("cccc", "third")]);
        let err = ship(
            &DiesAtManifest(uploader(&dest)),
            &sync,
            &state,
            "d0",
            "20261102T020000000Z",
            "h2",
        )
        .unwrap_err();
        assert_eq!(err, "killed");
        assert_eq!(
            names(&dest.join("d0").join(MANIFESTS)).unwrap(),
            ["20261102T010000000Z-h1"]
        );
        assert_eq!(
            fs::read_to_string(state.join(LEDGER)).unwrap(),
            ledger,
            "the ledger holds at the last manifest"
        );
        assert_eq!(
            newest_manifest(&state).unwrap(),
            state.join("manifests/20261102T010000000Z-h1")
        );

        let off_box = dest.join("d0");
        let out = t.path().join("restore");
        let got = assemble(
            &off_box.join("manifests/20261102T010000000Z-h1"),
            &off_box.join(FILES),
            &out,
        )
        .unwrap();
        assert_eq!(
            got,
            Assembled {
                head: "h1".into(),
                files: 1,
                bytes: 5
            }
        );
        assert_eq!(fs::read_to_string(out.join("aaaa.darc")).unwrap(), "first");

        let again = ship(
            &uploader(&dest),
            &sync,
            &state,
            "d0",
            "20261102T030000000Z",
            "h2",
        )
        .unwrap();
        assert_eq!(
            again.uploaded,
            ["cccc.darc"],
            "the next run re-sends what the dead one left unrecorded"
        );
    }

    #[test]
    fn the_drill_assembles_exactly_the_newest_manifests_file_set() {
        let t = TempDir::new("dolt-ship-drill");
        let (sync, state, dest) = (
            t.path().join("sync"),
            t.path().join("state"),
            t.path().join("dest"),
        );
        let put = uploader(&dest);
        for (i, files) in [
            vec![("aaaa", "first")],
            vec![("aaaa", "first"), ("bbbb", "second")],
            vec![("bbbb", "second"), ("dddd", "fourth")],
        ]
        .into_iter()
        .enumerate()
        {
            sync_to(&sync, &files);
            ship(
                &put,
                &sync,
                &state,
                "d0",
                &format!("20261102T0{i}0000000Z"),
                &format!("h{i}"),
            )
            .unwrap();
        }
        assert_eq!(names(&state.join(MANIFESTS)).unwrap().len(), KEEP_MANIFESTS);
        let newest = newest_manifest(&state).unwrap();
        let out = t.path().join("drill/snap");
        let got = assemble(&newest, &sync, &out).unwrap();
        assert_eq!(
            got,
            Assembled {
                head: "h2".into(),
                files: 2,
                bytes: 12
            }
        );
        assert_eq!(
            names(&out).unwrap(),
            ["bbbb.darc", "dddd.darc", "manifest"],
            "aaaa.darc is in the dir and not in the set"
        );
        assert!(
            assemble(&newest, &sync, &out).is_err(),
            "an existing restore dir is refused"
        );

        fs::remove_file(sync.join("dddd.darc")).unwrap();
        let err = assemble(&newest, &sync, &t.path().join("drill/again")).unwrap_err();
        assert!(
            err.contains("the manifest names dddd and no file holds it"),
            "{err}"
        );
    }

    #[test]
    fn a_stamp_that_does_not_sort_after_the_newest_manifest_is_refused() {
        let t = TempDir::new("dolt-ship-stamp");
        let (sync, state, dest) = (
            t.path().join("sync"),
            t.path().join("state"),
            t.path().join("dest"),
        );
        sync_to(&sync, &[("aaaa", "first")]);
        ship(
            &uploader(&dest),
            &sync,
            &state,
            "d0",
            "20261102T010000500Z",
            "hb",
        )
        .unwrap();
        for stamp in ["20261102T010000500Z", "20261102T010000499Z"] {
            let err = ship(&uploader(&dest), &sync, &state, "d0", stamp, "ha").unwrap_err();
            assert!(
                err.contains("does not sort after the newest uploaded manifest"),
                "{err}"
            );
        }
        assert_eq!(
            names(&dest.join("d0").join(MANIFESTS)).unwrap(),
            ["20261102T010000500Z-hb"]
        );
    }

    #[test]
    fn a_table_file_whose_size_changed_under_its_name_is_refused() {
        let t = TempDir::new("dolt-ship-changed");
        let (sync, state, dest) = (
            t.path().join("sync"),
            t.path().join("state"),
            t.path().join("dest"),
        );
        sync_to(&sync, &[("aaaa", "first")]);
        ship(
            &uploader(&dest),
            &sync,
            &state,
            "d0",
            "20261102T010000000Z",
            "h1",
        )
        .unwrap();
        sync_to(&sync, &[("aaaa", "rewritten")]);
        let err = ship(
            &uploader(&dest),
            &sync,
            &state,
            "d0",
            "20261102T020000000Z",
            "h2",
        )
        .unwrap_err();
        assert!(
            err.contains("a content-addressed table file changed"),
            "{err}"
        );
    }

    #[test]
    fn a_manifest_that_is_not_dolt_v5_or_names_a_path_is_refused() {
        assert_eq!(
            table_files(&format!("5:__DOLT__:{LOCK}:{ROOT}:{GEN}:aaaa:3:bbbb:9\n")).unwrap(),
            ["aaaa", "bbbb"]
        );
        assert_eq!(
            table_files(&format!("5:__DOLT__:{LOCK}:{ROOT}:{GEN}")).unwrap(),
            Vec::<String>::new()
        );
        assert!(table_files("4:x:y:z").is_err());
        assert!(table_files(&format!("5:__DOLT__:{LOCK}:{ROOT}:{GEN}:aaaa")).is_err());
        assert!(table_files(&format!("5:__DOLT__:{LOCK}:{ROOT}:{GEN}:../x:1")).is_err());
        assert!(manifest_name("20261102T010000000Z", "a/b").is_err());
    }
}
