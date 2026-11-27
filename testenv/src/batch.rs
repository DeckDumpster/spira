//! Running the selected suites in the container: serial or parallel, per-suite timeout,
//! exclusive drains, PSI admission, and the three harness faults that must never read as a
//! red suite (container death, exec storm, lost `--user` account). DESIGN.md §4.3. With
//! `BatchCfg::deadline`, a hard cut of the whole suite phase (DESIGN.md D7).

use crate::phase::{RunEvent, RunLog};
use crate::build::is_enospc;
use crate::fixture::{is_user_account_fault, Fixtures, Liveness, Session};
use crate::record::{self, Mode, Producer, ResultRecord, Status};
use crate::runtime::cancelled;
use crate::schedule::{Admission, Job};
use crate::tap;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub fn now_epoch() -> u64 {
    spira_config::vtime::now_epoch()
}

#[derive(Clone)]
pub struct BatchCfg {
    pub mode: Mode,
    pub producer: Producer,
    pub results: PathBuf,
    /// None disables the per-suite timeout.
    pub timeout: Option<Duration>,
    /// 0 = unlimited (parallel only).
    pub maxpar: u32,
    /// Process-weight budget the running suites' `# pids:` declarations may fill.
    pub pids_budget: u32,
    /// Suites of one lane running at once; 0 = no per-lane cap.
    pub lane_slots: u32,
    pub exec_fault_threshold: usize,
    pub quarantined: BTreeSet<String>,
    pub psi_pause: Duration,
    pub liveness_every: Duration,
    /// `--deadline`: measured from the start of [`run`]. None = no deadline (D7).
    pub deadline: Option<Duration>,
    /// `# testdb-mode: embedded` suites (suite.rs): exempted from the batch-wide server
    /// fixture (DESIGN-testdb.md §2.4) by stripping `SPIRA_TESTDB_MODE=server` from just
    /// their own environment (sp-gjx1b).
    pub embedded_only: BTreeSet<String>,
    pub resume: Option<crate::resume::Store>,
}

/// The sim container is its own pool: `SIM_SLOTS` bounds the lane and its own pids budget
/// applies, so the per-lane cap of the shared pool is off.
pub fn sim_cfg(main: &BatchCfg) -> BatchCfg {
    BatchCfg {
        maxpar: crate::schedule::SIM_SLOTS,
        pids_budget: crate::schedule::sim_pids_budget(),
        lane_slots: 0,
        ..main.clone()
    }
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
    /// A red suite's output says the disk was full: the pass has no verdict, the box does.
    pub disk_fault: Option<String>,
    pub cancelled: bool,
    /// The deadline stopped the launch loop; jobs with no record were never started.
    pub deadline_hit: bool,
}

impl BatchOutcome {
    /// Folds a concurrently-run lane's outcome into this one.
    pub fn absorb(&mut self, other: BatchOutcome) {
        self.records.extend(other.records);
        self.container_dead = self.container_dead.take().or(other.container_dead);
        self.exec_fault = self.exec_fault.take().or(other.exec_fault);
        self.account_fault = self.account_fault.take().or(other.account_fault);
        self.disk_fault = self.disk_fault.take().or(other.disk_fault);
        self.cancelled |= other.cancelled;
        self.deadline_hit |= other.deadline_hit;
    }

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
    /// Suites whose exit status podman's own exec lost (sp-3azqi): never red, never
    /// green, their own verdict unknown — named so the batch can report FAULT, not RED.
    pub fn faulted(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, r)| r.status.is_fault())
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
            || self.disk_fault.is_some()
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
/// Returns whether `.out` was written.
pub fn write_suite(results: &Path, suite: &str, rec: &ResultRecord, output: &str) -> bool {
    let wrote_out = fs::write(
        results.join(format!("{suite}.out")),
        record::normalize_output(output),
    )
    .is_ok();
    if rec.status.executed() {
        if let Ok(j) = serde_json::to_string(&tap::parse(output)) {
            let _ = fs::write(results.join(format!("{suite}.tap.json")), j);
        }
    }
    write_atomic(
        &results.join(format!("{suite}.result")),
        &format!("{rec}\n"),
    );
    record_suite(results, suite, rec);
    wrote_out
}

