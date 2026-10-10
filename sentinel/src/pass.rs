//! The pass: its bookkeeping (acted/progressed, phases, temp files) and the order the
//! checks run in for each entry point (DESIGN.md §4).

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cfg::{Cfg, Context, Declared};
use crate::host::{Host, Io, Out, Spec};
use crate::seams;
use crate::store::{self, Bd, Snapshot};
use crate::temps;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Pass,
    Report,
    SummonOnly,
    Audit,
    /// CHECK 3c alone; `dry` prints the decision and writes nothing.
    OpenChildren { dry: bool },
    /// `land_escalate` alone, stdin `<why>\n<evidence>` (sp-31hjr): the real-sender
    /// suites' way to drive the native escalation without a whole pass.
    LandEscalate,
    /// `summon_fayth` alone (wave 4.27, family G): lib.sh's own shim target, and
    /// czar-pass's direct call — no lib.sh sourcing at all any more.
    Summon { fayth: String, pool: Option<i64>, require_express: bool },
    /// `summon_argv` alone: `aeon --escape`'s own seam reaches it through lib.sh's shim.
    SummonArgv { fayth: String },
    /// `world_gate` alone: ditto.
    WorldGate { fayth: String, prefix: String },
    /// `named_unit_stop` alone: `acceptance-local.sh`'s own shim target.
    NamedUnitStop { glob: String },
    /// `ck7_summon_pass` alone, under its own flock — lib.sh's shim target, and the way a
    /// bash fixture now proves the lock serializes two real contenders without a database.
    SummonPass,
    /// lib.sh `mark_queue_waiters`'s shim target (wave 4.28, sp-fbqsv).
    MarkQueueWaiters,
    /// lib.sh `close_landed_queue_waiters`'s shim target (wave 4.28, sp-fbqsv).
    CloseLandedQueueWaiters,
    /// lib.sh `detect_unclaimable_ready`'s shim target (wave 4.28, sp-fbqsv).
    DetectUnclaimable,
    /// lib.sh `file_unclaimable_incidents`'s shim target; stdin is detect's output
    /// (wave 4.28, sp-fbqsv).
    FileUnclaimable,
    /// lib.sh `detect_branch_collisions`'s shim target (wave 4.28, sp-fbqsv).
    DetectCollisions,
    /// lib.sh `park_branch_collisions`'s shim target; stdin is detect's output
    /// (wave 4.28, sp-fbqsv).
    ParkCollisions,
}

impl Mode {
    /// Only the first argument is read, exactly as `[ "${1:-}" = --x ]` did.
    pub fn from_first(a: Option<&str>) -> Mode {
        match a {
            Some("--report") => Mode::Report,
            Some("--summon-only") => Mode::SummonOnly,
            Some("--audit") => Mode::Audit,
            Some("--open-children") => Mode::OpenChildren { dry: false },
            Some("--land-escalate") => Mode::LandEscalate,
            Some("--mark-queue-waiters") => Mode::MarkQueueWaiters,
            Some("--close-landed-queue-waiters") => Mode::CloseLandedQueueWaiters,
            Some("--detect-unclaimable") => Mode::DetectUnclaimable,
            Some("--file-unclaimable") => Mode::FileUnclaimable,
            Some("--detect-collisions") => Mode::DetectCollisions,
            Some("--park-collisions") => Mode::ParkCollisions,
            _ => Mode::Pass,
        }
    }

    /// The only mode with a second argument: `--open-children --dry-run`.
    pub fn from_args(a: Option<&str>, b: Option<&str>) -> Mode {
        match (Mode::from_first(a), b) {
            (Mode::OpenChildren { .. }, Some("--dry-run")) => Mode::OpenChildren { dry: true },
            (m, _) => m,
        }
    }

