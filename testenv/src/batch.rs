//! Running the selected suites in the container: serial or parallel, per-suite timeout,
//! exclusive drains, PSI admission, and the three harness faults that must never read as a
//! red suite (container death, exec storm, lost `--user` account). DESIGN.md §4.3. With
//! `BatchCfg::deadline`, a hard cut of the whole suite phase (DESIGN.md D7).

use crate::fixture::{is_user_account_fault, Fixtures, Liveness, Session};
use crate::record::{self, Mode, Producer, ResultRecord, Status};
use crate::runtime::cancelled;
use crate::schedule::Job;
use crate::skipgate::{self, SkipGate};
use crate::tap;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct BatchCfg {
    pub mode: Mode,
    pub producer: Producer,
    pub results: PathBuf,
    /// None disables the per-suite timeout.
    pub timeout: Option<Duration>,
    /// 0 = unlimited (parallel only).
    pub maxpar: u32,
    pub exec_fault_threshold: usize,
    pub quarantined: BTreeSet<String>,
    pub psi_pause: Duration,
    /// `--deadline`: measured from the start of [`run`]. None = no deadline (D7).
    pub deadline: Option<Duration>,
    /// The skip contract (DESIGN.md §3.7): an undeclared SKIP is reclassified red here, at
    /// the point the record is built, so the printed line and the written `.result` agree.
    pub skip_gate: SkipGate,
    /// `# testdb-mode: embedded` suites (suite.rs): exempted from the batch-wide server
    /// fixture (DESIGN-testdb.md §2.4) by stripping `SPIRA_TESTDB_MODE=server` from just
    /// their own environment (sp-gjx1b).
    pub embedded_only: BTreeSet<String>,
}

/// Side effects the executor reports through, so tests can observe them.
pub struct Hooks<'a> {
    pub log: &'a (dyn Fn(&str) + Sync),
    /// A per-suite stdout line (and its TAP detail lines).
    pub line: &'a (dyn Fn(&str) + Sync),
    /// suite, rc, wall_secs, bd_calls, bd_ms
    pub timing: &'a (dyn Fn(&str, i32, u64, u64, u64) + Sync),
    /// Memory pressure above the admission threshold right now.
    pub psi_high: &'a (dyn Fn() -> bool + Sync),
}

#[derive(Debug, Default)]
pub struct BatchOutcome {
    pub records: BTreeMap<String, ResultRecord>,
    pub container_dead: Option<String>,
    pub exec_fault: Option<usize>,
    pub account_fault: Option<String>,
    pub cancelled: bool,
    /// The deadline stopped the launch loop; jobs with no record were never started.
    pub deadline_hit: bool,
}

impl BatchOutcome {
    pub fn blocking_reds(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, r)| r.status.blocking())
            .map(|(s, _)| s.clone())
            .collect()
    }
    pub fn quarantined_reds(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, r)| r.status == Status::QuarantinedRed)
            .map(|(s, _)| s.clone())
            .collect()
    }
    /// Suites cut by the deadline (killed or never started), in name order.
    pub fn deferred(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, r)| r.status == Status::Deferred)
            .map(|(s, _)| s.clone())
            .collect()
    }
    pub fn harness_fault(&self) -> bool {
        self.container_dead.is_some()
            || self.exec_fault.is_some()
            || self.account_fault.is_some()
            || self.cancelled
    }
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// `.out` (and `.tap.json`) first, `.result` last: its presence means the suite completed.
pub fn write_suite(results: &Path, suite: &str, rec: &ResultRecord, output: &str) {
    let _ = fs::write(
        results.join(format!("{suite}.out")),
        record::normalize_output(output),
    );
    if rec.status.executed() {
        if let Ok(j) = serde_json::to_string(&tap::parse(output)) {
            let _ = fs::write(results.join(format!("{suite}.tap.json")), j);
        }
    }
    write_atomic(
        &results.join(format!("{suite}.result")),
        &format!("{rec}\n"),
    );
}

/// Pre-empted suites (disabled, skip-req) get a record and an empty `.out`.
pub fn write_preempted(results: &Path, suite: &str, rec: &ResultRecord) {
    let _ = fs::write(results.join(format!("{suite}.out")), "");
    write_atomic(
        &results.join(format!("{suite}.result")),
        &format!("{rec}\n"),
    );
}

