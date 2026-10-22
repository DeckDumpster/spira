// Retention for run/tsd/: drop rows older than a per-family number of days, atomically.
//
// A prune streams the file into a sibling temp file without holding the appenders' lock, then
// takes the lock, copies whatever was appended meanwhile, and renames the temp over the family
// file. Appenders re-check their inode after taking the lock (lib.rs), so a row appended to the
// replaced file is never lost. Only `<family>.jsonl` files are touched; markers, locks and
// checkpoints in the same directory are not families.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// The longest trailing window, in days, any reader queries a family over. A family is kept at
/// least this long whatever the config says; a family absent here is bounded only by one day.
/// suite-timing and the landing-path families are read as "the last N runs", with no time
/// window, so they are floored at the default retention.
pub const READER_WINDOW_DAYS: &[(&str, u32)] = &[
    ("suite-timing", 30),
    ("gate-run", 30),
    ("cert-pass", 30),
    ("batch-round", 30),
    ("admission", 30),
    ("slots", 30),
    ("escape", 7),
    ("backlog", 1),
];

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub struct Pruned {
    pub kept: u64,
    pub dropped: u64,
}

pub fn floor_days(family: &str) -> u32 {
    READER_WINDOW_DAYS.iter().find(|(f, _)| *f == family).map_or(1, |(_, d)| *d)
}

/// `family=days` pairs, whitespace or comma separated. Anything unparsable is an error: a
/// retention that guessed would delete rows nobody asked to lose.
pub fn parse_overrides(s: &str) -> Result<BTreeMap<String, u32>, String> {
    let mut out = BTreeMap::new();
    for pair in s.split(|c: char| c.is_whitespace() || c == ',').filter(|p| !p.is_empty()) {
        let (fam, days) = pair.split_once('=').ok_or_else(|| format!("{pair:?} is not family=days"))?;
        if !crate::valid_family(fam) {
            return Err(format!("{fam:?} is not a family name"));
        }
        let days: u32 = days.parse().map_err(|_| format!("{pair:?}: days is not a whole number"))?;
        out.insert(fam.to_string(), days);
    }
    Ok(out)
}

/// The days a family is kept: its override or the default, never below its readers' window.
pub fn keep_days(family: &str, default: u32, overrides: &BTreeMap<String, u32>) -> u32 {
    overrides.get(family).copied().unwrap_or(default).max(floor_days(family))
}

fn row_epoch(line: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"ts\":\"";
    let at = line.windows(KEY.len()).position(|w| w == KEY)? + KEY.len();
    let end = line[at..].iter().position(|&b| b == b'"')?;
    crate::parse_ts(std::str::from_utf8(&line[at..at + end]).ok()?)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".prune.tmp");
    PathBuf::from(s)
}