    /// The full argv (sans argv[0]) — the only modes that take more than a flag and an
    /// optional `--dry-run` (`--summon`, `--summon-argv`, `--world-gate`,
    /// `--named-unit-stop`) are parsed here; everything else falls back to
    /// [`Mode::from_args`] unchanged.
    pub fn from_argv(args: &[String]) -> Mode {
        let a0 = args.first().map(String::as_str);
        match a0 {
            Some("--summon") => Mode::Summon {
                fayth: args.get(1).cloned().unwrap_or_default(),
                pool: args.get(2).filter(|s| !s.is_empty()).and_then(|s| s.parse().ok()),
                require_express: args.get(3).is_some_and(|s| !s.is_empty()),
            },
            Some("--summon-argv") => Mode::SummonArgv { fayth: args.get(1).cloned().unwrap_or_default() },
            Some("--world-gate") => Mode::WorldGate {
                fayth: args.get(1).cloned().unwrap_or_default(),
                prefix: args.get(2).cloned().unwrap_or_default(),
            },
            Some("--named-unit-stop") => Mode::NamedUnitStop { glob: args.get(1).cloned().unwrap_or_default() },
            Some("--summon-pass") => Mode::SummonPass,
            _ => Mode::from_args(a0, args.get(1).map(String::as_str)),
        }
    }
}

pub struct Sentinel<'a> {
    pub h: &'a Host<'a>,
    pub cfg: Cfg,
    pub ctx: Context,
    pub mode: Mode,
    pub acted: Cell<u32>,
    pub progressed: Cell<u32>,
    /// The seams' act/progress mailbox (DESIGN.md §6).
    pub tally_file: PathBuf,
    pub lib: PathBuf,
    /// This executable, for the audit worker's own command line.
    pub exe: String,
    pub pass_id: String,
    phase: RefCell<Option<(String, i64)>>,
    pub started: i64,
    /// The machine could not be read or written this pass → exit 1.
    pub lc_failed: Cell<bool>,
    /// The pass's one `spira-lc list` (lc_rows), shared by CHECK 7's ready cache and
    /// CHECK 2/2c — one lifecycle read per pass.
    pub lc_memo: RefCell<Option<Option<Vec<crate::model::LcRow>>>>,
}

