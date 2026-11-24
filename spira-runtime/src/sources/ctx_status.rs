//! The `ctx.status` source: keeps one status file per live status line session current, so the
//! refresh prints it without measuring anything (`ctx-meter` DESIGN.md). Submits nothing; its
//! work is the writes.
//!
//! Wakes on inotify over the session records and the archivist's state files, never on a timer,
//! and recomputes a session only when its record or its archivist state was written. Every
//! session is recomputed once at start, because a reboot empties the memory-backed directory.
//! Dormant unless `SPIRA_RUNTIME_COCKPIT_TIER_WRITE` is true, so a shadow instance never writes
//! the files the live one's refresh reads.

use std::path::PathBuf;
use std::sync::Arc;

use ctx_meter::status::{self, Wrote};
use runtime::host::Submit;
use runtime::sources::{on_pool, Running, Source};
use tokio::sync::mpsc;

use super::watch::{self, Changes, Watch};
use crate::tasks::g::cockpit::WRITE_KEY;

pub const NAME: &str = "ctx.status";

pub struct CtxStatus {
    cfg: ctx_meter::Config,
    write: bool,
}

impl CtxStatus {
    /// Pinned from `cfg`; `status_root` is the user runtime directory's records directory.
    pub fn configured(cfg: &dyn Fn(&str) -> Result<String, String>, status_root: PathBuf) -> Result<CtxStatus, String> {
        let mut meter = ctx_meter::Config::from_lookup(cfg)?;
        meter.status = status_root;
        let write = matches!(cfg(WRITE_KEY)?.as_str(), "1" | "true");
        Ok(CtxStatus { cfg: meter, write })
    }
}

fn now() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

fn archivist_dir(cfg: &ctx_meter::Config) -> PathBuf {
    cfg.run.join("archivist")
}

/// Every session recomputed, then the records and statuses past idle removed.
pub fn start(cfg: &ctx_meter::Config, now: i64) -> Vec<(String, Wrote)> {
    let _guard = status::lock();
    let done = status::sessions(&cfg.status).into_iter().filter_map(|sid| status::refresh_session(cfg, now, &sid).map(|w| (sid, w))).collect();
    status::prune(&cfg.status, now, cfg.idle);
    done
}

/// The sessions `changes` concerns, recomputed once each: a written record or `<sid>.state`
/// names one, `sweep.state` all of them. Answers what each write did.
pub fn apply(cfg: &ctx_meter::Config, now: i64, changes: &Changes) -> Vec<(String, Wrote)> {
    let (records, arc) = (status::sessions_dir(&cfg.status), archivist_dir(cfg));
    let mut sids: Vec<String> = vec![];
    for (dir, names) in changes {
        for name in names {
            if dir == &records {
                sids.extend(name.strip_suffix(".json").map(String::from));
            } else if dir == &arc && name == "sweep.state" {
                sids.extend(status::sessions(&cfg.status));
            } else if dir == &arc {
                sids.extend(name.strip_suffix(".state").map(String::from));
            }
        }
    }
    sids.sort();
    sids.dedup();
    let _guard = status::lock();
    let done = sids.into_iter().filter_map(|sid| status::refresh_session(cfg, now, &sid).map(|w| (sid, w))).collect();
    status::prune(&cfg.status, now, cfg.idle);
    done
}

async fn say(what: String) {
    let _ = on_pool(NAME, Arc::new(move || eprintln!("spira-runtime: source {NAME}: {what}"))).await;
}

impl Source for CtxStatus {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(async move {
            if !self.write || self.cfg.status.as_os_str().is_empty() || tx.is_closed() {
                return;
            }
            let cfg = Arc::new(self.cfg);
            match watch::platform() {
                Ok(w) => serve(cfg, w).await,
                Err(e) => say(e).await,
            }
        })
    }
}