fn detail_lines(rec: &ResultRecord, output: &str) -> Vec<String> {
    if rec.status != Status::Red && rec.status != Status::QuarantinedRed {
        return Vec::new();
    }
    tap::parse(output)
        .not_ok
        .into_iter()
        .take(10)
        .map(|l| format!("      {l}"))
        .collect()
}

struct Shared<'a> {
    session: &'a Session<'a>,
    cfg: &'a BatchCfg,
    hooks: &'a Hooks<'a>,
    fixtures: &'a Fixtures,
    records: Mutex<BTreeMap<String, ResultRecord>>,
    empty_output: Mutex<BTreeMap<String, bool>>,
    account_fault: Mutex<Option<String>>,
    deadline_at: Option<Instant>,
}

impl Shared<'_> {
    fn past_deadline(&self) -> bool {
        self.deadline_at.is_some_and(|d| Instant::now() >= d)
    }

    /// A suite cut by the deadline: `killed_after` Some = it was running and was killed
    /// (its partial output is kept), None = never started. No timing row: a truncated wall
    /// time would drag the suite's median down (D7).
    fn finish_deferred(&self, suite: &str, killed_after: Option<u64>, output: &str) {
        let rec = ResultRecord::deferred(
            suite,
            killed_after,
            self.cfg.mode,
            self.cfg.producer,
            now_epoch(),
        );
        write_suite(&self.cfg.results, suite, &rec, output);
        (self.hooks.line)(&record::suite_line(suite, &rec));
        self.records.lock().unwrap().insert(suite.to_string(), rec);
    }

    fn raw_path(&self, suite: &str) -> PathBuf {
        self.cfg.results.join(format!(".{suite}.raw"))
    }

    fn exec_suite(&self, n: usize, suite: &str) -> (i32, u64, String) {
        let mut req = self
            .session
            .suite_request(self.cfg.mode, n, suite, self.fixtures);
        if self.cfg.embedded_only.contains(suite) {
            // This suite declared `# testdb-mode: embedded`: undo the batch-wide server
            // fixture's env for it alone, so its own _testdb_embedded_check runs as it would
            // with no server template built at all (DESIGN-testdb.md §2.4, sp-gjx1b).
            req.env.retain(|(k, _)| k != "SPIRA_TESTDB_MODE");
        }
        req.timeout = self.cfg.timeout;
        req.deadline = self.deadline_at;
        let raw = self.raw_path(suite);
        req.output = Some(raw.clone());
        let t0 = Instant::now();
        let out = self.session.rt.exec(&req);
        let secs = t0.elapsed().as_secs();
        let _ = fs::remove_file(raw);
        (out.rc, secs, out.output)
    }

    fn finish(&self, n: usize, suite: &str, rc: i32, secs: u64, output: &str) {
        let rec = ResultRecord::from_exit(
            rc,
            secs,
            output,
            suite,
            self.cfg.quarantined.contains(suite),
            self.cfg.mode,
            self.cfg.producer,
            now_epoch(),
        );
        let rec = skipgate::apply(
            &self.cfg.skip_gate,
            suite,
            self.cfg.quarantined.contains(suite),
            rec,
        );
        write_suite(&self.cfg.results, suite, &rec, output);
        (self.hooks.line)(&record::suite_line(suite, &rec));
        for l in detail_lines(&rec, output) {
            (self.hooks.line)(&l);
        }
        self.empty_output
            .lock()
            .unwrap()
            .insert(suite.to_string(), output.trim().is_empty());
        self.records.lock().unwrap().insert(suite.to_string(), rec);
        let (calls, ms) = crate::timing::bd_totals(&self.session.read_file(&self.session.bd_log(
            self.cfg.mode,
            n,
            suite,
        )));
        (self.hooks.timing)(suite, rc, secs, calls, ms);
    }

    /// A red with 0 s and no output — or the account-fault text — is podman failing to
    /// reach the suite, not the suite failing: rewrite it unreached.
    fn reclassify(&self, include_account_text: bool) {
        let empties = self.empty_output.lock().unwrap().clone();
        let mut recs = self.records.lock().unwrap();
        for (suite, rec) in recs.iter_mut() {
            if rec.status != Status::Red {
                continue;
            }
            let out = fs::read_to_string(self.cfg.results.join(format!("{suite}.out")))
                .unwrap_or_default();
            let storm_shape = rec.secs == 0 && empties.get(suite).copied().unwrap_or(false);
            if storm_shape || (include_account_text && is_user_account_fault(&out)) {
                *rec = ResultRecord::unreached(now_epoch());
                write_preempted(&self.cfg.results, suite, rec);
            }
        }
    }

    fn storm_count(&self) -> usize {
        let empties = self.empty_output.lock().unwrap();
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, r)| {
                r.status == Status::Red && r.secs == 0 && empties.get(*s).copied().unwrap_or(false)
            })
            .count()
    }
}

