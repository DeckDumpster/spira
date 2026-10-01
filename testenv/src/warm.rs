//! Warm slots (DESIGN.md §11, sp-govet): a scratch worktree per slot with a container that
//! was booted on it **before** a trial asked — claimed by at most one trial, torn down by
//! that trial, and replaced by a detached refill. No container ever runs suites for two
//! trials, so isolation holds by construction, not by a cleaning list.

use crate::fixture::Session;
use crate::runtime::{ContainerRuntime, ExecRequest};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Container-name prefix of every spare (never `spira-batch-`, so the batch orphan sweep's
/// age rule cannot take an idle spare).
pub const PREFIX: &str = "spira-warm-";
/// Written by the trial into the slot, read back from inside the spare: proof it mounts
/// this slot's checkout.
pub const NONCE_FILE: &str = "target/.testenv-warm-nonce";

/// (worktree, lock, spare record) of slot `i`, under the scratch root of `$SPIRA_RUN`
/// ([`crate::worktree::scratch_root`]).
pub fn paths(run: &Path, i: usize) -> (PathBuf, PathBuf, PathBuf) {
    let base = crate::worktree::scratch_root(run);
    (
        base.join(format!(".testenv-warm-{i}")),
        base.join(format!(".testenv-warm-{i}.lock")),
        base.join(format!(".testenv-warm-{i}.spare")),
    )
}

/// A booted, never-used container waiting in a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spare {
    pub name: String,
    pub tag: String,
    pub booted: u64,
}

impl Spare {
    pub fn render(&self) -> String {
        format!(
            "name={}\ntag={}\nbooted={}\n",
            self.name, self.tag, self.booted
        )
    }
    pub fn parse(text: &str) -> Option<Spare> {
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let name = get("name").filter(|n| n.starts_with(PREFIX))?;
        Some(Spare {
            name,
            tag: get("tag")?,
            booted: get("booted").and_then(|v| v.parse().ok()).unwrap_or(0),
        })
    }
}

pub fn read_spare(record: &Path) -> Option<Spare> {
    Spare::parse(&fs::read_to_string(record).ok()?)
}

/// Atomic: a reader sees the whole record or none.
pub fn write_spare(record: &Path, spare: &Spare) -> std::io::Result<()> {
    let tmp = record.with_extension(format!("spare.tmp.{}", std::process::id()));
    fs::write(&tmp, spare.render())?;
    fs::rename(&tmp, record)
}

/// Names every spare record under `run` currently holds (for the warm sweep).
pub fn recorded_names(run: &Path, slots: usize) -> Vec<String> {
    (0..slots.max(16))
        .filter_map(|i| read_spare(&paths(run, i).2))
        .map(|s| s.name)
        .collect()
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A fresh container name for slot `i`: never one an earlier container of this slot had.
pub fn fresh_name(i: usize) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{PREFIX}{i}-{}-{}-{n}", now_epoch(), std::process::id())
}

/// What a trial got from its slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A spare: running, current image, mounting this slot. The caller probes it.
    Spare(String),
    /// None usable; the reason is logged and the trial boots its own container.
    Cold(String),
}

fn owner_file(owner_dir: &Path, name: &str) -> PathBuf {
    owner_dir.join(format!("{name}.owner"))
}

fn discard(rt: &dyn ContainerRuntime, owner_dir: &Path, name: &str) {
    rt.purge(name);
    let _ = fs::remove_file(owner_file(owner_dir, name));
}