/// Creates the directories, watches the records and the archivist's state, recomputes every
/// session once, then applies each change heard until the watch fails or the pool refuses.
async fn serve(cfg: Arc<ctx_meter::Config>, mut watch: Box<dyn Watch>) {
    let dirs = [status::sessions_dir(&cfg.status), status::status_dir(&cfg.status), archivist_dir(&cfg)];
    let made = dirs.clone();
    let _ = on_pool(NAME, Arc::new(move || made.iter().for_each(|d| drop(std::fs::create_dir_all(d))))).await;
    for dir in [&dirs[0], &dirs[2]] {
        if let Err(e) = watch.add(dir) {
            return say(e).await;
        }
    }
    let boot = cfg.clone();
    let _ = on_pool(NAME, Arc::new(move || start(&boot, now()))).await;
    loop {
        let changes = match watch.changed().await {
            Ok(c) => c,
            Err(e) => return say(e).await,
        };
        let (cfg, changes) = (cfg.clone(), Arc::new(changes));
        if on_pool(NAME, Arc::new(move || apply(&cfg, now(), &changes))).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::watch::Changed;
    use std::path::Path;
    use ctx_meter::status::{put_record, read_status, Record};
    use std::os::unix::fs::MetadataExt;

    struct Rig {
        dir: testkit::TempDir,
    }

    fn row(n: usize, total: i64) -> String {
        format!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"m{n}\",\"usage\":{{\"input_tokens\":{total},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n")
    }

    impl Rig {
        fn new() -> Rig {
            let rig = Rig { dir: testkit::TempDir::new("ctx-status") };
            std::fs::create_dir_all(rig.dir.path().join("run/archivist")).unwrap();
            std::fs::write(rig.transcript(), row(0, 1000)).unwrap();
            rig.record();
            rig
        }
        fn cfg(&self) -> ctx_meter::Config {
            ctx_meter::Config { warn: 200_000, high: 400_000, limit: 1_000_000, projects: self.dir.path().join("projects"), run: self.dir.path().join("run"), status: self.dir.path().join("rt"), idle: 1800, hhmm: |_| String::new() }
        }
        fn transcript(&self) -> PathBuf {
            self.dir.path().join("s1.jsonl")
        }
        fn record(&self) {
            let len = std::fs::metadata(self.transcript()).unwrap().len();
            put_record(&status::sessions_dir(&self.cfg().status).join("s1.json"), &Record { session: "s1".into(), transcript: self.transcript().display().to_string(), cwd: String::new(), len });
        }
        fn ino(&self) -> u64 {
            std::fs::metadata(status::status_dir(&self.cfg().status).join("s1.json")).unwrap().ino()
        }
        fn heard(&self, dir: PathBuf, name: &str) -> Changes {
            vec![(dir, vec![name.to_string()])]
        }
        fn applied(&self, dir: PathBuf, name: &str) -> Vec<Wrote> {
            apply(&self.cfg(), now(), &self.heard(dir, name)).into_iter().map(|(_, w)| w).collect()
        }
    }

    #[test_id(tmux14p)]
    #[test]
    fn a_changed_record_rewrites_its_status_once_and_an_unchanged_one_does_not() {
        let rig = Rig::new();
        let records = status::sessions_dir(&rig.cfg().status);
        assert_eq!(rig.applied(records.clone(), "s1.json"), [Wrote::Rewrote], "the first status is a write");
        let first = rig.ino();
        assert_eq!(rig.applied(records.clone(), "s1.json"), [Wrote::Touched]);
        assert_eq!(rig.ino(), first, "an unchanged record leaves the status file alone");

        std::fs::write(rig.transcript(), row(0, 1000) + &row(1, 2000)).unwrap();
        rig.record();
        assert_eq!(rig.applied(records, "s1.json"), [Wrote::Rewrote]);
        assert_ne!(rig.ino(), first);
        assert_eq!(read_status(&status::status_dir(&rig.cfg().status).join("s1.json")).unwrap().turns, 2);
    }

    #[test_id(tex8thq)]
    #[test]
    fn a_session_state_write_rewrites_that_sessions_status_once() {
        let rig = Rig::new();
        let arc = archivist_dir(&rig.cfg());
        rig.applied(status::sessions_dir(&rig.cfg().status), "s1.json");
        let first = rig.ino();
        assert_eq!(rig.applied(arc.clone(), "s1.state"), [Wrote::Touched], "a state file that says nothing new changes nothing");
        std::fs::write(arc.join("s1.state"), "state=sweeping\n").unwrap();
        assert_eq!(rig.applied(arc.clone(), "s1.state"), [Wrote::Rewrote]);
        assert_ne!(rig.ino(), first);
        assert!(read_status(&status::status_dir(&rig.cfg().status).join("s1.json")).unwrap().archivist.contains("sweeping"));
        assert!(rig.applied(arc, "s1.state.tmp").is_empty(), "a temporary sibling names no session");
    }

    #[test_id(twd8rvf)]
    #[test]
    fn a_sweep_state_write_rewrites_every_status_once() {
        let rig = Rig::new();
        let arc = archivist_dir(&rig.cfg());
        rig.applied(status::sessions_dir(&rig.cfg().status), "s1.json");
        let first = rig.ino();
        std::fs::write(arc.join("sweep.state"), "sweep_state=skipped\nreason=capacity\n").unwrap();
        assert_eq!(rig.applied(arc.clone(), "sweep.state"), [Wrote::Rewrote]);
        assert_ne!(rig.ino(), first);
        let second = rig.ino();
        std::fs::write(arc.join("sweep.state"), "sweep_state=ran\n").unwrap();
        assert_eq!(rig.applied(arc.clone(), "sweep.state"), [Wrote::Rewrote], "a pass that ran undoes the skipped line");
        let third = rig.ino();
        assert_ne!(third, second);
        assert_eq!(rig.applied(arc, "sweep.state"), [Wrote::Touched]);
        assert_eq!(rig.ino(), third);
    }

    #[test_id(tghhptq)]
    #[test]
    fn start_writes_every_session_and_prunes_what_has_gone_idle() {
        let rig = Rig::new();
        let records = status::sessions_dir(&rig.cfg().status);
        put_record(&records.join("gone.json"), &Record { session: "gone".into(), ..Default::default() });
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5000);
        std::fs::File::open(records.join("gone.json")).unwrap().set_modified(old).unwrap();
        let done = start(&rig.cfg(), now());
        assert_eq!(done.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["gone", "s1"]);
        assert!(!records.join("gone.json").exists(), "a record past idle is removed");
        assert!(!status::status_dir(&rig.cfg().status).join("gone.json").exists(), "and its status with it");
        assert!(status::status_dir(&rig.cfg().status).join("s1.json").exists());
    }

    #[test_id(tlccnii)]
    #[test]
    fn the_source_is_dormant_unless_this_instance_writes() {
        let cfg = |k: &str| -> Result<String, String> { Ok(if k == WRITE_KEY { "0".into() } else { "1".into() }) };
        let source = CtxStatus::configured(&cfg, PathBuf::from("rt")).unwrap();
        assert!(!source.write);
        let (tx, _rx) = mpsc::channel(1);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(Box::new(source).run(tx));
        assert!(!Path::new("rt").exists(), "a dormant source writes nothing");
    }

    struct Scripted(Vec<Changes>);

    impl Watch for Scripted {
        fn add(&mut self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn changed(&mut self) -> Changed<'_> {
            let next = if self.0.is_empty() { Err("script ended".to_string()) } else { Ok(self.0.remove(0)) };
            Box::pin(async move { next })
        }
    }

    #[test_id(tbk5w1d)]
    #[tokio::test]
    async fn the_loop_writes_every_session_at_start_and_applies_each_change_it_hears() {
        let rig = Rig::new();
        let cfg = Arc::new(rig.cfg());
        let arc = archivist_dir(&cfg);
        std::fs::write(arc.join("s1.state"), "state=sweeping\n").unwrap();
        serve(cfg.clone(), Box::new(Scripted(vec![vec![(arc, vec!["s1.state".to_string()])]]))).await;
        let written = read_status(&status::status_dir(&cfg.status).join("s1.json")).unwrap();
        assert!(written.archivist.contains("sweeping"), "{written:?}");
    }
}
