//! Which bead, and the atomic claim of it (DESIGN.md §4.1).
//!
//! Ranking is `spira-claim`'s: the ready set goes to it on STDIN and the epic lookup and the
//! resumable set in FILES — never argv (sp-o4trx: the claim outage was argv E2BIG). The one
//! thing left here is resumability, which needs the repo map and git and which spira-claim
//! by design leaves to its caller. Nothing in this module knows about briefs or runes
//! (sp-f0qhr).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use crate::bd::BeadRow;
use crate::ports::{Exec, Git};
use crate::util::Out;

/// One `spira-claim select --top-tier` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierLine {
    pub id: String,
    pub branch: String,
    pub repo: String,
    pub eprio: String,
    pub estarted: String,
    pub bprio: String,
}

pub fn parse_tier(text: &str) -> Vec<TierLine> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() < 6 || p[0].is_empty() {
                return None;
            }
            Some(TierLine { id: p[0].into(), branch: p[1].into(), repo: p[2].into(), eprio: p[3].into(), estarted: p[4].into(), bprio: p[5].into() })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Ranked ids, best first, and which of them are resumable (and the tier label).
    Ranked { ids: Vec<String>, resumable: Vec<String>, tier: Option<String> },
    /// A lookup or rank that never completed: `awake <f> claim-error <why>`, exit 1.
    ClaimError { log: String, ledger: String },
}

pub struct Selector<'a> {
    pub exec: &'a dyn Exec,
    pub git: &'a dyn Git,
    pub claim_bin: &'a str,
    pub rune: &'a str,
    pub scratch: &'a Path,
    pub pid: u32,
    /// spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
    /// `_dwarf_repo_info` bash seam this used to shell into.
    pub repos: &'a spira_config::repos::Registry,
}

impl Selector<'_> {
    fn file(&self, stem: &str) -> PathBuf {
        self.scratch.join(format!(".{stem}.{}", self.pid))
    }

    fn select_args(&self, base: &[&str]) -> Vec<String> {
        // The ready set is the machine's: `select` judges blockers by its rows too
        // (`--blockers machine`), the rule the ready set was built with (sp-s9675.2).
        let mut a = crate::ports::s(base);
        a.extend(crate::ports::s(&["--blockers", "machine"]));
        a
    }

    pub fn select(&self, ready_json: &str) -> Selection {
        let ready = ready_json.as_bytes().to_vec();
        let lk = self.exec.exec(self.claim_bin, &["epics".to_string()], Some(ready.clone()), None);
        if !lk.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_parent_lookup failed (rc={}) — not reporting idle for a lookup that never completed",
                    self.rune, lk.code
                ),
                ledger: format!("claim-error epic_parent_lookup failed rc={}", lk.code),
            };
        }
        let lk_file = self.file("epic-lookup");
        let rs_file = self.file("resumable");
        let _ = std::fs::write(&lk_file, lk.stdout.as_bytes());
        let lkf = lk_file.display().to_string();

        // The top tier only: resumability costs one git process per candidate.
        let tier = self.exec.exec(self.claim_bin, &self.select_args(&["select", "--rune", self.rune, "--epics", &lkf, "--top-tier"]), Some(ready.clone()), None);
        let band = if tier.success() { parse_tier(&tier.stdout) } else { Vec::new() };
        let (resumable, tier_label) = self.resumable(&band);

        let _ = std::fs::write(&rs_file, resumable.join(","));
        let rsf = rs_file.display().to_string();
        let ranked = self.exec.exec(self.claim_bin, &self.select_args(&["select", "--rune", self.rune, "--epics", &lkf, "--resumable", &rsf]), Some(ready), None);
        let _ = std::fs::remove_file(&lk_file);
        let _ = std::fs::remove_file(&rs_file);
        if !ranked.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_rank_rows failed (rc={}) — not reporting idle for a rank that never completed",
                    self.rune, ranked.code
                ),
                ledger: format!("claim-error epic_rank_rows failed rc={}", ranked.code),
            };
        }
        let ids = ranked.stdout.lines().filter_map(|l| l.split('\t').nth(5)).filter(|s| !s.is_empty()).map(String::from).collect();
        Selection::Ranked { ids, resumable, tier: tier_label }
    }

    /// Every top-tier candidate whose branch is ahead of its base — all of them, not just the
    /// first, so a lost race falls through to the next resumable one (sp-3ntca).
    fn resumable(&self, band: &[TierLine]) -> (Vec<String>, Option<String>) {
        if band.is_empty() {
            return (Vec::new(), None);
        }
        let mut names: Vec<String> = band.iter().map(|t| t.repo.clone()).collect();
        names.sort();
        names.dedup();
        // `_dwarf_repo_info <names…>`: root + base ref per name, skipping any name whose
        // root does not resolve to a real checkout — same filter the bash composite made.
        let repos: BTreeMap<String, (String, String)> = names
            .iter()
            .filter_map(|n| {
                let root = self.repos.root(n)?;
                if !Path::new(&root).join(".git").exists() {
                    return None;
                }
                let base = spira_config::repos::landref(self.repos, &root).unwrap_or_default();
                Some((n.clone(), (root, base)))
            })
            .collect();
        let (mut ids, mut label) = (Vec::new(), None);
        for t in band {
            let br = if t.branch.is_empty() { format!("spira/{}", t.id) } else { t.branch.clone() };
            let Some((root, base)) = repos.get(&t.repo) else { continue };
            if base.is_empty() {
                continue;
            }
            let n = self.git.git(Path::new(root), &["rev-list", "--count", &format!("{base}..{br}")]);
            let n: i64 = if n.success() { n.text().trim().parse().unwrap_or(0) } else { 0 };
            if n > 0 {
                ids.push(t.id.clone());
                label = Some(format!("P{}{}/P{}", t.eprio, if t.estarted == "0" { "/started" } else { "" }, t.bprio));
            }
        }
        (ids, label)
    }
}

