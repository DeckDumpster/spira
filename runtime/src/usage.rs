//! What each dwarf uses, read from its unit's cgroup: cumulative CPU, read and write bytes per
//! disk, memory now and at peak, and the size of its worktree and build directory. Read-only:
//! it reads `/sys/fs/cgroup`, `/proc` and `/sys/dev/block`, and writes nothing.
//!
//! A dwarf is a transient unit (`spira-run-sentinel_2dcall-<rune>_2d<at>_2d<n>.service`, or a
//! `spira-dwarf-*` unit started by name) in a slice of the user manager spira-runtime runs
//! under. Its bead and build directory are read from the environment the dwarf exports to its
//! children, and only the keys in [`ENV_KEYS`] are kept: the rest of an environ is never copied
//! out of the read buffer. The rest of its identity is on disk where the dwarf put it: the rune
//! in its unit name, its name in `<run>/dwarf-<rune name>-<bead>.name`, its worktree at
//! `<run>/worktree/<bead>`, and the repo that worktree belongs to (the checkout its `.git`
//! file points into, by directory name).
//!
//! A hand build (a Concierge subagent's cargo in `<run>/worktree/<bead>-hand`) is not a unit.
//! Its row is the cgroup of the processes whose working directory is a hand worktree: the
//! session scope they run in, so the row is that whole scope's load.
//!
//! Disks are mapped at read time: an `io.stat` line names a whole disk by `MAJ:MIN`; the mount
//! of that disk's filesystem root (from mountinfo, the partition walked up to its disk through
//! sysfs) names it, and the mount's basename is its role (`/srv/ssd-b` is `ssd-b`; `/` is `root`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::units;

/// The environment keys a dwarf exports to its children that say who it is and where it builds.
pub const ENV_KEYS: [&str; 2] = ["BEAD_ID", "CARGO_TARGET_DIR"];

/// The series `watch.dwarf_usage` pushes and `dwarves.sh usage bead` reads back, one name each.
pub mod metric {
    pub const CPU_SECONDS: &str = "spira_dwarf_cpu_seconds";
    pub const IO_READ_BYTES: &str = "spira_dwarf_io_read_bytes";
    pub const IO_WRITE_BYTES: &str = "spira_dwarf_io_write_bytes";
    pub const MEMORY_BYTES: &str = "spira_dwarf_memory_bytes";
    pub const MEMORY_PEAK_BYTES: &str = "spira_dwarf_memory_peak_bytes";
    pub const WORKTREE_BYTES: &str = "spira_dwarf_worktree_bytes";
    pub const BUILD_DIR_BYTES: &str = "spira_dwarf_build_dir_bytes";
    pub const WALL_SECONDS: &str = "spira_dwarf_wall_seconds";
    /// Every name above shares this prefix, so one selector reads them all.
    pub const PREFIX: &str = "spira_dwarf_";
}

/// The suffix a hand-build worktree carries under `<run>/worktree/`.
pub const HAND_SUFFIX: &str = "-hand";

/// Where the sampler reads. [`Roots::live`] is the host; a test points each at a fixture tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    /// The cgroup2 mount, `/sys/fs/cgroup`.
    pub cgroup: PathBuf,
    /// `/proc`.
    pub proc_dir: PathBuf,
    /// `/sys`.
    pub sys: PathBuf,
    /// The harness run directory; hand worktrees are `<run_dir>/worktree/*-hand`.
    pub run_dir: PathBuf,
}

impl Roots {
    pub fn live(run_dir: &Path) -> Roots {
        Roots { cgroup: "/sys/fs/cgroup".into(), proc_dir: "/proc".into(), sys: "/sys".into(), run_dir: run_dir.to_path_buf() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Dwarf,
    Hand,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Dwarf => "dwarf",
            Kind::Hand => "hand",
        }
    }
}

/// One disk's cumulative bytes for one cgroup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIo {
    /// The kernel's name for the whole disk (`sdc`).
    pub device: String,
    /// The mount of the disk's filesystem root (`/srv/ssd-b`), empty when none is mounted.
    pub mount: String,
    /// The mount's basename (`ssd-b`); `root` for `/`; the device name when nothing is mounted.
    pub role: String,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// One dwarf's (or one hand scope's) usage at one instant. Counters are cumulative over the
