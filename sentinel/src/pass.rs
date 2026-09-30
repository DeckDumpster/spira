//! The pass: its bookkeeping (acted/progressed, phases, temp files) and the order the
//! checks run in for each entry point (DESIGN.md §4).

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cfg::{Cfg, Context, Lifecycle};
use crate::host::{Host, Io, Out, Spec};
use crate::seams;
use crate::store::{self, Bd, Snapshot};
use crate::temps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pass,
    Report,
    SummonOnly,
    Audit,
}

impl Mode {
    /// Only the first argument is read, exactly as `[ "${1:-}" = --x ]` did.
    pub fn from_first(a: Option<&str>) -> Mode {
        match a {
            Some("--report") => Mode::Report,
            Some("--summon-only") => Mode::SummonOnly,
            Some("--audit") => Mode::Audit,
            _ => Mode::Pass,
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
    /// The one switch for everything that touches the lifecycle machine (DESIGN.md §2.9).
    pub lc: Lifecycle,
    /// ON mode only: the machine could not be read or written this pass → exit 1.
    pub lc_failed: Cell<bool>,
    /// ON: the pass's one `spira-lc list` (lc_rows), shared by CHECK 7's ready cache and
    /// CHECK 2/2c — one lifecycle read per pass.
    pub lc_memo: RefCell<Option<Option<Vec<crate::model::LcRow>>>>,
}

impl<'a> Sentinel<'a> {
    pub fn new(
        h: &'a Host<'a>,
        ctx: Context,
        home: &Path,
        mode: Mode,
        exe: String,
        pass_id: String,
        lc: Lifecycle,
    ) -> Sentinel<'a> {
        let cfg = Cfg::from_context(&ctx, home);
        // Every child sees the same switch this process resolved; OFF is
        // SPIRA_LIFECYCLE_ENFORCE=0 and nothing else (sp-gypjk: no poisoned tool path).
        h.set_env(
            "SPIRA_LIFECYCLE_ENFORCE",
            if lc == Lifecycle::On { "1" } else { "0" },
        );
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
            lc,
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

    /// `[ -x mail.sh ] && mail.sh send operator --from … --subject … --kind question
    /// --default … <<body` → true only when the ask was accepted.
    pub fn mail(
        &self,
        from: &str,
        subject: &str,
        default: &str,
        body: &str,
        own_dedup: bool,
    ) -> bool {
        let mut s = Spec::args_owned(
            "mail.sh",
            vec![
                "send".into(),
                "operator".into(),
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
        if own_dedup {
            s = s.env("SPIRA_MAIL_REPEAT_CONSIDERED", "sentinel-own-dedup");
        }
        self.h.run(s).ok()
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
        match self.mode {
            Mode::SummonOnly => self.summon_only(),
            _ => self.full(),
        }
    }

    /// The bulk reads, the DB check and the goal check (every mode but --summon-only).
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
        let snap = Snapshot::new(list_raw, list, ready);
        // A GOAL THAT NAMES NO BEAD IS NEVER REACHED — and it stops nothing else. sp-ejf3's
        // concern is a "goal reached" fired against a dangling reference; that is prevented
        // by `goal_known` below. Refusing the whole pass instead stopped reclaim, landing and
        // summoning (2026-09-29: sp-spira never existed; the bash check never fired because
        // `bd show --json` prints `[]` for a missing bead, so it only surfaced in the rewrite).
        if !self.cfg.skip_reclaim && snap.get(&self.cfg.goal).is_none() {
            self.log(&format!(
                "GOAL UNRESOLVABLE — {} names no bead in {}; completion is not assessed this pass, every other check runs",
                self.cfg.goal, self.cfg.db
            ));
        }
        Ok(snap)
    }

    /// Write the snapshots (and the ready cache) where every child reads them.
    fn export_snapshot(&self, snap: &Snapshot) {
        if !snap.list_raw.is_empty() {
            if let Some(p) = self.temp_file("list-snapshot", &snap.list_raw) {
                self.h.set_env("SPIRA_LIST_SNAPSHOT", &p.to_string_lossy());
            }
        }
        if let Some(ready) = &snap.ready {
            if let Some(p) = self.temp_file("ready-snapshot", &snap.ready_raw) {
                self.h.set_env("SPIRA_READY_SNAPSHOT", &p.to_string_lossy());
            }
            if self.mode == Mode::Pass {
                self.export_ready_cache(ready);
            }
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

        // STATE
        let (open_children, plan_ready, plan_inprog) = if self.cfg.skip_reclaim {
            (Vec::new(), Some(0), 0)
        } else {
            (
                snap.goal_open_children(&self.cfg.goal),
                snap.plan_ready(&self.cfg),
                snap.plan_inprog(&self.cfg),
            )
        };
        let n_open = open_children.len();
        let live = self.live_total();
        self.log(&format!(
            "state: goal={} open={} plan_ready={} in_progress={} aeons={} fayths=[{}]",
            self.cfg.goal,
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
            self.h
                .print(&format!("\nOpen beads under {}:", self.cfg.goal));
            for id in &open_children {
                self.h.print(&format!("  {id}"));
            }
            return 0;
        }

        self.export_snapshot(&snap);

        self.phase("CHECK1");
        self.check1();
        let goal_known = self.cfg.skip_reclaim || snap.get(&self.cfg.goal).is_some();
        let goal_reached = goal_known && n_open == 0;
        if goal_reached {
            self.log(&format!(
                "goal reached — {} has no open children; finishing the sending",
                self.cfg.goal
            ));
        }

        self.phase("CHECK2");
        let lc_rows = match (self.cfg.skip_reclaim, self.lc) {
            (false, Lifecycle::On) => self.lc_rows(),
            _ => None,
        };
        if !self.cfg.skip_reclaim {
            match self.lc {
                Lifecycle::On => {
                    if let Some(rows) = &lc_rows {
                        self.check2(&snap, rows);
                    }
                }
                Lifecycle::Off => self.check2_legacy(&snap),
            }
        }
        self.phase("CHECK2b");
        self.check2b();
        self.phase("CHECK2c");
        let mut plan_ready = plan_ready;
        if !self.cfg.skip_reclaim {
            match self.lc {
                Lifecycle::On => {
                    if let Some(rows) = &lc_rows {
                        self.check2c(rows);
                    }
                }
                Lifecycle::Off => {
                    if self.check2c_legacy(&snap) > 0 {
                        // Every freed bead is claimable now; CHECK 3 and 8 reason about it.
                        plan_ready = self.plan_ready_live();
                    }
                }
            }
        }
        self.phase("CHECK3");
        let plan_ready = self.check3(plan_ready, plan_inprog, n_open);

        self.audit_dispatch();

        self.phase("CHECK6");
        self.check6();
        self.phase("CHECK3b");
        self.seam(
            "check3b",
            seams::CHECK3B,
            None,
            Io::Inherit,
            Io::Inherit,
            true,
        );
        self.phase("CHECK7");
        self.seam("ck7", seams::CK7, None, Io::Inherit, Io::Inherit, true);

        if goal_reached {
            self.phase("end");
            self.log(&format!(
                "pass complete — {} action(s), {} progress, goal reached",
                self.acted.get(),
                self.progressed.get()
            ));
            self.budget_check();
            return self.exit_code();
        }

        self.phase("CHECK8");
        self.check8(plan_ready, plan_inprog, n_open, &open_children);
        self.phase("end");
        self.log(&format!(
            "pass complete — {} action(s), {} progress",
            self.acted.get(),
            self.progressed.get()
        ));
        self.budget_check();
        self.exit_code()
    }

    /// ON mode: a machine that could not be read or written fails the unit, every pass,
    /// until it is fixed — after the rest of the pass (landing, summoning) has run.
    fn exit_code(&self) -> i32 {
        i32::from(self.lc_failed.get())
    }

    fn audit(&self, snap: &Snapshot) -> i32 {
        self.check4(snap);
        if !self.cfg.skip_closed {
            self.check5(snap);
        }
        self.check6b();
        if !self.cfg.skip_reclaim {
            self.check7c();
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

    /// `ready_count "<scope,>plan" "spira-poison,<ask>"`, asked live.
    pub fn plan_ready_live(&self) -> Option<usize> {
        let mut a = store::ready_args(&self.cfg);
        a.push("--label".into());
        a.push(self.cfg.plan_labels().join(","));
        a.push("--exclude-label".into());
        a.push(format!("spira-poison,{}", self.cfg.ask));
        store::read_json(&self.bd(), self.h, &a)
            .ok()
            .map(|(_, v)| v.len())
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

/// The pidfile fallback of `aeon_count`: live pidfiles whose process is an aeon; dead ones
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
        let alive = std::fs::read_to_string(e.path())
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .and_then(|p| std::fs::read(format!("/proc/{p}/cmdline")).ok())
            .map(|c| is_aeon_cmdline(&c))
            .unwrap_or(false);
        if alive {
            n += 1;
        } else {
            let _ = std::fs::remove_file(e.path());
        }
    }
    n
}

/// An aeon's /proc cmdline: the bash runner (`… aeon.sh …`) or the Rust binary, whose argv[0]
/// is `…/aeon` (lib.sh aeon_alive's rule after the cutover).
pub fn is_aeon_cmdline(c: &[u8]) -> bool {
    let argv0 = c.split(|b| *b == 0).next().unwrap_or(&[]);
    let argv0 = String::from_utf8_lossy(argv0);
    String::from_utf8_lossy(c).contains("aeon.sh") || argv0 == "aeon" || argv0.ends_with("/aeon")
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
    #[test]
    fn aeon_cmdline_matches_the_script_and_the_binary_only() {
        assert!(super::is_aeon_cmdline(b"bash\0/h/spira/aeon.sh\0builder\0"));
        assert!(super::is_aeon_cmdline(b"/r/current/bin/aeon\0--home\0/r/current/spira\0builder\0"));
        assert!(!super::is_aeon_cmdline(b"/usr/bin/sleep\0aeon\0"));
        assert!(!super::is_aeon_cmdline(b"/r/bin/aeonic\0"));
    }

    use super::*;

    #[test]
    fn only_the_first_argument_selects_a_mode() {
        assert_eq!(Mode::from_first(None), Mode::Pass);
        assert_eq!(Mode::from_first(Some("--report")), Mode::Report);
        assert_eq!(Mode::from_first(Some("--summon-only")), Mode::SummonOnly);
        assert_eq!(Mode::from_first(Some("--audit")), Mode::Audit);
        assert_eq!(Mode::from_first(Some("--bogus")), Mode::Pass);
    }

    #[test]
    fn grep_w_is_word_bounded() {
        assert!(grep_w("builder ops", "ops"));
        assert!(!grep_w("builder operations", "ops"));
        assert!(grep_w("ops-x", "ops"), "grep -w treats '-' as a boundary");
        assert!(!grep_w("", "ops"));
    }
}