/// Rewrite `path` without the rows stamped before `cutoff` (epoch seconds). A row whose `ts`
/// cannot be read is kept. Nothing is touched when no row is old enough.
pub fn prune_file(path: &Path, cutoff: u64) -> Result<Pruned, String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let src = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pruned::default()),
        Err(e) => return Err(err(e)),
    };
    let ino = src.metadata().map_err(err)?.ino();
    let tmp = tmp_path(path);
    let result = (|| -> Result<Pruned, String> {
        let mut out = File::create(&tmp).map_err(err)?;
        let mut stats = Pruned::default();
        let mut pos: u64 = 0;
        let mut rd = BufReader::with_capacity(1 << 20, src);
        let mut line = Vec::new();
        {
            let mut w = std::io::BufWriter::with_capacity(1 << 20, &mut out);
            loop {
                line.clear();
                let n = rd.read_until(b'\n', &mut line).map_err(err)?;
                if n == 0 || line.last() != Some(&b'\n') {
                    break;
                }
                pos += n as u64;
                if row_epoch(&line).is_some_and(|t| t < cutoff) {
                    stats.dropped += 1;
                } else {
                    stats.kept += 1;
                    w.write_all(&line).map_err(err)?;
                }
            }
            w.flush().map_err(err)?;
        }
        if stats.dropped == 0 {
            return Ok(stats);
        }
        let mut live = OpenOptions::new().read(true).append(true).open(path).map_err(err)?;
        let fd = live.as_raw_fd();
        if unsafe { crate::flock(fd, crate::LOCK_EX) } != 0 {
            return Err(format!("{}: flock failed", path.display()));
        }
        let swap = (|| -> Result<(), String> {
            if live.metadata().map_err(err)?.ino() != ino {
                return Err(format!("{}: replaced while pruning; left for the next pass", path.display()));
            }
            live.seek(SeekFrom::Start(pos)).map_err(err)?;
            let mut tail = Vec::new();
            live.read_to_end(&mut tail).map_err(err)?;
            out.write_all(&tail).map_err(err)?;
            out.sync_all().map_err(err)?;
            std::fs::rename(&tmp, path).map_err(err)
        })();
        let _ = unsafe { crate::flock(fd, crate::LOCK_UN) };
        swap.map(|()| stats)
    })();
    if result.as_ref().map_or(true, |s| s.dropped == 0) {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Prune every `<family>.jsonl` under `<root>/tsd/` to its bound as of `now` (epoch seconds).
/// Returns each family's days kept and what the prune did; a family that fails is reported
/// and the others still run.
pub fn prune_root(
    root: &Path,
    default_days: u32,
    overrides: &BTreeMap<String, u32>,
    now: u64,
) -> Vec<(String, u32, Result<Pruned, String>)> {
    let dir = root.join("tsd");
    let mut families: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".jsonl").map(str::to_string))
        .filter(|f| crate::valid_family(f))
        .collect();
    families.sort();
    families
        .into_iter()
        .map(|f| {
            let days = keep_days(&f, default_days, overrides);
            let cutoff = now.saturating_sub(u64::from(days) * 86_400);
            let r = prune_file(&crate::family_path(root, &f), cutoff);
            (f, days, r)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_790_000_000;

    fn row(age_days: u64, n: u32) -> String {
        format!("{{\"ts\":\"{}\",\"family\":\"f\",\"n\":{n}}}", crate::iso_utc(NOW - age_days * DAY))
    }

    #[test]
    fn rows_older_than_the_bound_go_and_rows_inside_it_stay() {
        let t = testkit::TempDir::new("tsd-prune");
        let p = crate::family_path(t.path(), "f");
        for (age, n) in [(40, 1), (31, 2), (29, 3), (1, 4)] {
            crate::append_line(&p, &row(age, n)).unwrap();
        }
        crate::append_line(&p, "{\"no_ts\":true}").unwrap();
        let s = prune_file(&p, NOW - 30 * DAY).unwrap();
        assert_eq!(s, Pruned { kept: 3, dropped: 2 });
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("\"n\":1") && !text.contains("\"n\":2"));
        assert!(text.contains("\"n\":3") && text.contains("\"n\":4") && text.contains("no_ts"));
        assert!(!tmp_path(&p).exists());
    }

    #[test]
    fn a_prune_with_nothing_old_leaves_the_file_untouched() {
        let t = testkit::TempDir::new("tsd-prune-noop");
        let p = crate::family_path(t.path(), "f");
        crate::append_line(&p, &row(1, 1)).unwrap();
        let before = std::fs::metadata(&p).unwrap().ino();
        assert_eq!(prune_file(&p, NOW - 30 * DAY).unwrap(), Pruned { kept: 1, dropped: 0 });
        assert_eq!(std::fs::metadata(&p).unwrap().ino(), before);
        assert_eq!(prune_file(&t.path().join("tsd/none.jsonl"), 0).unwrap(), Pruned::default());
    }

    #[test]
    fn appends_racing_a_prune_all_survive() {
        let t = testkit::TempDir::new("tsd-prune-race");
        let p = crate::family_path(t.path(), "f");
        for i in 0..200 {
            crate::append_line(&p, &row(40, i)).unwrap();
        }
        let writers: Vec<_> = (0..4u32)
            .map(|w| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for i in 0..300u32 {
                        crate::append_line(&p, &row(0, 10_000 + w * 1000 + i)).unwrap();
                    }
                })
            })
            .collect();
        for _ in 0..20 {
            prune_file(&p, NOW - 30 * DAY).unwrap();
        }
        for w in writers {
            w.join().unwrap();
        }
        prune_file(&p, NOW - 30 * DAY).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let fresh = text.lines().filter(|l| l.contains("\"n\":1")).count();
        assert_eq!(fresh, 1200, "a concurrent append was lost");
        assert!(text.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()));
    }

    #[test]
    fn only_family_files_are_pruned_and_markers_stay() {
        let t = testkit::TempDir::new("tsd-prune-root");
        let dir = t.path().join("tsd");
        std::fs::create_dir_all(dir.join(".ingested-runs")).unwrap();
        std::fs::write(dir.join(".ingested-runs/run-1"), "old marker\n").unwrap();
        std::fs::write(dir.join("f.jsonl.lock"), "").unwrap();
        std::fs::write(dir.join(".unit-run-checkpoint.json"), "{}").unwrap();
        crate::append_line(&crate::family_path(t.path(), "f"), &row(90, 1)).unwrap();
        let r = prune_root(t.path(), 30, &BTreeMap::new(), NOW);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].2, Ok(Pruned { kept: 0, dropped: 1 }));
        assert_eq!(std::fs::read_to_string(dir.join(".ingested-runs/run-1")).unwrap(), "old marker\n");
        assert!(dir.join("f.jsonl.lock").exists() && dir.join(".unit-run-checkpoint.json").exists());
    }

    #[test]
    fn a_bound_is_never_shorter_than_the_readers_window() {
        let o = parse_overrides("suite-timing=3 escape=2, other=2").unwrap();
        assert_eq!(keep_days("suite-timing", 30, &o), 30);
        assert_eq!(keep_days("escape", 30, &o), 7);
        assert_eq!(keep_days("other", 30, &o), 2);
        assert_eq!(keep_days("other", 0, &BTreeMap::new()), 1);
        assert_eq!(keep_days("suite-timing", 90, &BTreeMap::new()), 90);
        assert!(parse_overrides("nonsense").is_err());
        assert!(parse_overrides("../x=3").is_err());
        assert!(parse_overrides("a=-1").is_err());
    }
}