/// cgroup's life.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub kind: Kind,
    /// The unit (or scope) the cgroup is.
    pub unit: String,
    /// The cgroup's path under the cgroup2 mount.
    pub cgroup: String,
    pub rune: String,
    pub name: String,
    /// For a hand row, every hand bead its scope builds, comma-joined.
    pub bead: String,
    pub repo: String,
    /// The unit's start in ms since the epoch, from a `sentinel-call` unit key; `None` otherwise.
    pub started_ms: Option<u64>,
    pub cpu_usec: u64,
    pub io: Vec<DeviceIo>,
    pub memory_current: u64,
    pub memory_peak: u64,
    pub worktree: String,
    pub worktree_bytes: Option<u64>,
    pub build_dir: String,
    pub build_dir_bytes: Option<u64>,
}

impl Sample {
    pub fn cpu_seconds(&self) -> f64 {
        self.cpu_usec as f64 / 1e6
    }
}

/// A whole disk the mounts name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disk {
    pub device: String,
    pub mount: String,
    pub role: String,
}

/// `usage_usec` from a `cpu.stat`.
pub fn cpu_usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat.lines().find_map(|l| l.strip_prefix("usage_usec ")).and_then(|v| v.trim().parse().ok())
}

/// Each `io.stat` line as (`MAJ:MIN`, rbytes, wbytes).
pub fn io_stat(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let dev = f.next()?;
            let (mut r, mut w) = (0, 0);
            for kv in f {
                match kv.split_once('=') {
                    Some(("rbytes", v)) => r = v.parse().unwrap_or(0),
                    Some(("wbytes", v)) => w = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Some((dev.to_string(), r, w))
        })
        .collect()
}

/// A one-number cgroup file (`memory.current`, `memory.peak`).
pub fn scalar(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn basename(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The whole disk `MAJ:MIN` belongs to, as (its `MAJ:MIN`, its kernel name), from sysfs: a
/// partition is walked up to its disk; a device-mapper device is its own disk.
pub fn whole_disk(sys: &Path, majmin: &str) -> Option<(String, String)> {
    let dev = std::fs::canonicalize(sys.join("dev/block").join(majmin)).ok()?;
    let disk = if dev.join("partition").is_file() { dev.parent()?.to_path_buf() } else { dev };
    let mm = read(&disk.join("dev")).trim().to_string();
    (!mm.is_empty()).then(|| (mm, basename(&disk)))
}

/// Every whole disk a filesystem root is mounted from, by the disk's `MAJ:MIN`, named by that
/// mount. `whole` maps a mount's `MAJ:MIN` to its disk ([`whole_disk`]).
pub fn disks(mountinfo: &str, whole: impl Fn(&str) -> Option<(String, String)>) -> BTreeMap<String, Disk> {
    let mut out = BTreeMap::new();
    for line in mountinfo.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(mm), Some(&"/"), Some(mount)) = (f.get(2), f.get(3), f.get(4)) else { continue };
        let Some((disk, device)) = whole(mm) else { continue };
        let role = if *mount == "/" { "root".to_string() } else { basename(Path::new(mount)) };
        out.entry(disk).or_insert(Disk { device, mount: mount.to_string(), role });
    }
    out
}

/// The [`ENV_KEYS`] an environ (NUL-separated `K=V`) carries; every other key is dropped unread.
pub fn env_of(environ: &[u8]) -> BTreeMap<String, String> {
    environ
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let at = kv.iter().position(|b| *b == b'=')?;
            let key = std::str::from_utf8(&kv[..at]).ok()?;
            if !ENV_KEYS.contains(&key) || at + 1 == kv.len() {
                return None;
            }
            Some((key.to_string(), String::from_utf8_lossy(&kv[at + 1..]).into_owned()))
        })
        .collect()
}

/// Bytes allocated on disk under `path` (what `du` counts), symlinks not followed; 0 for a
/// path that does not exist.
pub fn dir_bytes(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        total += m.blocks() * 512;
        if m.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        }
    }
    total
}