struct DoneOnDrop(mpsc::Sender<()>);
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Run `jobs` (already ordered) and return what happened. Suites never reached have no
/// record here; the caller writes them `unreached`.
pub fn run(
    session: &Session,
    cfg: &BatchCfg,
    hooks: &Hooks,
    fixtures: &Fixtures,
    jobs: &[Job],
) -> BatchOutcome {
    let sh = Shared {
        session,
        cfg,
        hooks,
        fixtures,
        records: Mutex::new(BTreeMap::new()),
        empty_output: Mutex::new(BTreeMap::new()),
        account_fault: Mutex::new(None),
        // The clock starts as the first suite is scheduled (D7).
        deadline_at: cfg.deadline.map(|d| Instant::now() + d),
    };
    let mut outcome = BatchOutcome::default();
    match cfg.mode {
        Mode::Serial => run_serial(&sh, jobs, &mut outcome),
        Mode::Parallel => run_parallel(&sh, jobs, &mut outcome),
    }
    if outcome.deadline_hit && !outcome.harness_fault() {
        let d = cfg.deadline.map(|d| d.as_secs()).unwrap_or(0);
        (hooks.log)(&format!(
            "deadline {d}s reached — no further suites started; running suites killed"
        ));
        for job in jobs {
            let seen = sh.records.lock().unwrap().contains_key(&job.name);
            if !seen {
                sh.finish_deferred(&job.name, None, "");
            }
        }
    }
    outcome.records = sh.records.into_inner().unwrap();
    outcome.cancelled |= cancelled();
    outcome
}

fn run_serial(sh: &Shared, jobs: &[Job], outcome: &mut BatchOutcome) {
    let mut consecutive = 0usize;
    for (i, job) in jobs.iter().enumerate() {
        if cancelled() {
            outcome.cancelled = true;
            break;
        }
        if sh.past_deadline() {
            outcome.deadline_hit = true;
            break;
        }
        let n = i + 1;
        let (rc, secs, output) = sh.exec_suite(n, &job.name);
        if rc == crate::runtime::RC_DEADLINE {
            sh.finish_deferred(&job.name, Some(secs), &output);
            outcome.deadline_hit = true;
            break;
        }
        if rc != 0 && secs == 0 && output.trim().is_empty() {
            consecutive += 1;
        } else {
            consecutive = 0;
        }
        if consecutive >= sh.cfg.exec_fault_threshold {
            (sh.hooks.log)(&format!("harness fault — {consecutive} consecutive exec failures (podman exec not reaching suites)"));
            outcome.exec_fault = Some(consecutive);
            sh.reclassify(false);
            break;
        }
        if rc != 0 && is_user_account_fault(&output) {
            let d = format!("podman exec --user spirauser failed during {}: account missing from container passwd", job.name);
            (sh.hooks.log)(&format!(
                "harness fault — {d} — remaining suites will be unreached"
            ));
            outcome.account_fault = Some(d);
            break;
        }
        if let Liveness::Dead { detail } = sh.session.liveness() {
            (sh.hooks.log)(&format!(
                "container died during {} ({detail}) — remaining suites will be unreached",
                job.name
            ));
            outcome.container_dead = Some(detail);
            break;
        }
        if rc == crate::runtime::RC_CANCELLED && cancelled() {
            outcome.cancelled = true;
            break;
        }
        sh.finish(n, &job.name, rc, secs, &output);
    }
}