/// Claim slot `i`'s spare for this trial (the caller holds the slot lock and has checked
/// the tree out into `slot`). The owner file is rewritten to this process **before** the
/// record is removed, so a concurrent warm sweep never sees the spare both unrecorded and
/// ownerless; once the record is gone no other trial can claim it, whatever happens next.
pub fn claim(
    rt: &dyn ContainerRuntime,
    run: &Path,
    owner_dir: &Path,
    i: usize,
    slot: &Path,
    image_tag: &str,
    deadline: Option<Instant>,
) -> Claim {
    let record = paths(run, i).2;
    let Some(spare) = read_spare(&record) else {
        let _ = fs::remove_file(&record);
        return Claim::Cold("no spare booted in this slot yet".into());
    };
    let _ = fs::write(
        owner_file(owner_dir, &spare.name),
        format!("{}\n", std::process::id()),
    );
    let _ = fs::remove_file(&record);
    let cold = |why: String| {
        discard(rt, owner_dir, &spare.name);
        Claim::Cold(format!("spare {} discarded: {why}", spare.name))
    };
    if rt.inspect(&spare.name, "{{.State.Running}}").as_deref() != Some("true") {
        return cold("not running".into());
    }
    if spare.tag != image_tag {
        return cold(format!(
            "booted from image {}, the image is now {image_tag}",
            spare.tag
        ));
    }
    let nonce = format!(
        "{}-{}-{:?}",
        std::process::id(),
        now_epoch(),
        Instant::now()
    );
    let nonce_path = slot.join(NONCE_FILE);
    if let Some(d) = nonce_path.parent() {
        let _ = fs::create_dir_all(d);
    }
    if fs::write(&nonce_path, &nonce).is_err() {
        return cold("cannot write the mount nonce".into());
    }
    let mut req = ExecRequest::new(
        &spare.name,
        &[
            "cat",
            &format!("{}/{NONCE_FILE}", crate::fixture::WORKSPACE),
        ],
    );
    req.deadline = deadline;
    let seen = rt.exec(&req);
    let _ = fs::remove_file(&nonce_path);
    if !seen.ok() || seen.output.trim() != nonce {
        return cold("it does not mount this slot's checkout".into());
    }
    Claim::Spare(spare.name)
}

/// The warm half of the orphan sweep: a `spira-warm-*` container no record names whose owner
/// file names a dead pid — or, with no owner file, older than `min_age` — is purged.
#[allow(clippy::too_many_arguments)]
pub fn sweep(
    rt: &dyn ContainerRuntime,
    owner_dir: &Path,
    run: &Path,
    slots: usize,
    min_age: u64,
    started_at: &dyn Fn(&str) -> Option<u64>,
    log: &dyn Fn(&str),
) {
    let keep = recorded_names(run, slots);
    for name in rt.names_with_prefix(PREFIX) {
        if keep.contains(&name) {
            continue;
        }
        let owner = fs::read_to_string(owner_file(owner_dir, &name))
            .ok()
            .map(|s| s.trim().to_string());
        let orphan = match owner.as_deref() {
            Some(pid) if !pid.is_empty() => !Path::new("/proc").join(pid).exists(),
            _ => started_at(&name).is_some_and(|t| now_epoch().saturating_sub(t) >= min_age),
        };
        if orphan {
            discard(rt, owner_dir, &name);
            log(&format!("swept orphan warm container {name}"));
        }
    }
}

/// Reduce the COUNT of warm slots when the scratch root is short of real free space —
/// never throttle the job itself (law-reduce-the-count-never-throttle-the-job). With
/// `free_mib(root)` below `floor_mib`, drop recorded spares oldest-booted-first, each
/// under its own slot lock so a spare a trial is using right now (its lock held, or its
/// record already gone) is never touched; re-probes real free space after each drop,
/// since that is what dropping an idle container actually buys back, and stops as soon
/// as the floor is cleared or there is nothing idle left. Returns how many were dropped.
#[allow(clippy::too_many_arguments)]
pub fn shed(
    rt: &dyn ContainerRuntime,
    owner_dir: &Path,
    run: &Path,
    slots: usize,
    floor_mib: u64,
    root: &Path,
    free_mib: &dyn Fn(&Path) -> Option<u64>,
    log: &dyn Fn(&str),
) -> usize {
    let Some(mut free) = free_mib(root) else {
        return 0;
    };
    if free >= floor_mib {
        return 0;
    }
    let mut oldest: Vec<(usize, Spare)> = (0..slots)
        .filter_map(|i| read_spare(&paths(run, i).2).map(|sp| (i, sp)))
        .collect();
    oldest.sort_by_key(|(_, sp)| sp.booted);
    let mut dropped = 0;
    for (i, spare) in oldest {
        if free >= floor_mib {
            break;
        }
        let (_, lock_path, record) = paths(run, i);
        let Some(_lock) = crate::worktree::try_lock(&lock_path) else {
            continue; // busy: a trial (or a refill) holds this slot right now — never dropped
        };
        // Re-read under the lock: a refill may have replaced or cleared the record between
        // our scan and this lock, and we must discard only the spare we actually recorded.
        if read_spare(&record).as_ref().map(|sp| &sp.name) != Some(&spare.name) {
            continue;
        }
        let _ = fs::remove_file(&record);
        discard(rt, owner_dir, &spare.name);
        log(&format!(
            "shed idle warm slot {i} ({}): free space below the {floor_mib} MiB floor (SPIRA_TMPFS_SHED_FREE_MIB)",
            spare.name
        ));
        dropped += 1;
        free = free_mib(root).unwrap_or(free);
    }
    dropped
}