/// The cgroup a `/proc/<pid>/cgroup` names (the v2 `0::` line), without its leading `/`.
fn cgroup_of(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("0::")).map(|p| p.trim().trim_start_matches('/').to_string())
}

/// The user manager's cgroup spira-runtime (or the CLI) runs under: its own cgroup up to the
/// `user@<uid>.service` component, else that cgroup's parent.
fn manager(roots: &Roots) -> Option<PathBuf> {
    let own = cgroup_of(&read(&roots.proc_dir.join("self/cgroup")))?;
    let parts: Vec<&str> = own.split('/').collect();
    let rel = match parts.iter().position(|c| c.starts_with("user@") && c.ends_with(".service")) {
        Some(i) => parts[..=i].join("/"),
        None => parts[..parts.len().saturating_sub(1)].join("/"),
    };
    Some(roots.cgroup.join(rel))
}

/// Every pid in `dir` and the cgroups below it.
fn pids(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = read(&dir.join("cgroup.procs")).split_whitespace().map(str::to_string).collect();
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        for s in subs {
            out.extend(pids(&s));
        }
    }
    out
}

/// The cgroup's own counters, its disks named through `named`.
fn counters(dir: &Path, named: &dyn Fn(&str) -> (String, String, String), s: &mut Sample) {
    s.cpu_usec = cpu_usage_usec(&read(&dir.join("cpu.stat"))).unwrap_or(0);
    s.memory_current = scalar(&read(&dir.join("memory.current"))).unwrap_or(0);
    s.memory_peak = scalar(&read(&dir.join("memory.peak"))).unwrap_or(0);
    s.io = io_stat(&read(&dir.join("io.stat")))
        .into_iter()
        .filter(|(_, r, w)| r + w > 0)
        .map(|(mm, r, w)| {
            let (device, mount, role) = named(&mm);
            DeviceIo { device, mount, role, read_bytes: r, write_bytes: w }
        })
        .collect();
    s.io.sort_by(|a, b| a.device.cmp(&b.device));
}

/// The name the dwarf on `bead` wrote beside its pidfile (`<run>/dwarf-<rune name>-<bead>.name`).
fn dwarf_name(run_dir: &Path, bead: &str) -> String {
    if bead.is_empty() {
        return String::new();
    }
    let tail = format!("-{bead}.name");
    let Ok(rd) = std::fs::read_dir(run_dir) else { return String::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| basename(p).starts_with("dwarf-") && basename(p).ends_with(&tail)).collect();
    files.sort();
    files.first().map(|p| read(p).trim().to_string()).unwrap_or_default()
}

/// The checkout a worktree belongs to, by directory name: its `.git` file reads
/// `gitdir: <checkout>/.git/worktrees/<name>`.
fn repo_of(worktree: &Path) -> String {
    let text = read(&worktree.join(".git"));
    let Some(gitdir) = text.trim().strip_prefix("gitdir:").map(str::trim) else { return String::new() };
    gitdir.split_once("/.git/").map(|(checkout, _)| basename(Path::new(checkout))).unwrap_or_default()
}

/// The bytes under each comma-separated path that exists; `None` when none does.
fn sized(paths: &str) -> Option<u64> {
    let there: Vec<&Path> = paths.split(',').filter(|p| !p.is_empty()).map(Path::new).filter(|p| p.exists()).collect();
    (!there.is_empty()).then(|| there.iter().map(|p| dir_bytes(p)).sum())
}

/// A hand scope's beads, worktrees and build directories, as its processes name them.
type HandRow = (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>);