impl<'a> Sentinel<'a> {
    pub fn new(
        h: &'a Host<'a>,
        ctx: Context,
        declared: Declared,
        home: &Path,
        mode: Mode,
        exe: String,
        pass_id: String,
    ) -> Sentinel<'a> {
        let cfg = Cfg::from_context(&ctx, home, declared);
        let tally_file = cfg
            .run
            .join(format!(".sentinel-tally.{}", std::process::id()));
        let lib = cfg.home.join("lib.sh");
        let started = h.now();
        Sentinel {
            h,
            cfg,
            ctx,
            mode,
            acted: Cell::new(0),
            progressed: Cell::new(0),
            tally_file,
            lib,
            exe,
            pass_id,
            phase: RefCell::new(None),
            started,
            lc_failed: Cell::new(false),
            lc_memo: RefCell::new(None),
        }
    }

    pub fn log(&self, m: &str) {
        self.h.log(m);
    }

    pub fn bd(&self) -> Bd<'_> {
        Bd { cfg: &self.cfg }
    }

    /// `act`: a write happened.
    pub fn act(&self, msg: &str) {
        self.acted.set(self.acted.get() + 1);
        self.log(&format!("ACT {msg}"));
    }

    /// `progress`: the DAG moved. In the audit worker the line also goes to the mailbox the
    /// next normal pass drains, because `progressed` is per-process and CHECK 8 must see it.
    pub fn progress(&self, msg: &str) {
        self.progressed.set(self.progressed.get() + 1);
        self.act(msg);
        if self.mode == Mode::Audit {
            append_line(&self.cfg.audit_mailbox, msg);
        }
    }

    /// `_phase <name>`: flush the check that just ended as a tsd row, start the next clock.
    /// The audit worker never had `_phase` defined, so it writes no rows (B5).
    pub fn phase(&self, name: &str) {
        if self.mode != Mode::Pass {
            return;
        }
        let now = self.h.now();
        let prev = self.phase.borrow_mut().replace((name.to_string(), now));
        let (pname, t0) = prev.unwrap_or_else(|| ("setup".to_string(), self.started));
        {
            let bin = &self.cfg.tsd_bin;
            let root = self.cfg.run.to_string_lossy().into_owned();
            let _ = self.h.run(
                Spec::args_owned(
                    bin.clone(),
                    vec![
                        "--family".into(),
                        "sentinel-phase".into(),
                        "--root".into(),
                        root,
                        "--field-str".into(),
                        format!("pass={}", self.pass_id),
                        "--field-str".into(),
                        format!("check={pname}"),
                        "--field".into(),
                        format!("secs={}", now - t0),
                    ],
                )
                .out(Io::Null)
                .err(Io::Null),
            );
        }
    }

    /// Run one lib.sh seam (DESIGN.md §6). `count` adds its act/progress tally to this
    /// pass's counters — false for the seams the old script ran inside `$(…)`, whose
    /// counter increments never survived the subshell.
    pub fn seam(
        &self,
        name: &str,
        body: &str,
        stdin: Option<Vec<u8>>,
        out: Io,
        err: Io,
        count: bool,
    ) -> Out {
        let _ = std::fs::write(&self.tally_file, "");
        let mut s = Spec::args_owned(
            "bash",
            vec!["-c".into(), seams::script(body), format!("sentinel-{name}")],
        )
        .env("SENTINEL_LIB", self.lib.to_string_lossy())
        .env("SENTINEL_TALLY", self.tally_file.to_string_lossy())
        .out(out)
        .err(err);
        if let Some(b) = stdin {
            s = s.stdin(b);
        }
        let o = self.h.run(s);
        if o.rc == 97 {
            self.log(&format!(
                "WARN seam {name}: lib.sh did not load (rc 97) — this step did not run"
            ));
        }
        let tally = std::fs::read_to_string(&self.tally_file).unwrap_or_default();
        let _ = std::fs::remove_file(&self.tally_file);
        if count {
            for line in tally.lines() {
                match line.split_once('\t') {
                    Some(("act", _)) => self.acted.set(self.acted.get() + 1),
                    Some(("progress", m)) => {
                        self.acted.set(self.acted.get() + 1);
                        self.progressed.set(self.progressed.get() + 1);
                        if self.mode == Mode::Audit {
                            append_line(&self.cfg.audit_mailbox, m);
                        }
                    }
                    _ => {}
                }
            }
        }
        o
    }

    /// `[ -x mail ] && mail send concierge --from … --subject … --kind question
    /// --default … <<body` → true only when the ask was accepted.
    pub fn mail(
        &self,
        from: &str,
        subject: &str,
        default: &str,
        body: &str,
        bead: &str,
        own_dedup: bool,
    ) -> bool {
        let mut s = Spec::args_owned(
            "mail",
            vec![
                "send".into(),
                "concierge".into(),
                "--from".into(),
                from.into(),
                "--subject".into(),
                subject.into(),
                "--kind".into(),
                "question".into(),
                "--default".into(),
                default.into(),
            ],
        )
        .stdin(body.as_bytes().to_vec())
        .out(Io::Null)
        .err(Io::Null);
        if !bead.is_empty() {
            s.args.push("--bead".into());
            s.args.push(bead.into());
        }
        if own_dedup {
            s = s.env("SPIRA_MAIL_REPEAT_CONSIDERED", "sentinel-own-dedup");
        }
        self.h.run(s).ok()
    }

    /// lib.sh `ask_already_open <subject>` — ported natively (sp-31hjr). True when an
    /// OPEN ask already carries `subject` in its title. THE STRONGEST DEDUPE IS "IS IT
    /// ALREADY IN FRONT OF HIM": the database is the queue, so this asks the database
    /// rather than a clock or a stamp file; a closed ask does NOT suppress a new one —
    /// a condition recurring after an answer is new information.
    pub fn ask_already_open(&self, subject: &str) -> bool {
        if subject.is_empty() {
            return false;
        }
        // An ask is not a work bead: its bd status is its only state (spira_config::nonwork).
        let [status_flag, open] = spira_config::nonwork::status_args(spira_config::nonwork::Kind::Ask, spira_config::nonwork::Which::Open);
        let out = self.bd().call_owned(
            self.h,
            &[
                "list".into(),
                status_flag,
                open,
                "--label".into(),
                self.cfg.ask.clone(),
                "--limit".into(),
                "0".into(),
                "--json".into(),
            ],
            None,
        );
        if !out.ok() {
            return false;
        }
        let js = crate::model::json_only(&out.stdout);
        crate::model::parse_beads(js)
            .map(|rows| rows.iter().any(|b| b.title.as_deref().unwrap_or("").contains(subject)))
            .unwrap_or(false)
    }

    pub fn git(&self, repo: &str, args: &[&str]) -> Out {
        let mut a = vec!["-C".to_string(), repo.to_string()];
        a.extend(args.iter().map(|s| s.to_string()));
        self.h.run(Spec::args_owned("git", a).err(Io::Null))
    }

    pub fn script(&self, name: &str) -> PathBuf {
        self.cfg.home.join(name)
    }

    /// A temp file under $SPIRA_RUN that is removed however this process exits (G8).
    pub fn temp_file(&self, stem: &str, body: &str) -> Option<PathBuf> {
        let p = temps::create(&self.cfg.run, stem)?;
        if std::fs::write(&p, body).is_err() {
            temps::remove(&p);
            return None;
        }
        Some(p)
    }

    // -----------------------------------------------------------------------------------
    // The entry points.

    pub fn run(&self) -> i32 {
        match &self.mode {
            Mode::SummonOnly => self.summon_only(),
            Mode::OpenChildren { dry } => self.open_children_only(*dry),
            Mode::LandEscalate => self.land_escalate_cmd(),
            Mode::Summon { fayth, pool, require_express } => self.summon_cmd(fayth, *pool, *require_express),
            Mode::SummonArgv { fayth } => self.summon_argv_cmd(fayth),
            Mode::WorldGate { fayth, prefix } => self.world_gate_cmd(fayth, prefix),
            Mode::NamedUnitStop { glob } => self.named_unit_stop_cmd(glob),
            Mode::SummonPass => self.ck7_summon_pass(),
            Mode::MarkQueueWaiters => {
                self.mark_queue_waiters(None);
                0
            }
            Mode::CloseLandedQueueWaiters => {
                self.close_landed_queue_waiters();
                0
            }
            Mode::DetectUnclaimable => {
                self.h.print(&self.detect_unclaimable_ready(None));
                0
            }
            Mode::FileUnclaimable => {
                self.file_unclaimable_incidents(&read_stdin());
                0
            }
            Mode::DetectCollisions => {
                let cs = self.detect_branch_collisions();
                let text: Vec<String> = cs.iter().map(crate::detect::Collision::line).collect();
                self.h.print(&text.join("\n"));
                0
            }
            Mode::ParkCollisions => {
                let input = read_stdin();
                let cs: Vec<crate::detect::Collision> =
                    input.lines().filter_map(crate::detect::Collision::parse_line).collect();
                let outs = self.park_branch_collisions(&cs);
                let text: Vec<String> = outs.iter().map(crate::detect::ParkOutcome::line).collect();
                self.h.print(&text.join("\n"));
                0
            }
            _ => self.full(),
        }
    }

    /// `sentinel --open-children`: one snapshot read, CHECK 3c over it, nothing else.
    fn open_children_only(&self, dry: bool) -> i32 {
        let r = store::bulk(&self.bd(), self.h, &store::ready_raw_args(&self.cfg));
        let (list_raw, list) = match r.list {
            Ok(x) => x,
            Err(e) => {
                self.h
                    .log_err(&format!("mark_open_children: the store snapshot could not be read ({e})"));
                return 1;
            }
        };
        // A child's state is its lifecycle row (design §3.4): the one `spira-lc list`.
        let snap = Snapshot::new(list_raw, list, r.ready.ok()).with_lc(self.state_rows().as_deref());
        self.mark_open_children(&snap, dry);
        0
    }

    /// The bulk reads and the DB check (every mode but --summon-only).
    fn read_store(&self) -> Result<Snapshot, i32> {
        let bd = self.bd();
        let r = store::bulk(&bd, self.h, &store::ready_raw_args(&self.cfg));
        let (list_raw, list) = match r.list {
            Ok(x) => x,
            Err(e) => {
                if !self.cfg.skip_reclaim {
                    self.log(&format!(
                        "DATABASE UNREADABLE — bd cannot reach {}; state is unknown and this pass cannot close any gap",
                        self.cfg.db
                    ));
                    return Err(1);
                }
                self.h.log_err(&format!("WARN the store snapshot could not be read ({e}); checks fall back to their own queries"));
                (String::new(), Vec::new())
            }
        };
        let ready = match r.ready {
            Ok(x) => Some(x),
            Err(e) => {
                self.h.log_err(&format!("WARN the ready snapshot could not be read ({e}); plan_ready is unknown this pass"));
                None
            }
        };
        // Every state decision over this snapshot reads the bead's lifecycle row, never bd
        // status (design §3.4, sp-mve9i): the pass's one `spira-lc list`, read whatever
        // fail-closed (`state_rows`).
        Ok(Snapshot::new(list_raw, list, ready).with_lc(self.state_rows().as_deref()))
    }

    /// Write the snapshots (and the ready cache) where every child reads them.
    fn export_snapshot(&self, snap: &Snapshot) {
        if !snap.list_raw.is_empty() {
            if let Some(p) = self.temp_file("list-snapshot", &snap.list_raw) {
                self.h.set_env("SPIRA_LIST_SNAPSHOT", &p.to_string_lossy());
            }
        }
        if snap.ready.is_some() && self.mode == Mode::Pass {
            self.export_ready_cache();
        }
    }

    fn full(&self) -> i32 {
        let snap = match self.read_store() {
            Ok(s) => s,
            Err(rc) => return rc,
        };
        if self.mode == Mode::Audit {
            self.export_snapshot(&snap);
            return self.audit(&snap);
        }

        // STATE — the open plan backlog. There is no goal epic: Spira works the whole
        // backlog continuously (sp-2f9sa, sp-k6m1m).
        let (open_plan, plan_ready, plan_inprog) = if self.cfg.skip_reclaim {
            (Vec::new(), Some(0), 0)
        } else {
            // plan_ready is spira-claim's ready set (sp-7g5q6); open and in_progress read the
            // snapshot's lifecycle rows (sp-mve9i).
            let ready = self.plan_ready_live();
            (snap.plan_open(&self.cfg), ready, snap.plan_inprog(&self.cfg))
        };
        let n_open = open_plan.len();
        let live = self.live_total();
        self.log(&format!(
            "state: open={} plan_ready={} in_progress={} aeons={} fayths=[{}]",
            n_open,
            plan_ready
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into()),
            plan_inprog,
            live,
            self.cfg.fayths_str
        ));
        self.roster_warnings();
        if self.mode == Mode::Report {
            self.h.print("\nOpen plan beads:");
            for id in &open_plan {
                self.h.print(&format!("  {id}"));
            }
            return 0;
        }

        self.export_snapshot(&snap);

        self.phase("CHECK1");
        self.check1();

        self.phase("CHECK2");
        let lc_rows = if self.cfg.skip_reclaim { None } else { self.lc_rows() };
        if let Some(rows) = &lc_rows {
            self.check2(&snap, rows);
        }
        self.phase("CHECK2b");
        self.check2b();
        self.phase("CHECK2c");
        if let Some(rows) = &lc_rows {
            self.check2c(rows);
        }
        self.phase("CHECK2d");
        if let Some(rows) = &lc_rows {
            self.check2d(&snap, rows);
        }
        self.phase("CHECK3");
        let plan_ready = self.check3(plan_ready, plan_inprog, n_open);

        self.audit_dispatch();

        self.phase("CHECK6");
        self.check6();
        self.phase("CHECK3b");
        // S4 is retired (wave 4.28, sp-fbqsv): mark_queue_waiters/close_landed_queue_waiters
        // are native now, reading the pass's own broad ready snapshot already in memory.
        self.mark_queue_waiters(snap.ready.as_deref());
        self.close_landed_queue_waiters();
        // CHECK 3c from the snapshot: one walk in memory, never one `bd children` per
        // candidate (sp-du8bv: that loop was 91% of every pass).
        self.phase("CHECK3c");
        self.mark_open_children(&snap, false);
        self.phase("CHECK7");
        self.ck7_summon_pass();

        self.phase("CHECK8");
        self.check8(plan_ready, plan_inprog, n_open, &open_plan);
        self.phase("end");
        self.log(&format!(
            "pass complete — {} action(s), {} progress",
            self.acted.get(),
            self.progressed.get()
        ));
        self.budget_check();
        self.exit_code()
    }

    /// A machine that could not be read or written fails the unit, every pass,
    /// until it is fixed — after the rest of the pass (landing, summoning) has run.
    fn exit_code(&self) -> i32 {
        i32::from(self.lc_failed.get())
    }

    fn audit(&self, snap: &Snapshot) -> i32 {
        self.check4(snap);
        if let Some(rows) = self.lc_rows() {
            self.check_rowless(snap, &rows);
        }
        self.check6b();
        if !self.cfg.skip_reclaim {
            self.check7c(snap);
            self.check7d();
        }
        let _ = std::fs::write(
            self.cfg.run.join("audit.status"),
            format!("SP_AUDIT_AT={}\nSP_AUDIT_RC=0\n", self.h.now()),
        );
        self.log(&format!(
            "audit pass complete — {} action(s), {} progress",
            self.acted.get(),
            self.progressed.get()
        ));
        self.exit_code()
    }

    /// B7: the positive control on the pass-time budget (DESIGN.md §5).
    fn budget_check(&self) {
        let took = self.h.now() - self.started;
        if took > self.cfg.pass_target {
            self.log(&format!(
                "WARN pass took {took}s — over the {}s pass budget (SPIRA_SENTINEL_PASS_TARGET_SECS)",
                self.cfg.pass_target
            ));
        }
    }

    // -----------------------------------------------------------------------------------

    /// CHECK 1 — completed pilgrimages.
    fn check1(&self) {
        let o = self.h.run(Spec::args_owned(
            self.script("pilgrimage.sh").to_string_lossy().into_owned(),
            vec!["check".into()],
        ));
        let text = format!("{}{}", o.stdout, o.stderr);
        if !text.is_empty() {
            self.h.print(&text);
        }
        let n = text
            .lines()
            .filter(|l| l.starts_with("PILGRIMAGE COMPLETE"))
            .count();
        if n > 0 {
            self.progress(&format!("announced and closed {n} completed pilgrimage(s)"));
        }
    }

    /// CHECK 2b — stranded work (the strand crate).
    fn check2b(&self) {
        let bin = self.cfg.strand_bin.clone();
        let o = self.h.run(Spec::args_owned(bin, vec!["check".into()]));
        let text = format!("{}{}", o.stdout, o.stderr);
        if !text.is_empty() {
            self.h.print(&text);
        }
        let moved = text.lines().filter(|l| l.starts_with("RECLAIMED")).count();
        let escal = text.lines().filter(|l| l.starts_with("STRANDED")).count();
        if moved > 0 {
            self.progress(&format!("handled {moved} stranded item(s)"));
        }
        if escal > 0 {
            self.act(&format!("escalated {escal} stranded item(s)"));
        }
    }

    /// CHECK 3 — stale blocked flags. Returns plan_ready, recounted when it ran.
    fn check3(
        &self,
        plan_ready: Option<usize>,
        plan_inprog: usize,
        n_open: usize,
    ) -> Option<usize> {
        if self.cfg.skip_reclaim || plan_ready != Some(0) || plan_inprog != 0 || n_open == 0 {
            return plan_ready;
        }
        self.bd().quiet(self.h, &["recompute-blocked"], None);
        let now = self.plan_ready_live().unwrap_or(0);
        self.log("recomputed is_blocked");
        if now != 0 {
            self.progress(&format!("recompute-blocked freed {now} bead(s)"));
        }
        Some(now)
    }

    /// `ready_count "<scope,>plan" "spira-poison,<ask>"`, asked live: spira-claim's
    /// `ready-count`, the one ready set (the machine's READY/REWORK rows) the summoner counts
    /// and an aeon claims from — never `bd ready`, whose status and assignee no claim writes
    /// (sp-7g5q6). A refusal is `None` (unknown), never 0.
    pub fn plan_ready_live(&self) -> Option<usize> {
        let o = self.h.run(Spec::args_owned(
            self.cfg.claim_bin.clone(),
            // No exclude labels: poison and ask are holds on the row, which the machine's own
            // claimable set already leaves out (sp-psztcc).
            vec!["ready-count".into(), self.cfg.plan_labels().join(",")],
        ));
        if o.ok() { o.stdout.trim().parse().ok() } else { None }
    }

    /// The fleet: `aeon_count` summed over the roster, from one unit listing.
    pub fn live_total(&self) -> usize {
        let names = self.ctx.fayth_names();
        if self.cfg.summon == "systemd-run" {
            let o = self.h.run(
                Spec::args_owned(
                    self.cfg.systemctl.clone(),
                    vec![
                        "--user".into(),
                        "list-units".into(),
                        "spira-aeon-*".into(),
                        "--no-legend".into(),
                    ],
                )
                .err(Io::Null),
            );
            let units: Vec<&str> = o
                .stdout
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .collect();
            return names
                .iter()
                .map(|f| {
                    units
                        .iter()
                        .filter(|u| u.starts_with(&format!("spira-aeon-{f}-")))
                        .count()
                })
                .sum();
        }
        names.iter().map(|f| pid_count(&self.cfg.run, f)).sum()
    }

    /// roster_warnings: a WARN per chamber persona SPIRA_FAYTHS leaves out, once per change.
    fn roster_warnings(&self) {
        let roster = &self.cfg.fayths_str;
        let excluded: Vec<&String> = self
            .ctx
            .chamber
            .iter()
            .filter(|f| !grep_w(roster, f))
            .collect();
        if excluded.is_empty() {
            let _ = std::fs::remove_file(&self.cfg.roster_stamp);
            return;
        }
        let mut sorted: Vec<&str> = excluded.iter().map(|s| s.as_str()).collect();
        sorted.sort();
        let stamp: String = sorted.iter().flat_map(|s| [*s, " "]).collect();
        if std::fs::read_to_string(&self.cfg.roster_stamp)
            .ok()
            .as_deref()
            == Some(stamp.as_str())
        {
            return;
        }
        let _ = std::fs::write(&self.cfg.roster_stamp, &stamp);
        for f in excluded {
            self.log(&format!("WARN {f}.fayth is in the chamber but not in SPIRA_FAYTHS — that persona will never be summoned here"));
        }
    }
}