fn run_parallel(sh: &Shared, jobs: &[Job], outcome: &mut BatchOutcome) {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let mut inflight = 0usize;
        let wait_one = |inflight: &mut usize| {
            if *inflight > 0 && rx.recv().is_ok() {
                *inflight -= 1;
            }
        };
        for (idx, job) in jobs.iter().enumerate() {
            if cancelled() {
                outcome.cancelled = true;
                break;
            }
            if let Some(reason) = &job.exclusive {
                while inflight > 0 {
                    wait_one(&mut inflight);
                }
                (sh.hooks.log)(&format!(
                    "draining for exclusive suite {} ({reason})",
                    job.name
                ));
            }
            while (sh.hooks.psi_high)() && !cancelled() && !sh.past_deadline() {
                (sh.hooks.log)("memory pressure above threshold — pausing suite launch");
                std::thread::sleep(sh.cfg.psi_pause);
            }
            let n = idx + 1;
            if job.exclusive.is_none() && sh.cfg.maxpar > 0 {
                while inflight >= sh.cfg.maxpar as usize {
                    wait_one(&mut inflight);
                }
            }
            if sh.past_deadline() {
                outcome.deadline_hit = true;
                break;
            }
            if let Liveness::Dead { detail } = sh.session.liveness() {
                (sh.hooks.log)(&format!(
                    "container died before suite {} — remaining suites will be unreached",
                    job.name
                ));
                outcome.container_dead = Some(detail);
                break;
            }
            if let Some(d) = sh.account_fault.lock().unwrap().clone() {
                (sh.hooks.log)(&format!(
                    "harness fault — {d} — remaining suites will be unreached"
                ));
                outcome.account_fault = Some(d);
                break;
            }
            sh.session.make_home(n);
            let done = DoneOnDrop(tx.clone());
            let name = job.name.clone();
            scope.spawn(move || {
                let _done = done;
                let (rc, secs, output) = sh.exec_suite(n, &name);
                if rc == crate::runtime::RC_CANCELLED && cancelled() {
                    return;
                }
                if rc == crate::runtime::RC_DEADLINE {
                    sh.finish_deferred(&name, Some(secs), &output);
                    return;
                }
                if rc != 0 && is_user_account_fault(&output) {
                    *sh.account_fault.lock().unwrap() = Some(format!("podman exec --user spirauser failed during {name}: account missing from container passwd"));
                }
                sh.finish(n, &name, rc, secs, &output);
            });
            inflight += 1;
            if job.exclusive.is_some() {
                while inflight > 0 {
                    wait_one(&mut inflight);
                }
            }
        }
        while inflight > 0 {
            wait_one(&mut inflight);
        }
    });

    if outcome.account_fault.is_none() {
        if let Some(d) = sh.account_fault.lock().unwrap().clone() {
            (sh.hooks.log)(&format!("harness fault — {d}"));
            outcome.account_fault = Some(d);
        }
    }
    if outcome.container_dead.is_none() {
        match sh.session.liveness() {
            Liveness::Dead { detail } => {
                (sh.hooks.log)(&format!("container died during parallel run ({detail})"));
                outcome.container_dead = Some(detail);
            }
            Liveness::Alive => {
                let k = sh.storm_count();
                if k >= sh.cfg.exec_fault_threshold {
                    (sh.hooks.log)(&format!("harness fault — {k} parallel exec failures (podman exec not reaching suites)"));
                    outcome.exec_fault = Some(k);
                }
            }
        }
    }
    if outcome.container_dead.is_some()
        || outcome.exec_fault.is_some()
        || outcome.account_fault.is_some()
    {
        sh.reclassify(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::fake::FakeRuntime;
    use crate::runtime::ExecOutcome;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn tmpdir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("testenv-batch-{tag}"))
    }

    fn cfg(mode: Mode, results: &Path, maxpar: u32) -> BatchCfg {
        BatchCfg {
            mode,
            producer: Producer::Explicit,
            results: results.to_path_buf(),
            timeout: None,
            maxpar,
            exec_fault_threshold: 5,
            quarantined: BTreeSet::new(),
            psi_pause: Duration::from_millis(1),
            deadline: None,
            skip_gate: SkipGate::load("").unwrap(),
            embedded_only: BTreeSet::new(),
        }
    }

    fn jobs(names: &[&str]) -> Vec<Job> {
        names
            .iter()
            .map(|n| Job {
                name: n.to_string(),
                exclusive: None,
            })
            .collect()
    }

    struct Seen {
        lines: Mutex<Vec<String>>,
        logs: Mutex<Vec<String>>,
        timings: Mutex<Vec<(String, i32)>>,
    }

    fn with_hooks<R>(f: impl FnOnce(&Hooks, &Seen) -> R) -> R {
        let seen = Seen {
            lines: Mutex::new(vec![]),
            logs: Mutex::new(vec![]),
            timings: Mutex::new(vec![]),
        };
        let log = |s: &str| seen.logs.lock().unwrap().push(s.to_string());
        let line = |s: &str| seen.lines.lock().unwrap().push(s.to_string());
        let timing = |s: &str, rc: i32, _: u64, _: u64, _: u64| {
            seen.timings.lock().unwrap().push((s.to_string(), rc))
        };
        let psi = || false;
        let hooks = Hooks {
            log: &log,
            line: &line,
            timing: &timing,
            psi_high: &psi,
        };
        f(&hooks, &seen)
    }

    fn session(rt: &FakeRuntime) -> Session<'_> {
        let mut s = Session::new(rt, "i1", "aeon");
        s.liveness_sleep = Duration::from_millis(1);
        s
    }

    #[test]
    fn parallel_records_every_outcome_and_writes_out_before_result() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok 1 - a\n");
        rt.suite("test-b.sh", 1, "not ok 1 - b broke\nFAIL b\n");
        rt.suite("test-c.sh", 77, "1..0 # SKIP not applicable here\n");
        rt.suite("test-d.sh", 124, "slow\n");
        let dir = tmpdir("par");
        let s = session(&rt);
        let mut c = cfg(Mode::Parallel, &dir, 2);
        // declared, so this test still exercises a genuine (green) Skip outcome — the skip
        // contract itself (undeclared -> red) is covered by skipgate.rs and run/tests.rs.
        c.skip_gate = SkipGate::load(
            "test-c.sh\tskip:not_applicable_here\tfixture: declared for this test\n",
        )
        .unwrap();
        let out = with_hooks(|h, seen| {
            let o = run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-a.sh", "test-b.sh", "test-c.sh", "test-d.sh"]),
            );
            assert_eq!(seen.timings.lock().unwrap().len(), 4);
            assert!(seen
                .lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.trim() == "not ok 1 - b broke"));
            o
        });
        assert!(!out.harness_fault());
        assert_eq!(out.records["test-a.sh"].status, Status::Ok);
        assert_eq!(out.records["test-b.sh"].status, Status::Red);
        assert_eq!(out.records["test-c.sh"].status, Status::Skip);
        assert_eq!(out.records["test-d.sh"].status, Status::Timeout);
        assert_eq!(out.blocking_reds(), vec!["test-b.sh", "test-d.sh"]);
        let res = fs::read_to_string(dir.join("test-b.sh.result")).unwrap();
        assert!(res.starts_with("red "));
        assert!(res.trim_end().ends_with(" parallel explicit 1"));
        assert_eq!(
            fs::read_to_string(dir.join("test-b.sh.out")).unwrap(),
            "not ok 1 - b broke\nFAIL b\n"
        );
        assert!(dir.join("test-b.sh.tap.json").exists());
        // per-suite HOME created first, each suite its own SPIRA_INSTANCE
        let insts: BTreeSet<String> = rt
            .suite_execs()
            .iter()
            .map(|r| r.env_value("SPIRA_INSTANCE").unwrap().to_string())
            .collect();
        assert_eq!(insts.len(), 4);
        assert!(rt
            .exec_argv()
            .iter()
            .any(|a| a.first().is_some_and(|c| c.ends_with("/bd-meter"))
                && a.get(1).map(String::as_str) == Some("--install")));
    }

    #[test]
    fn quarantined_reds_do_not_block() {
        let rt = FakeRuntime::new();
        rt.suite("test-q.sh", 2, "FAIL\n");
        let dir = tmpdir("quar");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.quarantined.insert("test-q.sh".into());
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-q.sh"])));
        assert_eq!(out.records["test-q.sh"].status, Status::QuarantinedRed);
        assert!(out.blocking_reds().is_empty());
        assert_eq!(out.quarantined_reds(), vec!["test-q.sh"]);
    }

    /// `# testdb-mode: embedded` (sp-gjx1b): a suite named in `embedded_only` never sees the
    /// batch-wide server fixture's env, even though every other suite does.
    #[test]
    fn embedded_only_suites_do_not_see_the_batch_wide_server_fixture_env() {
        let rt = FakeRuntime::new();
        rt.suite("test-emb.sh", 0, "ok\n");
        rt.suite("test-srv.sh", 0, "ok\n");
        let dir = tmpdir("embedded-only");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.embedded_only.insert("test-emb.sh".into());
        with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::Server,
                &jobs(&["test-emb.sh", "test-srv.sh"]),
            )
        });
        let execs = rt.suite_execs();
        let emb = execs
            .iter()
            .find(|r| r.argv[1].ends_with("test-emb.sh"))
            .unwrap();
        let srv = execs
            .iter()
            .find(|r| r.argv[1].ends_with("test-srv.sh"))
            .unwrap();
        assert_eq!(emb.env_value("SPIRA_TESTDB_MODE"), None);
        assert_eq!(srv.env_value("SPIRA_TESTDB_MODE"), Some("server"));
    }

    #[test]
    fn maxpar_bounds_concurrency() {
        let rt = FakeRuntime::new();
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (l2, p2) = (live.clone(), peak.clone());
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
            },
            move |_| {
                let now = l2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                l2.fetch_sub(1, Ordering::SeqCst);
                ExecOutcome {
                    rc: 0,
                    output: "ok\n".into(),
                }
            },
        );
        let dir = tmpdir("maxpar");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 2);
        let names: Vec<String> = (0..8).map(|i| format!("test-{i}.sh")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&refs)));
        assert_eq!(out.records.len(), 8);
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert!(peak.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn exclusive_suite_runs_alone() {
        let rt = FakeRuntime::new();
        let live = Arc::new(AtomicUsize::new(0));
        let excl_saw = Arc::new(AtomicUsize::new(99));
        let (l2, e2) = (live.clone(), excl_saw.clone());
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
            },
            move |r| {
                let now = l2.fetch_add(1, Ordering::SeqCst) + 1;
                if r.argv[1].ends_with("test-x.sh") {
                    e2.store(now, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(20));
                l2.fetch_sub(1, Ordering::SeqCst);
                ExecOutcome {
                    rc: 0,
                    output: "ok\n".into(),
                }
            },
        );
        let dir = tmpdir("excl");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 0);
        let mut js = jobs(&["test-a.sh", "test-b.sh", "test-x.sh", "test-c.sh"]);
        js[2].exclusive = Some("heavy".into());
        let out = with_hooks(|h, seen| {
            let o = run(&s, &c, h, &Fixtures::PerSuite, &js);
            assert!(seen
                .logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("draining for exclusive suite test-x.sh (heavy)")));
            o
        });
        assert_eq!(out.records.len(), 4);
        assert_eq!(excl_saw.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn container_death_mid_parallel_leaves_the_rest_unrecorded() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        // alive for the first launch check, then dead (definitive false) for the second.
        rt.running_answers(&[Some("true"), Some("false")]);
        let dir = tmpdir("dead");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 1);
        let out = with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-a.sh", "test-b.sh", "test-c.sh"]),
            )
        });
        assert!(out.container_dead.is_some());
        assert!(out.harness_fault());
        assert!(!out.records.contains_key("test-b.sh"));
        assert!(!out.records.contains_key("test-c.sh"));
    }

    #[test]
    fn exec_storm_in_parallel_reclassifies_empty_zero_second_reds() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
            },
            |_| ExecOutcome {
                rc: 125,
                output: String::new(),
            },
        );
        let dir = tmpdir("storm");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 0);
        let names: Vec<String> = (0..6).map(|i| format!("test-{i}.sh")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&refs)));
        assert_eq!(out.exec_fault, Some(6));
        assert!(out.records.values().all(|r| r.status == Status::Unreached));
        assert_eq!(
            fs::read_to_string(dir.join("test-0.sh.result"))
                .unwrap()
                .trim(),
            format!("unreached {} 0 -", out.records["test-0.sh"].epoch)
        );
    }

    #[test]
    fn exec_storm_in_serial_breaks_after_the_threshold() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
            },
            |_| ExecOutcome {
                rc: 1,
                output: String::new(),
            },
        );
        let dir = tmpdir("sstorm");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.exec_fault_threshold = 3;
        let names: Vec<String> = (0..6).map(|i| format!("test-{i}.sh")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&refs)));
        assert_eq!(out.exec_fault, Some(3));
        assert_eq!(rt.suite_execs().len(), 3);
        assert_eq!(out.records.len(), 2);
        assert!(out.records.values().all(|r| r.status == Status::Unreached));
    }

    #[test]
    fn lost_user_account_stops_serial_before_recording_the_suite() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        rt.suite(
            "test-b.sh",
            126,
            "Error: unable to find user spirauser: no matching entries in passwd file\n",
        );
        let dir = tmpdir("acct");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        let out = with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-a.sh", "test-b.sh", "test-c.sh"]),
            )
        });
        assert!(out.account_fault.is_some());
        assert_eq!(out.records.len(), 1);
        assert_eq!(rt.suite_execs().len(), 2);
    }

    #[test]
    fn lost_user_account_in_parallel_is_reclassified_unreached() {
        let rt = FakeRuntime::new();
        rt.suite(
            "test-b.sh",
            126,
            "Error: unable to find user spirauser: no matching entries in passwd file\n",
        );
        rt.suite("test-a.sh", 0, "ok\n");
        let dir = tmpdir("pacct");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 1);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-b.sh", "test-a.sh"])));
        assert!(out.account_fault.is_some());
        assert_eq!(out.records["test-b.sh"].status, Status::Unreached);
        assert!(!out.records.contains_key("test-a.sh"));
    }

    #[test]
    fn serial_death_check_after_each_suite() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        rt.suite("test-b.sh", 1, "boom\n");
        rt.running_answers(&[Some("true"), Some("false")]);
        let dir = tmpdir("sdead");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        let out = with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-a.sh", "test-b.sh", "test-c.sh"]),
            )
        });
        assert!(out.container_dead.is_some());
        assert_eq!(
            out.records.keys().cloned().collect::<Vec<_>>(),
            vec!["test-a.sh"]
        );
        let e = rt.suite_execs();
        assert_eq!(e[0].env_value("SPIRA_RUN"), Some("/tmp/spira-batch-i1"));
    }

    #[test]
    fn timeout_is_passed_to_every_suite_exec() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        let dir = tmpdir("tmo");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.timeout = Some(Duration::from_secs(600));
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert_eq!(rt.suite_execs()[0].timeout, Some(Duration::from_secs(600)));
    }

    /// Suites that take `ms` each but honour the exec's deadline the way `run_bounded` does:
    /// killed at the deadline with RC_DEADLINE and their partial output.
    fn timed_suites(rt: &FakeRuntime, times: &[(&str, u64, i32)]) {
        let table: Vec<(String, u64, i32)> = times
            .iter()
            .map(|(n, ms, rc)| (format!("/workspace/spira/{n}"), *ms, *rc))
            .collect();
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
            },
            move |r| {
                let (_, ms, rc) = table
                    .iter()
                    .find(|(p, _, _)| Some(p) == r.argv.get(1))
                    .cloned()
                    .unwrap_or_default();
                let end = Instant::now() + Duration::from_millis(ms);
                loop {
                    if r.deadline.is_some_and(|d| Instant::now() >= d) {
                        return ExecOutcome {
                            rc: crate::runtime::RC_DEADLINE,
                            output: "partial\n".into(),
                        };
                    }
                    if Instant::now() >= end {
                        return ExecOutcome {
                            rc,
                            output: if rc == 0 {
                                "ok\n".into()
                            } else {
                                "FAIL x\n".into()
                            },
                        };
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            },
        );
    }

    #[test]
    fn deadline_stops_new_starts_in_serial() {
        let rt = FakeRuntime::new();
        timed_suites(
            &rt,
            &[
                ("test-a.sh", 80, 0),
                ("test-b.sh", 80, 0),
                ("test-c.sh", 80, 0),
            ],
        );
        let dir = tmpdir("dl-serial");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.deadline = Some(Duration::from_millis(120));
        let out = with_hooks(|h, seen| {
            let o = run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-a.sh", "test-b.sh", "test-c.sh"]),
            );
            assert!(seen
                .logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("deadline 0s reached")));
            // no timing row for a deferred suite
            assert_eq!(seen.timings.lock().unwrap().len(), 1);
            o
        });
        assert!(!out.harness_fault());
        assert!(out.deadline_hit);
        assert_eq!(out.records["test-a.sh"].status, Status::Ok);
        // b was running at the deadline: killed, deferred, partial output kept
        assert_eq!(out.records["test-b.sh"].status, Status::Deferred);
        assert_eq!(out.records["test-b.sh"].fingerprint, "deadline:test-b.sh");
        assert_eq!(
            fs::read_to_string(dir.join("test-b.sh.out")).unwrap(),
            "partial\n"
        );
        // c never started
        assert_eq!(out.records["test-c.sh"].status, Status::Deferred);
        assert_eq!(out.records["test-c.sh"].fingerprint, "-");
        assert_eq!(rt.suite_execs().len(), 2);
        assert_eq!(out.deferred(), vec!["test-b.sh", "test-c.sh"]);
        assert!(out.blocking_reds().is_empty());
        assert!(fs::read_to_string(dir.join("test-c.sh.result"))
            .unwrap()
            .starts_with("deferred "));
    }

    #[test]
    fn deadline_in_parallel_kills_the_running_and_starts_nothing_new() {
        let rt = FakeRuntime::new();
        // order is kept: the long one first (the selector's priority), then shorts
        timed_suites(
            &rt,
            &[
                ("test-long.sh", 5_000, 0),
                ("test-s1.sh", 10, 0),
                ("test-s2.sh", 10, 1),
                ("test-s3.sh", 400, 0),
                ("test-s4.sh", 10, 0),
            ],
        );
        let dir = tmpdir("dl-par");
        let s = session(&rt);
        let mut c = cfg(Mode::Parallel, &dir, 2);
        c.deadline = Some(Duration::from_millis(200));
        let t0 = Instant::now();
        let out = with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&[
                    "test-long.sh",
                    "test-s1.sh",
                    "test-s2.sh",
                    "test-s3.sh",
                    "test-s4.sh",
                ]),
            )
        });
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "the deadline is hard"
        );
        assert_eq!(out.records["test-long.sh"].status, Status::Deferred);
        assert_eq!(
            out.records["test-long.sh"].fingerprint,
            "deadline:test-long.sh"
        );
        assert_eq!(out.records["test-s1.sh"].status, Status::Ok);
        // a red that finished before the deadline stays red and blocking
        assert_eq!(out.records["test-s2.sh"].status, Status::Red);
        assert_eq!(out.blocking_reds(), vec!["test-s2.sh"]);
        // s3 was started before the deadline and killed at it; s4 never started
        assert_eq!(out.records["test-s3.sh"].status, Status::Deferred);
        assert_eq!(out.records["test-s4.sh"].status, Status::Deferred);
        assert_eq!(out.records["test-s4.sh"].fingerprint, "-");
        // exec calls race across threads, so compare the set that started
        let mut started: Vec<String> = rt
            .suite_execs()
            .iter()
            .map(|r| r.argv[1].trim_start_matches("/workspace/spira/").to_string())
            .collect();
        started.sort();
        assert_eq!(
            started,
            vec!["test-long.sh", "test-s1.sh", "test-s2.sh", "test-s3.sh"]
        );
        assert!(rt.suite_execs().iter().all(|r| r.deadline.is_some()));
    }

    #[test]
    fn a_suite_that_hits_its_own_timeout_before_the_deadline_is_still_a_timeout() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 124, "slow\n");
        let dir = tmpdir("dl-tmo");
        let s = session(&rt);
        let mut c = cfg(Mode::Serial, &dir, 0);
        c.deadline = Some(Duration::from_secs(60));
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert_eq!(out.records["test-a.sh"].status, Status::Timeout);
        assert!(!out.deadline_hit);
        assert!(out.deferred().is_empty());
    }

    #[test]
    fn without_a_deadline_nothing_is_deferred_and_no_exec_carries_one() {
        let rt = FakeRuntime::new();
        timed_suites(&rt, &[("test-a.sh", 30, 0), ("test-b.sh", 30, 0)]);
        let dir = tmpdir("dl-none");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 1);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh", "test-b.sh"])));
        assert!(!out.deadline_hit);
        assert!(out.deferred().is_empty());
        assert!(out.records.values().all(|r| r.status == Status::Ok));
        assert!(rt.suite_execs().iter().all(|r| r.deadline.is_none()));
    }
}