fn record_suite(results: &Path, suite: &str, rec: &ResultRecord) {
    let event = RunEvent::Suite { name: suite.into(), status: rec.status.as_str().into() };
    if let Err(e) = RunLog::in_dir(results).record(event, now_epoch()) {
        eprintln!("batch: run event not recorded: {e}");
    }
}

/// Pre-empted suites (disabled, skip-req) get a record and an empty `.out`.
pub fn write_preempted(results: &Path, suite: &str, rec: &ResultRecord) {
    let _ = fs::write(results.join(format!("{suite}.out")), "");
    write_atomic(
        &results.join(format!("{suite}.result")),
        &format!("{rec}\n"),
    );
    record_suite(results, suite, rec);
}

fn detail_lines(rec: &ResultRecord, output: &str) -> Vec<String> {
    if rec.status != Status::Red && rec.status != Status::QuarantinedRed {
        return Vec::new();
    }
    output.lines().map(|l| format!("      {l}")).collect()
}

fn record_step(rc: i32, secs: u64) -> String {
    format!("record, exec rc={rc} after {secs}s")
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
    finished: Mutex<Vec<(usize, String, i32, u64)>>,
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
        if write_suite(&self.cfg.results, suite, &rec, output) {
            self.drop_raw(suite);
        }
        (self.hooks.line)(&record::suite_line(suite, &rec));
        self.records.lock().unwrap().insert(suite.to_string(), rec);
    }

    fn raw_path(&self, suite: &str) -> PathBuf {
        self.cfg.results.join(format!(".{suite}.raw"))
    }

    /// The guard keeps the suite in the cap's report until the caller has written its record.
    fn exec_suite(&self, n: usize, suite: &str) -> (i32, u64, String, crate::cap::Running) {
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
        let running = crate::cap::running(suite);
        running.log(&raw);
        running.step("exec");
        let t0 = Instant::now();
        let out = self.session.rt.exec(&req);
        let secs = t0.elapsed().as_secs();
        running.step(&record_step(out.rc, secs));
        (out.rc, secs, out.output, running)
    }

    /// Only for a suite whose `.out` was written: that file takes the raw log's place.
    fn drop_raw(&self, suite: &str) {
        let out = self.cfg.results.join(format!("{suite}.out"));
        crate::cap::supersede_log(&self.raw_path(suite), &out);
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
        let rec = rec.skip_as_red(self.cfg.quarantined.contains(suite));
        if write_suite(&self.cfg.results, suite, &rec, output) {
            self.drop_raw(suite);
        }
        if rec.status == Status::Ok {
            if let Some(store) = &self.cfg.resume {
                store.record(suite);
            }
        }
        (self.hooks.line)(&record::red_suite_line(suite, &rec, output));
        for l in detail_lines(&rec, output) {
            (self.hooks.line)(&l);
        }
        self.empty_output
            .lock()
            .unwrap()
            .insert(suite.to_string(), output.trim().is_empty());
        self.records.lock().unwrap().insert(suite.to_string(), rec);
        self.finished
            .lock()
            .unwrap()
            .push((n, suite.to_string(), rc, secs));
    }

    /// One exec reads every finished suite's bd log, instead of one per suite.
    fn report_timing(&self) {
        let done = std::mem::take(&mut *self.finished.lock().unwrap());
        let paths: Vec<String> = done
            .iter()
            .map(|(n, suite, _, _)| self.session.bd_log(self.cfg.mode, *n, suite))
            .collect();
        let logs = self.session.read_files(&paths);
        for ((_, suite, rc, secs), log) in done.iter().zip(logs) {
            let (calls, ms) = crate::timing::bd_totals(&log);
            (self.hooks.timing)(suite, *rc, *secs, calls, ms);
        }
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

    /// The first red suite whose output says the disk filled, with the container's free
    /// space on the filesystems a suite writes to.
    fn disk_fault(&self) -> Option<String> {
        let suite = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| matches!(r.status, Status::Red | Status::QuarantinedRed))
            .map(|(s, _)| s.clone())
            .find(|s| {
                fs::read_to_string(self.cfg.results.join(format!("{s}.out")))
                    .is_ok_and(|o| is_enospc(&o))
            })?;
        let d = format!(
            "{suite} hit 'No space left on device'; container free space: {}",
            self.session.free_space()
        );
        Some(d)
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

struct DoneOnDrop(mpsc::Sender<(u32, Option<String>)>, (u32, Option<String>));
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(self.1.clone());
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
        finished: Mutex::new(Vec::new()),
        // The clock starts as the first suite is scheduled (D7).
        deadline_at: cfg.deadline.map(|d| Instant::now() + d),
    };
    let mut outcome = BatchOutcome::default();
    match cfg.mode {
        Mode::Serial => run_serial(&sh, jobs, &mut outcome),
        Mode::Parallel => run_parallel(&sh, jobs, &mut outcome),
    }
    if !outcome.harness_fault() && !outcome.deadline_hit {
        rerun_lost(&sh, jobs);
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
    sh.report_timing();
    outcome.disk_fault = sh.disk_fault();
    outcome.records = sh.records.into_inner().unwrap();
    outcome.cancelled |= cancelled();
    outcome
}