/// `grep -qw -- word <<< text`: `word` occurs bounded by non-word characters.
pub fn grep_w(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let is_w = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(i) = text[from..].find(word) {
        let s = from + i;
        let e = s + word.len();
        let before = text[..s].chars().next_back().map_or(true, |c| !is_w(c));
        let after = text[e..].chars().next().map_or(true, |c| !is_w(c));
        if before && after {
            return true;
        }
        from = s + word.chars().next().map_or(1, |c| c.len_utf8());
    }
    false
}

/// The pidfile fallback of `aeon_count`: pidfiles whose identity lease is running; dead ones
/// are removed, as aeon_count does.
pub fn pid_count(run: &Path, fayth: &str) -> usize {
    let prefix = format!("aeon-{fayth}-");
    let Ok(rd) = std::fs::read_dir(run) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(&prefix) && name.ends_with(".pid")) {
            continue;
        }
        if sending::reap::aeon_alive(&e.path()) {
            n += 1;
        } else {
            let _ = std::fs::remove_file(e.path());
        }
    }
    n
}

/// All of stdin, for the standalone CLI modes whose shim passes along another function's
/// output (`file_unclaimable_incidents "$(cat)"`, `park_branch_collisions "$(cat)"`).
pub fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

pub fn append_line(p: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// `command -v <name>` against `path`: the first executable `<dir>/<name>`, else `name`
/// unchanged (so the spawn fails naming the tool). Resolution by PATH, never construction.
pub fn on_path(name: &str, path: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| is_exec(p))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

pub fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_argument_selects_a_mode() {
        assert_eq!(Mode::from_first(None), Mode::Pass);
        assert_eq!(Mode::from_first(Some("--report")), Mode::Report);
        assert_eq!(Mode::from_first(Some("--summon-only")), Mode::SummonOnly);
        assert_eq!(Mode::from_first(Some("--audit")), Mode::Audit);
        assert_eq!(Mode::from_first(Some("--bogus")), Mode::Pass);
        assert_eq!(
            Mode::from_args(Some("--open-children"), None),
            Mode::OpenChildren { dry: false }
        );
        assert_eq!(
            Mode::from_args(Some("--open-children"), Some("--dry-run")),
            Mode::OpenChildren { dry: true }
        );
        assert_eq!(Mode::from_args(Some("--audit"), Some("--dry-run")), Mode::Audit);
    }

    #[test]
    fn grep_w_is_word_bounded() {
        assert!(grep_w("builder ops", "ops"));
        assert!(!grep_w("builder operations", "ops"));
        assert!(grep_w("ops-x", "ops"), "grep -w treats '-' as a boundary");
        assert!(!grep_w("", "ops"));
    }
}