/// The claim loop's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub id: String,
    pub repo: String,
    pub row: BeadRow,
    pub raw: String,
}

/// One attempt's answer, spira-lc's own exit codes: 0 applied, 3 refused, else cannot tell.
pub const LC_APPLIED: i32 = 0;
pub const LC_REFUSED: i32 = 3;

/// A claim whose answer could not be read, and whose read-back could not tell either: the
/// dwarf may hold the bead, so the loop stops rather than claim a second (sp-druc64).
pub const LC_UNKNOWN: i32 = -1;

/// What `spira-lc holds <id>` said: the hold names, or that the store could not tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holds {
    Known(Vec<String>),
    Unknown(String),
}

/// One bead's lifecycle row, read as `spira-lc show <id>`'s JSON and nothing else: a non-zero
/// exit, or stdout that is not the `{"bead": …}` object, is no answer (an error is never data,
/// whatever stream it arrived on, and no word in it is matched). `Ok(None)`: the machine
/// answered and has no row for `id`.
fn show_row(o: &Out) -> Result<Option<spira_config::lc_state::Row>, String> {
    let said = || o.stderr.lines().chain(o.stdout.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    match o.code {
        0 => spira_config::lc_state::parse_show(&o.stdout).map_err(|e| format!("{e} ({})", said())),
        1 => Ok(None),
        rc => Err(format!("rc={rc}: {}", said())),
    }
}

/// `spira-lc show <id>`, retried on a non-answer with a doubling backoff from `delay`, `tries` in all.
fn ask_row(
    exec: &dyn Exec,
    id: &str,
    tries: u32,
    delay: Duration,
    sleep: &(dyn Fn(Duration) + Sync),
) -> Result<Option<spira_config::lc_state::Row>, String> {
    let mut wait = delay;
    let mut why = String::new();
    for i in 1..=tries.max(1) {
        match show_row(&exec.exec("spira-lc", &crate::ports::s(&["show", id]), None, None)) {
            Ok(row) => return Ok(row),
            Err(e) => why = e,
        }
        if i < tries {
            sleep(wait);
            wait = wait.saturating_mul(2);
        }
    }
    Err(why)
}

/// The holds on a bead, `wait` excluded (it does not block a claim), from the row's typed
/// `holds` field. A store that cannot tell is retried; one that still cannot tell, or that has
/// no row for the bead, is `Unknown`, never a list of hold names.
pub fn read_holds(exec: &dyn Exec, id: &str, tries: u32, delay: Duration, sleep: &(dyn Fn(Duration) + Sync)) -> Holds {
    match ask_row(exec, id, tries, delay, sleep) {
        Ok(Some(r)) => Holds::Known(r.holds.into_iter().filter(|h| h != "wait").collect()),
        Ok(None) => Holds::Unknown(format!("spira-lc has no lifecycle row for {id}")),
        Err(why) => Holds::Unknown(why),
    }
}

/// A claim attempt's exit, settled: applied and refused stand; "cannot tell" (the reply may
/// have been lost after the write landed) is settled by reading the row back — WORKING under
/// `holder` is applied, anything else read is refused, and a read-back that cannot tell
/// either is `LC_UNKNOWN`, so the loop never claims a second bead while it may hold one.
pub fn settle_claim(rc: i32, exec: &dyn Exec, id: &str, holder: &str, tries: u32, delay: Duration, sleep: &(dyn Fn(Duration) + Sync)) -> i32 {
    if rc == LC_APPLIED || rc == LC_REFUSED {
        return rc;
    }
    match ask_row(exec, id, tries, delay, sleep) {
        Ok(Some(r)) if r.working() && r.holder.as_deref() == Some(holder) => LC_APPLIED,
        Ok(_) => LC_REFUSED,
        Err(_) => LC_UNKNOWN,
    }
}

/// The row a claim just won reads WORKING under `holder`: a resume of a REWORK bead is a
/// Claim like any other, and a session started on a row still REWORK has a submit refused.
pub fn verify_held(exec: &dyn Exec, id: &str, holder: &str, tries: u32, delay: Duration, sleep: &(dyn Fn(Duration) + Sync)) -> Result<(), String> {
    match ask_row(exec, id, tries, delay, sleep)? {
        Some(r) if r.working() && r.holder.as_deref() == Some(holder) => Ok(()),
        Some(r) => Err(format!("{id} reads {} held by {:?} at version {:?}, not WORKING under {holder}", r.state, r.holder, r.version)),
        None => Err(format!("{id} has no lifecycle row")),
    }
}

/// Which WORKING row, if any, `mine`'s name holds under another session still running, read
/// from `spira-lc list --state WORKING`'s JSON rows. Err when the machine cannot answer: a
/// claim that cannot check for a double hold refuses rather than assume there is none.
pub fn double_hold(exec: &dyn Exec, mine: &str, live: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
    let o = exec.exec("spira-lc", &crate::ports::s(&["list", "--state", "WORKING"]), None, None);
    if o.code != 0 {
        return Err(format!("spira-lc list --state WORKING exited {}: {}", o.code, o.first_err_line()));
    }
    let rows = spira_config::lc_state::parse_rows(&o.stdout)?;
    Ok(rows.into_iter().find_map(|r| {
        let holder = r.holder.as_deref().filter(|_| r.working())?;
        (live(holder) && spira_config::session::same_name_other_session(holder, mine)).then_some(r.bead_id)
    }))
}

/// The lifecycle claim loop: one Claim event per ranked candidate until one applies. A refused
/// candidate is passed to `release` before the next is tried, so a claim that applied and was
/// then refused is never left held. The row is the claim, so a refusal is the bead no longer READY/REWORK — another dwarf won it — and
/// the next candidate is tried. Returns the claimed id, the log lines, and whether every
/// attempt was "cannot tell" (a machine that never answered is not an idle queue).
pub fn lc_claim_loop(
    ids: &[String],
    resumable: &[String],
    tier: Option<&str>,
    who: &str,
    mut release: impl FnMut(&str),
    mut attempt: impl FnMut(&str) -> i32,
) -> (Option<String>, Vec<String>, bool) {
    let mut logs = Vec::new();
    let mut answered = false;
    for id in ids {
        match attempt(id) {
            LC_APPLIED => {
                if resumable.iter().any(|r| r == id) {
                    logs.push(format!("{who}: resuming {id} ({}) — it already has work on its branch", tier.unwrap_or("top rank")));
                } else {
                    logs.push(format!("{who}: claiming {id} (epic-first rank)"));
                }
                return (Some(id.clone()), logs, false);
            }
            LC_REFUSED => {
                answered = true;
                release(id);
                logs.push(format!(
                    "{who}: the lifecycle machine refused ranked candidate {id} (no longer READY/REWORK, or its stack exceeds stack_max_depth) — trying the next ranked candidate"
                ));
            }
            LC_UNKNOWN => {
                logs.push(format!(
                    "{who}: the claim of {id} went unanswered and its row cannot be read back — it may be held, so no other candidate is claimed"
                ));
                return (None, logs, true);
            }
            rc => logs.push(format!("{who}: the lifecycle machine could not be reached for ranked candidate {id} (rc={rc}) — trying the next ranked candidate")),
        }
    }
    (None, logs, !ids.is_empty() && !answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeExec {
        #[allow(clippy::type_complexity)]
        calls: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
        epics: Out,
        tier: Out,
        rank: Out,
    }
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
            let sin = stdin.map(|b| String::from_utf8(b).unwrap());
            self.calls.lock().unwrap().push((prog.to_string(), args.to_vec(), sin));
            if args[0] == "epics" {
                return self.epics.clone();
            }
            if args.iter().any(|a| a == "--top-tier") {
                return self.tier.clone();
            }
            // The resumable file must exist while spira-claim reads it.
            let i = args.iter().position(|a| a == "--resumable").unwrap();
            let f = std::fs::read_to_string(&args[i + 1]).unwrap();
            let mut o = self.rank.clone();
            o.stdout = o.stdout.replace("{RES}", &f);
            o
        }
    }
    /// Each test its own scratch dir: the selector names its files `.<stem>.<pid>`, and
    /// parallel tests share one pid, so a shared dir let one test delete another's file.
    fn scratch(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("dwarf-claim-{name}"))
    }
    struct FakeGit(i64);
    impl Git for FakeGit {
        fn git(&self, _d: &Path, args: &[&str]) -> Out {
            assert_eq!(args[0], "rev-list");
            Out::ok(format!("{}\n", self.0))
        }
    }

    fn sel<'a>(e: &'a FakeExec, repos: &'a spira_config::repos::Registry, g: &'a FakeGit, dir: &'a Path) -> Selector<'a> {
        Selector { exec: e, git: g, claim_bin: "spira-claim", rune: "builder", scratch: dir, pid: 7, repos }
    }

    #[test]
    fn lookup_or_rank_failure_is_claim_error_not_idle() {
        let dir = scratch("t2");
        let e = FakeExec { calls: Mutex::new(vec![]), epics: Out::fail(2, "x"), tier: Out::ok(""), rank: Out::ok("") };
        let repos = spira_config::repos::Registry::default();
        let g = FakeGit(0);
        match sel(&e, &repos, &g, &dir).select("[]") {
            Selection::ClaimError { ledger, .. } => assert_eq!(ledger, "claim-error epic_parent_lookup failed rc=2"),
            o => panic!("{o:?}"),
        }
        let e2 = FakeExec { calls: Mutex::new(vec![]), epics: Out::ok("{}"), tier: Out::fail(2, ""), rank: Out::fail(7, "") };
        match sel(&e2, &repos, &g, &dir).select("[]") {
            Selection::ClaimError { ledger, log } => {
                assert_eq!(ledger, "claim-error epic_rank_rows failed rc=7");
                assert!(log.contains("not reporting idle"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn lc_claim_loop_takes_the_first_applied_and_never_steals_a_refused_one() {
        let ids = vec!["sp-held".to_string(), "sp-down".into(), "sp-ok".into(), "sp-never".into()];
        let mut tried = Vec::new();
        let (c, logs, unreachable) = lc_claim_loop(&ids, &["sp-ok".into()], Some("P1/P2"), "builder/eitri", |_| (), |id| {
            tried.push(id.to_string());
            match id {
                "sp-held" => LC_REFUSED,
                "sp-down" => 2,
                _ => LC_APPLIED,
            }
        });
        assert_eq!(c.as_deref(), Some("sp-ok"));
        assert!(!unreachable);
        assert_eq!(tried, vec!["sp-held", "sp-down", "sp-ok"], "stops at the first applied claim");
        assert!(logs[0].contains("refused ranked candidate sp-held"), "{logs:?}");
        assert!(logs[1].contains("could not be reached for ranked candidate sp-down (rc=2)"), "{logs:?}");
        assert_eq!(logs[2], "builder/eitri: resuming sp-ok (P1/P2) — it already has work on its branch");
    }

    #[test]
    fn a_claim_refused_after_it_applied_is_released_before_the_next_so_exactly_one_is_held() {
        let me = "dwarf-nordri@1.2";
        let st = Store::ready(&["sp-first", "sp-b", "sp-c"]);
        let ids = vec!["sp-first".to_string(), "sp-b".into(), "sp-c".into()];
        let (won, _, _) = lc_claim_loop(
            &ids,
            &[],
            None,
            "b/nordri",
            |id| {
                let mut rows = st.rows.lock().unwrap();
                if rows[id] == ("WORKING".to_string(), me.to_string()) {
                    rows.insert(id.to_string(), ("READY".into(), String::new()));
                }
            },
            |id| {
                let rc = st.claim(id, me);
                if id == "sp-first" {
                    return LC_REFUSED;
                }
                rc
            },
        );
        assert_eq!(won.as_deref(), Some("sp-b"));
        assert_eq!(
            st.held_by(me),
            vec!["sp-b"],
            "the refused-after-applied claim is handed back"
        );
    }

    #[test]
    fn lc_claim_loop_that_never_heard_the_machine_is_not_idle() {
        let ids = vec!["sp-a".to_string(), "sp-b".into()];
        let (c, _, unreachable) = lc_claim_loop(&ids, &[], None, "b/x", |_| (), |_| 2);
        assert!(c.is_none() && unreachable);
        let (c, _, unreachable) = lc_claim_loop(&ids, &[], None, "b/x", |_| (), |_| LC_REFUSED);
        assert!(c.is_none() && !unreachable, "refusals are an answer: idle");
        let (c, _, unreachable) = lc_claim_loop(&[], &[], None, "b/x", |_| (), |_| unreachable!());
        assert!(c.is_none() && !unreachable);
    }

    /// The scripted store the claim and holds tests run against: a row per bead, a reply
    /// that can be lost after its write landed, and `spira-lc holds`/`show` answers that can
    /// be "cannot tell" for a number of calls.
    #[derive(Default)]
    struct Store {
        rows: Mutex<BTreeMap<String, (String, String)>>,
        lost_reply: Vec<String>,
        holds: Vec<String>,
        cannot_tell_holds: Mutex<u32>,
        cannot_tell_show: Mutex<u32>,
        /// What the client-side `spira-lc holds` text view prints instead of the hold names:
        /// a usage line, or nothing at all when its own `show` failed (exit 0 either way).
        holds_text: Option<String>,
        /// `spira-lc list --state WORKING`'s answer, when a test scripts it.
        list: Option<Out>,
        calls: Mutex<Vec<String>>,
    }
    impl Store {
        fn ready(ids: &[&str]) -> Store {
            let s = Store::default();
            for id in ids {
                s.rows.lock().unwrap().insert(id.to_string(), ("READY".into(), String::new()));
            }
            s
        }
        /// lib.sh's lc_claim_bead against this store: the write lands, then the reply may be lost.
        fn claim(&self, id: &str, holder: &str) -> i32 {
            let mut rows = self.rows.lock().unwrap();
            let r = rows.get_mut(id).unwrap();
            if r.0 != "READY" && r.0 != "REWORK" {
                return LC_REFUSED;
            }
            *r = ("WORKING".into(), holder.into());
            if self.lost_reply.iter().any(|l| l == id) {
                2
            } else {
                LC_APPLIED
            }
        }
        /// `work submit`: accepted only from WORKING under the submitter.
        fn submit(&self, id: &str, holder: &str) -> Result<(), String> {
            let rows = self.rows.lock().unwrap();
            match rows.get(id) {
                Some((st, h)) if st == "WORKING" && h == holder => Ok(()),
                Some((st, _)) => Err(format!("IllegalTransition from {st}")),
                None => Err("no row".into()),
            }
        }
        fn held_by(&self, holder: &str) -> Vec<String> {
            self.rows.lock().unwrap().iter().filter(|(_, (st, h))| st == "WORKING" && h == holder).map(|(id, _)| id.clone()).collect()
        }
        fn take(n: &Mutex<u32>) -> bool {
            let mut n = n.lock().unwrap();
            if *n > 0 {
                *n -= 1;
                return true;
            }
            false
        }
    }
    impl Exec for Store {
        fn exec(&self, prog: &str, args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
            assert_eq!(prog, "spira-lc");
            self.calls.lock().unwrap().push(args.join(" "));
            let cannot = || Out { code: 2, stdout: "cannot tell: deadline\n".into(), stderr: "cannot tell: deadline".into() };
            match args[0].as_str() {
                "holds" => {
                    if Store::take(&self.cannot_tell_holds) {
                        return cannot();
                    }
                    Out::ok(self.holds_text.clone().unwrap_or_else(|| self.holds.join("\n")))
                }
                "show" => {
                    if Store::take(&self.cannot_tell_show) || Store::take(&self.cannot_tell_holds) {
                        return cannot();
                    }
                    let rows = self.rows.lock().unwrap();
                    let (st, h) = rows.get(&args[1]).cloned().unwrap_or_default();
                    Out::ok(serde_json::json!({"bead": {"bead_id": args[1], "state": st, "holder": h, "holds": self.holds, "version": "7"}}).to_string())
                }
                "list" => self.list.clone().expect("unscripted spira-lc list"),
                other => panic!("unscripted spira-lc {other}"),
            }
        }
    }

    fn no_sleep(_: Duration) {}

    #[test]
    fn holds_answering_cannot_tell_is_never_a_hold() {
        let st = Store { cannot_tell_holds: Mutex::new(u32::MAX), ..Store::ready(&["sp-a"]) };
        let h = read_holds(&st, "sp-a", 3, Duration::ZERO, &no_sleep);
        assert!(matches!(&h, Holds::Unknown(why) if why.contains("cannot tell")), "cannot tell must read as unknown, not as hold names: {h:?}");
        assert_eq!(st.calls.lock().unwrap().len(), 3, "cannot tell is retried, then given up as unknown");
    }

    #[test]
    fn holds_cannot_tell_then_an_answer_is_that_answer() {
        let st = Store { cannot_tell_holds: Mutex::new(1), holds: vec!["ask".into(), "wait".into()], ..Store::ready(&["sp-a"]) };
        let slept = Mutex::new(Vec::new());
        let h = read_holds(&st, "sp-a", 3, Duration::from_secs(1), &|d| slept.lock().unwrap().push(d));
        assert_eq!(h, Holds::Known(vec!["ask".into()]));
        assert_eq!(*slept.lock().unwrap(), vec![Duration::from_secs(1)], "one backoff before the retry");
        let st = Store { holds: vec![], ..Store::ready(&["sp-a"]) };
        assert_eq!(read_holds(&st, "sp-a", 3, Duration::ZERO, &no_sleep), Holds::Known(vec![]));
    }

    #[test]
    fn a_claim_whose_reply_is_lost_holds_exactly_that_one_bead() {
        let me = "dwarf-eitri@1.2";
        let st = Store { lost_reply: vec!["sp-a".into()], ..Store::ready(&["sp-a", "sp-b", "sp-c"]) };
        let ids = vec!["sp-a".to_string(), "sp-b".into(), "sp-c".into()];
        let (won, _, unreachable) = lc_claim_loop(&ids, &[], None, "b/eitri", |_| (), |id| {
            let rc = st.claim(id, me);
            settle_claim(rc, &st, id, me, 3, Duration::ZERO, &no_sleep)
        });
        assert_eq!(st.held_by(me), vec!["sp-a"], "the lost-reply claim landed: the dwarf holds it and nothing else");
        assert_eq!(won.as_deref(), Some("sp-a"));
        assert!(!unreachable);
    }

    #[test]
    fn resuming_a_rework_bead_leaves_its_row_working_under_this_holder_and_its_submit_is_accepted() {
        let me = "dwarf-eitri@1.2";
        let st = Store::default();
        st.rows.lock().unwrap().insert("sp-a".into(), ("REWORK".into(), String::new()));
        assert!(st.submit("sp-a", me).is_err(), "a REWORK row refuses a submit");
        assert!(verify_held(&st, "sp-a", me, 1, Duration::ZERO, &no_sleep).is_err(), "REWORK is not held");
        let ids = vec!["sp-a".to_string()];
        let (won, logs, _) = lc_claim_loop(&ids, &["sp-a".into()], None, "b/eitri", |_| (), |id| {
            let rc = st.claim(id, me);
            settle_claim(rc, &st, id, me, 1, Duration::ZERO, &no_sleep)
        });
        assert_eq!(won.as_deref(), Some("sp-a"));
        assert!(logs.iter().any(|l| l.contains("resuming sp-a")), "{logs:?}");
        assert_eq!(verify_held(&st, "sp-a", me, 1, Duration::ZERO, &no_sleep), Ok(()));
        assert_eq!(st.held_by(me), vec!["sp-a"]);
        assert_eq!(st.submit("sp-a", me), Ok(()));
    }

    #[test]
    fn a_claim_that_left_the_row_in_rework_is_refused_with_the_rows_state_and_version() {
        let st = Store::default();
        st.rows.lock().unwrap().insert("sp-a".into(), ("REWORK".into(), String::new()));
        let why = verify_held(&st, "sp-a", "dwarf-eitri@1.2", 1, Duration::ZERO, &no_sleep).unwrap_err();
        assert!(why.contains("REWORK") && why.contains("\"7\""), "{why}");
    }

    #[test]
    fn a_row_held_by_another_holder_is_not_verified_held() {
        let st = Store::default();
        st.rows.lock().unwrap().insert("sp-a".into(), ("WORKING".into(), "dwarf-other@9.9".into()));
        let why = verify_held(&st, "sp-a", "dwarf-eitri@1.2", 1, Duration::ZERO, &no_sleep).unwrap_err();
        assert!(why.contains("WORKING") && why.contains("dwarf-other@9.9") && why.contains("\"7\""), "{why}");
    }

    #[test]
    fn a_lost_claim_reply_that_cannot_be_read_back_claims_nothing_else() {
        let me = "dwarf-eitri@1.2";
        let st = Store { lost_reply: vec!["sp-a".into()], cannot_tell_show: Mutex::new(u32::MAX), ..Store::ready(&["sp-a", "sp-b"]) };
        let ids = vec!["sp-a".to_string(), "sp-b".into()];
        let (won, logs, unreachable) = lc_claim_loop(&ids, &[], None, "b/eitri", |_| (), |id| {
            let rc = st.claim(id, me);
            settle_claim(rc, &st, id, me, 2, Duration::ZERO, &no_sleep)
        });
        assert_eq!(st.held_by(me), vec!["sp-a"], "never a second claim while the first is unknown");
        assert!(won.is_none() && unreachable, "{logs:?}");
    }

    #[test]
    fn a_lost_claim_reply_that_did_not_land_tries_the_next() {
        let me = "dwarf-eitri@1.2";
        let st = Store::ready(&["sp-b"]);
        st.rows.lock().unwrap().insert("sp-a".into(), ("WORKING".into(), "dwarf-other@9.9".into()));
        let ids = vec!["sp-a".to_string(), "sp-b".into()];
        let (won, _, _) = lc_claim_loop(&ids, &[], None, "b/eitri", |_| (), |id| {
            let rc = if id == "sp-a" { 2 } else { st.claim(id, me) };
            settle_claim(rc, &st, id, me, 3, Duration::ZERO, &no_sleep)
        });
        assert_eq!(won.as_deref(), Some("sp-b"));
        assert_eq!(st.held_by(me), vec!["sp-b"]);
    }

    #[test]
    fn holds_are_the_rows_typed_field_never_words_on_stdout() {
        // `spira-lc holds` once printed its usage text at exit 0, and the words read as holds.
        let st = Store { holds_text: Some("usage: spira-lc holds <bead-id>\n".into()), ..Store::ready(&["sp-a"]) };
        assert_eq!(read_holds(&st, "sp-a", 1, Duration::ZERO, &no_sleep), Holds::Known(vec![]));
        let st = Store { holds: vec!["poison".into(), "wait".into()], holds_text: Some("usage\n".into()), ..Store::ready(&["sp-a"]) };
        assert_eq!(read_holds(&st, "sp-a", 1, Duration::ZERO, &no_sleep), Holds::Known(vec!["poison".into()]));
    }

    #[test]
    fn holds_whose_row_cannot_be_read_is_unknown_even_at_exit_0() {
        // The client-side `holds` answers exit 0 with empty output when its own show fails.
        let st = Store { holds_text: Some(String::new()), cannot_tell_show: Mutex::new(u32::MAX), ..Store::ready(&["sp-a"]) };
        let h = read_holds(&st, "sp-a", 2, Duration::ZERO, &no_sleep);
        assert!(matches!(h, Holds::Unknown(_)), "a row that cannot be read is not a row with no holds: {h:?}");
    }

    fn working(id: &str, holder: &str) -> serde_json::Value {
        serde_json::json!({"bead_id": id, "state": "WORKING", "holder": holder})
    }

    #[test]
    fn double_hold_reads_the_typed_working_rows() {
        let me = "dwarf-eitri@100.5";
        let rows = serde_json::json!([working("sp-x", "dwarf-other@7.7"), working("sp-y", "dwarf-eitri@200.9")]).to_string();
        let st = Store { list: Some(Out::ok(rows.clone())), ..Store::default() };
        assert_eq!(double_hold(&st, me, &|_| true), Ok(Some(String::from("sp-y"))));
        assert_eq!(double_hold(&st, me, &|_| false), Ok::<_, String>(None), "a dead session's row is the reaper's");
        let st = Store { list: Some(Out::ok(String::from("[]"))), ..Store::default() };
        assert_eq!(double_hold(&st, me, &|_| true), Ok::<_, String>(None));
    }

    #[test]
    fn double_hold_that_cannot_read_the_rows_refuses() {
        let me = "dwarf-eitri@100.5";
        let st = Store { list: Some(Out { code: 2, stdout: String::new(), stderr: "cannot tell: deadline".into() }), ..Store::default() };
        assert!(double_hold(&st, me, &|_| true).is_err(), "no answer is not 'no double hold'");
        let st = Store { list: Some(Out::ok(String::from("sp-y\tWORKING\tdwarf-eitri@200.9\n"))), ..Store::default() };
        assert!(double_hold(&st, me, &|_| true).is_err(), "a text view is not the typed rows");
    }
}