/// Every live dwarf and every hand scope, sampled now.
pub fn sample(roots: &Roots) -> Vec<Sample> {
    let Some(manager) = manager(roots) else { return vec![] };
    let mounted = disks(&read(&roots.proc_dir.join("self/mountinfo")), |mm| whole_disk(&roots.sys, mm));
    let named = |mm: &str| -> (String, String, String) {
        match mounted.get(mm) {
            Some(d) => (d.device.clone(), d.mount.clone(), d.role.clone()),
            None => {
                let device = whole_disk(&roots.sys, mm).map_or(mm.to_string(), |(_, n)| n);
                (device.clone(), String::new(), device)
            }
        }
    };
    let rel = |p: &Path| p.strip_prefix(&roots.cgroup).map_or_else(|_| p.display().to_string(), |r| r.display().to_string());

    let mut dwarves = vec![];
    let mut slices: Vec<PathBuf> = std::fs::read_dir(&manager).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && basename(p).ends_with(".slice")).collect()).unwrap_or_default();
    slices.sort();
    for slice in slices {
        let Ok(rd) = std::fs::read_dir(&slice) else { continue };
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && units::is_dwarf_unit(&basename(p))).collect();
        found.sort();
        for dir in found {
            let unit = basename(&dir);
            let mut env = BTreeMap::new();
            for pid in pids(&dir) {
                for (k, v) in env_of(&std::fs::read(roots.proc_dir.join(&pid).join("environ")).unwrap_or_default()) {
                    env.entry(k).or_insert(v);
                }
            }
            let get = |k: &str| env.get(k).cloned().unwrap_or_default();
            let bead = get("BEAD_ID");
            let worktree = if bead.is_empty() { String::new() } else { roots.run_dir.join("worktree").join(&bead).display().to_string() };
            let mut build_dir = get("CARGO_TARGET_DIR");
            if build_dir.is_empty() && !worktree.is_empty() && Path::new(&worktree).join("target").is_dir() {
                build_dir = Path::new(&worktree).join("target").display().to_string();
            }
            let started_ms = units::parse_unit_name(&unit).filter(|(t, _)| t == units::DWARF_TASK).and_then(|(_, key)| key.rsplit('-').nth(1).and_then(|at| at.parse().ok()));
            let mut s = Sample {
                kind: Kind::Dwarf,
                cgroup: rel(&dir),
                rune: units::dwarf_rune(&unit).unwrap_or_default(),
                name: dwarf_name(&roots.run_dir, &bead),
                repo: repo_of(Path::new(&worktree)),
                bead,
                started_ms,
                worktree_bytes: sized(&worktree),
                build_dir_bytes: sized(&build_dir),
                worktree,
                build_dir,
                unit,
                ..Sample::default()
            };
            counters(&dir, &named, &mut s);
            dwarves.push(s);
        }
    }

    // Hand builds: processes whose working directory is a `<run>/worktree/*-hand` tree, by the
    // cgroup they run in.
    let hand_root = roots.run_dir.join("worktree");
    let mut hands: BTreeMap<String, HandRow> = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(&roots.proc_dir) {
        for e in rd.flatten() {
            let pid = e.file_name().to_string_lossy().into_owned();
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) else { continue };
            let Ok(under) = cwd.strip_prefix(&hand_root) else { continue };
            let Some(top) = under.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()) else { continue };
            let Some(bead) = top.strip_suffix(HAND_SUFFIX) else { continue };
            let Some(cg) = cgroup_of(&read(&e.path().join("cgroup"))) else { continue };
            if dwarves.iter().any(|d| cg == d.cgroup || cg.starts_with(&format!("{}/", d.cgroup))) {
                continue;
            }
            let row = hands.entry(cg).or_default();
            row.0.insert(bead.to_string());
            row.1.insert(hand_root.join(&top).display().to_string());
            let target = env_of(&std::fs::read(e.path().join("environ")).unwrap_or_default()).remove("CARGO_TARGET_DIR");
            row.2.extend(target);
        }
    }
    let mut out = dwarves;
    for (cg, (beads, trees, mut targets)) in hands {
        if targets.is_empty() {
            targets.extend(trees.iter().map(|t| Path::new(t).join("target")).filter(|t| t.is_dir()).map(|t| t.display().to_string()));
        }
        let dir = roots.cgroup.join(&cg);
        let join = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(",");
        let (worktree, build_dir) = (join(&trees), join(&targets));
        let mut s = Sample {
            kind: Kind::Hand,
            unit: basename(&dir),
            cgroup: cg,
            rune: "hand".into(),
            name: "hand".into(),
            bead: join(&beads),
            worktree_bytes: sized(&worktree),
            build_dir_bytes: sized(&build_dir),
            worktree,
            build_dir,
            ..Sample::default()
        };
        counters(&dir, &named, &mut s);
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests;