/// Boot slot `i`'s spare (the refill; the caller holds the slot lock). Does nothing when a
/// live, current spare is already recorded. `Err` names why no spare was recorded.
pub fn boot(
    rt: &dyn ContainerRuntime,
    run: &Path,
    owner_dir: &Path,
    i: usize,
    image_tag: &str,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let (slot, _, record) = paths(run, i);
    if let Some(sp) = read_spare(&record) {
        if sp.tag == image_tag
            && rt.inspect(&sp.name, "{{.State.Running}}").as_deref() == Some("true")
        {
            return Ok(None);
        }
        let _ = fs::remove_file(&record);
        discard(rt, owner_dir, &sp.name);
    }
    if !slot.join(".git").exists() {
        return Err(format!("slot {} has no checkout yet", slot.display()));
    }
    let name = fresh_name(i);
    let mut session = Session::new(rt, "warm", "aeon");
    session.name = name.clone();
    session.setup_deadline = Some(Instant::now() + timeout);
    if let Err(f) = session.up(&slot) {
        discard(rt, owner_dir, &name);
        return Err(format!("boot of {name} failed: {}", f.message()));
    }
    let spare = Spare {
        name: name.clone(),
        tag: image_tag.to_string(),
        booted: now_epoch(),
    };
    if let Err(e) = write_spare(&record, &spare) {
        discard(rt, owner_dir, &name);
        return Err(format!("cannot record {name}: {e}"));
    }
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::fake::FakeRuntime;
    use crate::runtime::ExecOutcome;
    use std::sync::Arc;

    fn world(tag: &str) -> (testkit::TempDir, PathBuf, PathBuf) {
        let root = testkit::TempDir::new(&format!("testenv-warm-{tag}"));
        let run = root.join("run");
        let owner = root.join("owner");
        fs::create_dir_all(run.join("worktree/.testenv-warm-0/.git")).unwrap();
        fs::create_dir_all(&owner).unwrap();
        (root, run, owner)
    }

    /// The fake container "mounts" the slot: `cat` of the nonce reads the host file.
    fn mounts(rt: &FakeRuntime, slot: PathBuf) {
        rt.on(
            |r| r.argv.first().map(String::as_str) == Some("cat"),
            move |_| ExecOutcome {
                rc: 0,
                output: fs::read_to_string(slot.join(NONCE_FILE)).unwrap_or_default(),
            },
        );
    }

    #[test]
    fn the_record_round_trips_and_refuses_a_foreign_name() {
        let s = Spare {
            name: "spira-warm-0-1-2".into(),
            tag: "t1".into(),
            booted: 9,
        };
        assert_eq!(Spare::parse(&s.render()), Some(s));
        assert_eq!(Spare::parse("name=spira-batch-x\ntag=t\n"), None);
        assert_eq!(Spare::parse("name=spira-warm-0\n"), None, "no tag");
    }

    #[test]
    fn a_spare_is_claimed_once_and_its_record_is_gone_before_use() {
        let (_root, run, owner) = world("once");
        let rt = FakeRuntime::new();
        let (slot, _, record) = paths(&run, 0);
        mounts(&rt, slot.clone());
        assert!(boot(&rt, &run, &owner, 0, "t1", Duration::from_secs(5))
            .unwrap()
            .is_some());
        let name = read_spare(&record).unwrap().name;
        assert!(name.starts_with("spira-warm-0-"));
        assert_eq!(
            claim(&rt, &run, &owner, 0, &slot, "t1", None),
            Claim::Spare(name.clone())
        );
        assert!(!record.exists(), "claimed: no second trial can take it");
        assert_eq!(
            fs::read_to_string(owner.join(format!("{name}.owner")))
                .unwrap()
                .trim(),
            std::process::id().to_string()
        );
        assert!(matches!(
            claim(&rt, &run, &owner, 0, &slot, "t1", None),
            Claim::Cold(_)
        ));
    }

    #[test]
    fn a_stale_spare_is_discarded_not_used() {
        let (_root, run, owner) = world("stale");
        let (slot, _, record) = paths(&run, 0);
        // image changed since the spare booted
        let rt = FakeRuntime::new();
        mounts(&rt, slot.clone());
        boot(&rt, &run, &owner, 0, "old", Duration::from_secs(5)).unwrap();
        let name = read_spare(&record).unwrap().name;
        assert!(
            matches!(claim(&rt, &run, &owner, 0, &slot, "new", None), Claim::Cold(w) if w.contains("image"))
        );
        assert!(rt.purged.lock().unwrap().contains(&name));
        // not running
        let rt = FakeRuntime::new();
        mounts(&rt, slot.clone());
        boot(&rt, &run, &owner, 0, "t", Duration::from_secs(5)).unwrap();
        rt.running_answers(&[Some("false")]);
        assert!(
            matches!(claim(&rt, &run, &owner, 0, &slot, "t", None), Claim::Cold(w) if w.contains("not running"))
        );
        // mounts some other checkout: the nonce does not read back
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.first().map(String::as_str) == Some("cat"),
            |_| ExecOutcome {
                rc: 0,
                output: "someone-else".into(),
            },
        );
        boot(&rt, &run, &owner, 0, "t", Duration::from_secs(5)).unwrap();
        assert!(
            matches!(claim(&rt, &run, &owner, 0, &slot, "t", None), Claim::Cold(w) if w.contains("mount"))
        );
        assert!(
            !slot.join(NONCE_FILE).exists(),
            "the nonce never outlives the claim"
        );
    }

    #[test]
    fn a_failed_boot_records_nothing_and_leaves_no_container() {
        let (_root, run, owner) = world("fail");
        let rt = FakeRuntime::new();
        rt.testenv_rc.lock().unwrap().insert("probe".into(), 1);
        assert!(boot(&rt, &run, &owner, 0, "t", Duration::from_secs(5)).is_err());
        assert!(read_spare(&paths(&run, 0).2).is_none());
        assert!(rt.names_with_prefix(PREFIX).is_empty());
        // a slot with no checkout yet is not booted at all
        let rt = FakeRuntime::new();
        assert!(boot(&rt, &run, &owner, 1, "t", Duration::from_secs(5)).is_err());
        assert!(rt.testenv_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn the_warm_sweep_keeps_recorded_and_owned_spares_and_takes_orphans() {
        let (_root, run, owner) = world("sweep");
        let rt = FakeRuntime::new();
        boot(&rt, &run, &owner, 0, "t", Duration::from_secs(5)).unwrap();
        let recorded = read_spare(&paths(&run, 0).2).unwrap().name;
        for n in ["spira-warm-5-1-1", "spira-warm-6-1-1", "spira-warm-7-1-1"] {
            rt.containers.lock().unwrap().push(n.into());
        }
        // 5: owned by a live pid (us); 6: owner pid gone; 7: no owner file, old
        fs::write(
            owner.join("spira-warm-5-1-1.owner"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        fs::write(owner.join("spira-warm-6-1-1.owner"), "999999999\n").unwrap();
        let logs = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = logs.clone();
        sweep(&rt, &owner, &run, 3, 3600, &|_| Some(0), &move |m| {
            l2.lock().unwrap().push(m.into())
        });
        let purged = rt.purged.lock().unwrap().clone();
        assert!(!purged.contains(&recorded));
        assert!(!purged.contains(&"spira-warm-5-1-1".to_string()));
        assert!(purged.contains(&"spira-warm-6-1-1".to_string()));
        assert!(purged.contains(&"spira-warm-7-1-1".to_string()));
        assert_eq!(logs.lock().unwrap().len(), 2);
    }

    /// sp-s8v5r, positive control: a fake free-space reading below the floor drops the
    /// oldest-booted idle spare, and stops as soon as the (fake) drop clears the floor.
    #[test]
    fn shed_drops_the_oldest_idle_spare_first_until_the_floor_clears() {
        let (_root, run, owner) = world("shed");
        let base = crate::worktree::scratch_root(&run);
        fs::create_dir_all(&base).unwrap();
        let rt = FakeRuntime::new();
        for (i, booted) in [(0usize, 100u64), (1, 200), (2, 300)] {
            let name = format!("spira-warm-{i}-x");
            rt.containers.lock().unwrap().push(name.clone());
            write_spare(&paths(&run, i).2, &Spare { name, tag: "t".into(), booted }).unwrap();
        }
        // Below the 15 MiB floor until the first drop frees real space, as a real
        // container's teardown would — then above it, so exactly one spare goes.
        let calls = std::cell::Cell::new(0u64);
        let free = |_: &Path| {
            let n = calls.get();
            calls.set(n + 1);
            Some(if n == 0 { 10 } else { 20 })
        };
        let logs = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = logs.clone();
        let dropped = shed(&rt, &owner, &run, 3, 15, &base, &free, &move |m| {
            l2.lock().unwrap().push(m.into())
        });
        assert_eq!(dropped, 1);
        assert!(
            rt.purged.lock().unwrap().contains(&"spira-warm-0-x".to_string()),
            "the oldest-booted spare goes first"
        );
        assert!(read_spare(&paths(&run, 0).2).is_none());
        assert!(read_spare(&paths(&run, 1).2).is_some(), "slot 1 untouched");
        assert!(read_spare(&paths(&run, 2).2).is_some(), "slot 2 untouched");
        assert_eq!(logs.lock().unwrap().len(), 1);
    }

    /// sp-s8v5r, positive control: a busy slot (its flock held) is never dropped, even
    /// though it is the oldest and free space stays short the whole time.
    #[test]
    fn shed_never_drops_a_slot_whose_lock_is_held() {
        let (_root, run, owner) = world("shed-busy");
        let base = crate::worktree::scratch_root(&run);
        fs::create_dir_all(&base).unwrap();
        let rt = FakeRuntime::new();
        for (i, booted) in [(0usize, 100u64), (1, 200)] {
            let name = format!("spira-warm-{i}-x");
            rt.containers.lock().unwrap().push(name.clone());
            write_spare(&paths(&run, i).2, &Spare { name, tag: "t".into(), booted }).unwrap();
        }
        let (_, lock0, _) = paths(&run, 0);
        let _held = crate::worktree::try_lock(&lock0).unwrap();
        let dropped = shed(&rt, &owner, &run, 2, 1_000_000, &base, &|_| Some(0), &|_| {});
        assert_eq!(dropped, 1, "slot 0 is skipped; the idle slot 1 goes instead");
        assert!(!rt.purged.lock().unwrap().contains(&"spira-warm-0-x".to_string()), "never a slot in use");
        assert!(read_spare(&paths(&run, 0).2).is_some(), "the busy slot's record is untouched");
        assert!(rt.purged.lock().unwrap().contains(&"spira-warm-1-x".to_string()));
    }

    #[test]
    fn shed_does_nothing_when_free_space_already_clears_the_floor() {
        let (_root, run, owner) = world("shed-ok");
        let base = crate::worktree::scratch_root(&run);
        fs::create_dir_all(&base).unwrap();
        let rt = FakeRuntime::new();
        rt.containers.lock().unwrap().push("spira-warm-0-x".into());
        write_spare(
            &paths(&run, 0).2,
            &Spare { name: "spira-warm-0-x".into(), tag: "t".into(), booted: 1 },
        )
        .unwrap();
        let dropped = shed(&rt, &owner, &run, 1, 10, &base, &|_| Some(50), &|_| {});
        assert_eq!(dropped, 0);
        assert!(read_spare(&paths(&run, 0).2).is_some());
    }
}