/// A suite whose exit status podman lost is run once more, serially, and its second
/// record replaces the first: one lost status must not fault the other suites' verdicts.
fn rerun_lost(sh: &Shared, jobs: &[Job]) {
    let lost: Vec<(usize, &Job)> = jobs
        .iter()
        .enumerate()
        .filter(|(_, j)| {
            sh.records
                .lock()
                .unwrap()
                .get(&j.name)
                .is_some_and(|r| r.status.is_fault())
        })
        .collect();
    for (idx, job) in lost {
        if cancelled() || sh.past_deadline() {
            return;
        }
        let n = idx + 1;
        (sh.hooks.log)(&format!(
            "podman lost the exit status of {} — re-running it once",
            job.name
        ));
        if sh.cfg.mode == Mode::Parallel {
            sh.session.make_home(n);
        }
        let (rc, secs, output, _running) = sh.exec_suite(n, &job.name);
        if rc == crate::runtime::RC_DEADLINE {
            return;
        }
        if rc == crate::runtime::RC_CANCELLED && cancelled() {
            return;
        }
        sh.finish(n, &job.name, rc, secs, &output);
    }
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
        let (rc, secs, output, _running) = sh.exec_suite(n, &job.name);
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
    let (tx, rx) = mpsc::channel::<(u32, Option<String>)>();
    let mut adm = Admission::new(sh.cfg.maxpar, sh.cfg.pids_budget, sh.cfg.lane_slots);
    let mut last_alive: Option<Instant> = None;
    std::thread::scope(|scope| {
        let wait_one = |adm: &mut Admission| {
            if adm.running() > 0 {
                if let Ok((w, lane)) = rx.recv() {
                    adm.release(w, &lane);
                }
            }
        };
        let mut pending: Vec<(usize, &Job)> = jobs.iter().enumerate().collect();
        while !pending.is_empty() {
            if cancelled() {
                outcome.cancelled = true;
                break;
            }
            // The first suite that fits, so a lane at its slots or a heavy suite waiting on
            // weight never idles the pool; an exclusive suite is a barrier nothing passes.
            let pos = pending
                .iter()
                .enumerate()
                .find_map(|(i, (_, j))| match (j.exclusive.is_some(), i) {
                    (true, 0) => Some(0),
                    (true, _) => Some(usize::MAX),
                    _ => adm.fits(j).then_some(i),
                })
                .filter(|p| *p != usize::MAX);
            let Some(pos) = pos else {
                wait_one(&mut adm);
                continue;
            };
            let (idx, job) = pending.remove(pos);
            if let Some(reason) = &job.exclusive {
                while adm.running() > 0 {
                    wait_one(&mut adm);
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
            if sh.past_deadline() {
                outcome.deadline_hit = true;
                break;
            }
            if last_alive.is_none_or(|t| t.elapsed() >= sh.cfg.liveness_every) {
                if let Liveness::Dead { detail } = sh.session.liveness() {
                    (sh.hooks.log)(&format!(
                        "container died before suite {} — remaining suites will be unreached",
                        job.name
                    ));
                    outcome.container_dead = Some(detail);
                    break;
                }
                last_alive = Some(Instant::now());
            }
            if let Some(d) = sh.account_fault.lock().unwrap().clone() {
                (sh.hooks.log)(&format!(
                    "harness fault — {d} — remaining suites will be unreached"
                ));
                outcome.account_fault = Some(d);
                break;
            }
            let done = DoneOnDrop(tx.clone(), (job.weight, job.lane.clone()));
            let name = job.name.clone();
            scope.spawn(move || {
                let _done = done;
                sh.session.make_home(n);
                let (rc, secs, output, _running) = sh.exec_suite(n, &name);
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
            adm.admit(job);
            if job.exclusive.is_some() {
                while adm.running() > 0 {
                    wait_one(&mut adm);
                }
            }
        }
        while adm.running() > 0 {
            wait_one(&mut adm);
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
            pids_budget: crate::schedule::pids_budget(),
            lane_slots: crate::schedule::LANE_SLOTS,
            exec_fault_threshold: 5,
            quarantined: BTreeSet::new(),
            psi_pause: Duration::from_millis(1),
            liveness_every: Duration::ZERO,
            deadline: None,
            embedded_only: BTreeSet::new(),
            resume: None,
        }
    }

    fn jobs(names: &[&str]) -> Vec<Job> {
        names
            .iter()
            .map(|n| Job::new(n, None))
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
        let mut s = Session::new(rt, "i1", "dwarf");
        s.liveness_sleep = Duration::from_millis(1);
        s
    }

    #[test_id(tal286u)]
    #[test]
    // covers: UC-test-infrastructure-30
    fn parallel_records_every_outcome_and_writes_out_before_result() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok 1 - a\n");
        rt.suite("test-b.sh", 1, "not ok 1 - b broke\nFAIL b\n");
        rt.suite("test-c.sh", 77, "1..0 # SKIP not applicable here\n");
        rt.suite("test-d.sh", 124, "slow\n");
        let dir = tmpdir("par");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 2);
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
            assert!(seen
                .lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.trim() == "FAIL b"));
            assert!(seen
                .lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("  test-b.sh") && l.ends_with("— first red: case 1 b broke")));
            o
        });
        assert!(!out.harness_fault());
        assert_eq!(out.records["test-a.sh"].status, Status::Ok);
        assert_eq!(out.records["test-b.sh"].status, Status::Red);
        assert_eq!(out.records["test-c.sh"].status, Status::Red);
        assert_eq!(out.records["test-d.sh"].status, Status::Timeout);
        assert_eq!(out.blocking_reds(), vec!["test-b.sh", "test-c.sh", "test-d.sh"]);
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

    /// sp-3azqi: a suite whose podman exec lost its exit status (rc 255, the exact
    /// conmon exit-file-wait message, full passing TAP output ahead of it — exactly the
    /// shape the VM certification run hit for test-strand-reclaim-n.sh) records as Fault,
    /// never Red, and never blocks or quarantines like an ordinary suite defect would.
    #[test_id(tuvinxg)]
    #[test]
    fn podman_exec_wait_timeout_records_fault_not_red() {
        let rt = FakeRuntime::new();
        rt.suite(
            "test-strand-reclaim-n.sh",
            255,
            "TAP version 14\n1..4\nok 1 - a\nok 2 - b\nok 3 - c\nok 4 - d\n\n4 passed, 0 failed, 0 skipped\nError: timed out waiting for file /var/lib/containers/storage/overlay-containers/d609b04df0f16dfeda8af8adf28901c5d60de417ee573f9a66bfadf26b14e4f6/userdata/341cfdc06753bbf5d930ce5f9764fe6b5925ed6791e5bccfcb18cc41dc1d50ed/exit/d609b04df0f16dfeda8af8adf28901c5d60de417ee573f9a66bfadf26b14e4f6\n",
        );
        rt.suite("test-a.sh", 0, "ok\n");
        let dir = tmpdir("exec-lost");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 2);
        let out = with_hooks(|h, _| {
            run(
                &s,
                &c,
                h,
                &Fixtures::PerSuite,
                &jobs(&["test-strand-reclaim-n.sh", "test-a.sh"]),
            )
        });
        assert!(!out.harness_fault(), "a single lost exec is not a batch-wide harness fault");
        assert_eq!(
            out.records["test-strand-reclaim-n.sh"].status,
            Status::Fault
        );
        assert_eq!(out.faulted(), vec!["test-strand-reclaim-n.sh"]);
        assert!(out.blocking_reds().is_empty(), "a fault must never read as a red");
        assert!(out.quarantined_reds().is_empty());
        let res = fs::read_to_string(dir.join("test-strand-reclaim-n.sh.result")).unwrap();
        assert!(res.starts_with("fault "));
        assert!(res.trim_end().ends_with(" parallel explicit 255"));
    }

    #[test_id(ttvgdah)]
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

    #[test_id(t89ldp6)]
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
                    .is_some_and(|a| a.starts_with(&format!("{}/spira/test-", crate::fixture::WORKSPACE)))
            },
            move |r| {
                let now = l2.fetch_add(1, Ordering::SeqCst) + 1;
                if r.argv[1].ends_with("test-x.sh") {
                    e2.store(now, Ordering::SeqCst);
                }
                // clock-safe: the fake runner simulates a call of fixed length; the RC_DEADLINE it returns past r.deadline is what the test asserts
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

    #[test_id(tt5nwap)]
    #[test]
    fn a_suite_red_on_enospc_is_a_harness_fault_not_a_red() {
        let rt = FakeRuntime::new();
        rt.suite(
            "test-a.sh",
            1,
            "not ok 1 - seed\n/workspace/spira/testlib.sh: line 310: printf: write error: No space left on device\n",
        );
        rt.suite("test-b.sh", 1, "not ok 1 - real defect\n");
        let dir = tmpdir("enospc");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 2);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert!(out.harness_fault());
        assert!(out.disk_fault.as_deref().is_some_and(|d| d.contains("test-a.sh")));
        let dir = tmpdir("enospc-control");
        let c = cfg(Mode::Parallel, &dir, 2);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-b.sh"])));
        assert!(!out.harness_fault(), "an ordinary red is not a disk fault");
        assert_eq!(out.blocking_reds(), vec!["test-b.sh"]);
    }

    #[test_id(tjmil5q)]
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

    #[test_id(tceyouw)]
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
        assert_eq!(e[0].env_value("SPIRA_RUN"), Some(format!("{}/spira-batch-i1", system_paths::TMP).as_str()));
    }

    #[test_id(t70m6zb)]
    #[test]
    // covers: UC-test-infrastructure-30
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

    fn numbered(suite: &str) -> String {
        (1..=100).map(|n| format!("{suite} line {n}\n")).collect()
    }

    /// Answers each suite with 100 lines, limiting the cap's view to the exec's own thread.
    fn chatty_suites(rt: &FakeRuntime, suites: &[&'static str]) {
        for &suite in suites {
            rt.on_suite(suite, move |_| {
                crate::cap::ONLY_MY_UNITS.with(|o| o.set(true));
                ExecOutcome { rc: 0, output: numbered(suite) }
            });
        }
    }

    fn assert_cut_names_log_and_tail(cut: &(String, Vec<String>), dir: &Path, suite: &str) {
        let (what, report) = cut;
        assert_eq!(what, &format!("tier/suite {suite} (in exec)"));
        let log = crate::cap::log_line(suite, &dir.join(format!(".{suite}.raw")));
        assert!(report.contains(&log), "{report:?}");
        let head = format!("  last 60 lines ({suite}):");
        let at = report.iter().position(|l| *l == head).expect("a tail");
        let want: Vec<String> = (41..=100).map(|n| format!("    {suite} line {n}")).collect();
        assert_eq!(report[at + 1..at + 61], want[..]);
    }

    #[test_id(twgc547)]
    #[test]
    fn a_suite_cut_in_its_exec_is_reported_with_its_step_log_and_tail() {
        crate::cap::ONLY_MY_UNITS.with(|o| o.set(true));
        let rt = FakeRuntime::new();
        chatty_suites(&rt, &["test-a.sh"]);
        let dir = tmpdir("cut-serial");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert_cut_names_log_and_tail(&rt.cut_here.lock().unwrap()[0], &dir, "test-a.sh");
    }

    #[test_id(tf8b9mb)]
    #[test]
    fn each_parallel_suite_cut_in_its_exec_names_its_own_log() {
        let rt = FakeRuntime::new();
        chatty_suites(&rt, &["test-a.sh", "test-b.sh"]);
        let dir = tmpdir("cut-par");
        let s = session(&rt);
        let c = cfg(Mode::Parallel, &dir, 2);
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh", "test-b.sh"])));
        let mut cuts = rt.cut_here.lock().unwrap().clone();
        cuts.sort();
        assert_eq!(cuts.len(), 2);
        assert_cut_names_log_and_tail(&cuts[0], &dir, "test-a.sh");
        assert_cut_names_log_and_tail(&cuts[1], &dir, "test-b.sh");
    }

    #[test_id(t2pjdxj)]
    #[test]
    fn the_raw_log_outlives_only_a_suite_that_left_no_record() {
        let token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::runtime::LOCAL_CANCEL.with(|l| *l.borrow_mut() = Some(token.clone()));
        let rt = FakeRuntime::new();
        rt.on_suite("test-a.sh", move |_| {
            token.store(true, Ordering::SeqCst);
            ExecOutcome { rc: crate::runtime::RC_CANCELLED, output: "half a run\n".into() }
        });
        let dir = tmpdir("raw-cut");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        crate::runtime::LOCAL_CANCEL.with(|l| *l.borrow_mut() = None);
        assert!(out.cancelled && out.records.is_empty());
        assert_eq!(fs::read_to_string(dir.join(".test-a.sh.raw")).unwrap(), "half a run\n");

        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        let dir = tmpdir("raw-done");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert!(dir.join("test-a.sh.out").exists());
        assert!(!dir.join(".test-a.sh.raw").exists());

        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", crate::runtime::RC_DEADLINE, "partial\n");
        let dir = tmpdir("raw-deferred");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert!(dir.join("test-a.sh.out").exists());
        assert!(!dir.join(".test-a.sh.raw").exists());
    }

    #[test_id(tdf90a4)]
    #[test]
    fn the_raw_log_stays_when_out_could_not_be_written() {
        let rt = FakeRuntime::new();
        rt.suite("test-a.sh", 0, "ok\n");
        let dir = tmpdir("raw-no-out");
        fs::create_dir(dir.join("test-a.sh.out")).unwrap();
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        assert_eq!(fs::read_to_string(dir.join(".test-a.sh.raw")).unwrap(), "ok\n");
    }

    #[test_id(tfgj6g3)]
    #[test]
    fn a_log_the_cap_may_have_named_is_kept_after_the_suite_is_recorded() {
        let armed = Arc::new(Mutex::new(None));
        let slot = armed.clone();
        let rt = FakeRuntime::new();
        rt.on_suite("test-a.sh", move |_| {
            let cap = crate::cap::arm(crate::cap::Cap { secs: 0, source: "test" }, None);
            cap.await_trip();
            *slot.lock().unwrap() = Some(cap);
            ExecOutcome { rc: 0, output: "ok\n".into() }
        });
        let dir = tmpdir("raw-tripped");
        let s = session(&rt);
        let c = cfg(Mode::Serial, &dir, 0);
        let out = with_hooks(|h, _| run(&s, &c, h, &Fixtures::PerSuite, &jobs(&["test-a.sh"])));
        let cap = armed.lock().unwrap().take().expect("armed");
        let trip = cap.finish().expect("tripped");
        assert!(out.records.contains_key("test-a.sh"));
        let raw = dir.join(".test-a.sh.raw");
        assert!(trip.report.contains(&crate::cap::log_line("test-a.sh", &raw)), "{:?}", trip.report);
        assert!(raw.exists());
    }

    /// The first exec of `test-a.sh` answers `first`, every later one a pass; returns what
    /// a cap tripping while each record was being reported would have named. Each report
    /// must carry the log, by then `.out`, and its last line.
    fn cut_while_recording(mode: Mode, first: (i32, &'static str)) -> Vec<String> {
        let rt = FakeRuntime::new();
        let calls = AtomicUsize::new(0);
        rt.on_suite("test-a.sh", move |_| {
            crate::cap::ONLY_MY_UNITS.with(|o| o.set(true));
            let (rc, output) = match calls.fetch_add(1, Ordering::SeqCst) {
                0 => first,
                _ => (0, "ok\n"),
            };
            ExecOutcome { rc, output: output.into() }
        });
        let dir = tmpdir("cut-record");
        let s = session(&rt);
        let c = cfg(mode, &dir, 1);
        let seen = Mutex::new(Vec::new());
        let line = |l: &str| {
            if l.trim_start().starts_with("test-a.sh") {
                let what = crate::cap::describe();
                let report = crate::cap::trip_report(&what, 1, "test");
                let out = dir.join("test-a.sh.out");
                assert!(report.contains(&crate::cap::log_line("test-a.sh", &out)), "{report:?}");
                let last = fs::read_to_string(&out).unwrap();
                let last = format!("    {}", last.lines().last().unwrap());
                assert_eq!(report[report.len() - 2], last, "{report:?}");
                seen.lock().unwrap().push(what);
            }
        };
        let hooks = Hooks { log: &|_| {}, line: &line, timing: &|_, _, _, _, _| {}, psi_high: &|| false };
        run(&s, &c, &hooks, &Fixtures::PerSuite, &jobs(&["test-a.sh"]));
        seen.into_inner().unwrap()
    }

    #[test_id(tjwlmlx)]
    #[test]
    fn a_suite_stays_in_the_cap_report_while_its_record_is_written() {
        let step = |rc: i32| format!("tier/suite test-a.sh (in record, exec rc={rc} after 0s)");
        for mode in [Mode::Serial, Mode::Parallel] {
            assert_eq!(cut_while_recording(mode, (0, "ok\n")), vec![step(0)]);
            let lost = "Error: timed out waiting for file /x/userdata/y/exit/z\n";
            assert_eq!(cut_while_recording(mode, (255, lost)), vec![step(255), step(0)]);
        }
    }

    /// Suites that take `ms` each but honour the exec's deadline the way `run_bounded` does:
    /// killed at the deadline with RC_DEADLINE and their partial output.
    fn timed_suites(rt: &FakeRuntime, times: &[(&str, u64, i32)]) {
        let table: Vec<(String, u64, i32)> = times
            .iter()
            .map(|(n, ms, rc)| (format!("{}/spira/{n}", crate::fixture::WORKSPACE), *ms, *rc))
            .collect();
        rt.on(
            |r| {
                r.argv
                    .get(1)
                    .is_some_and(|a| a.starts_with(&format!("{}/spira/test-", crate::fixture::WORKSPACE)))
            },
            move |r| {
                let (_, ms, rc) = table
                    .iter()
                    .find(|(p, _, _)| Some(p) == r.argv.get(1))
                    .cloned()
                    .unwrap_or_default();
                let end = Instant::now() + Duration::from_millis(ms);
                loop {
                    // clock-safe: the fake runner simulates a call of fixed length; the RC_DEADLINE it returns past r.deadline is what the test asserts
                    if r.deadline.is_some_and(|d| Instant::now() >= d) {
                        return ExecOutcome {
                            rc: crate::runtime::RC_DEADLINE,
                            output: "partial\n".into(),
                        };
                    }
                    // clock-safe: the fake runner simulates a call of fixed length; the RC_DEADLINE it returns past r.deadline is what the test asserts
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
                    // clock-safe: the fake runner simulates a call of fixed length; the RC_DEADLINE it returns past r.deadline is what the test asserts
                    std::thread::sleep(Duration::from_millis(5));
                }
            },
        );
    }

    #[test_id(te6ykvk)]
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

#[cfg(test)]
mod vtime_tests {
    use super::*;


    #[test_id(ttl2uvx)]
    #[test]
    fn now_epoch_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, now_epoch);
        assert_eq!(got, 1_900_000_000);
    }
}
