//! submit, protect, stats, flush, step, claim, release (DESIGN.md §2.2).

use std::fs;

use super::{
    idents, landing_log, read_text, repo_path, resolve, statute_gate, take_lock, Ctx, World, FAIL,
    OK, USAGE,
};
use crate::cli::Text;
use crate::model::LandMode;
use crate::ports::{ref_branch, Emit};
use crate::records::{self, write_atomic};

/// conf.sh's spira_gate_outcome.
pub fn gate_outcome(rc: i32) -> &'static str {
    match rc {
        0 => "PASS",
        75 => "NO_VERDICT",
        76 => "BASE_FAIL",
        _ => "FAIL",
    }
}

pub fn submit(w: &World, branch: &str, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "submit", repo) else {
        return FAIL;
    };
    let name = c.r.name.clone();
    // repo_root, else $SPIRA_REPO (queue.sh: `|| repo="$SPIRA_REPO"`).
    let Some(path) =
        c.r.path
            .clone()
            .or_else(|| w.var("SPIRA_REPO").map(Into::into))
    else {
        w.err(format!("queue.sh submit: no such repo: {name}"));
        return FAIL;
    };
    let mode = c.r.mode;
    // Every change in a queued mode is a bead on spira/<id> with a lifecycle row — a
    // suite-state edit included (sp-lck63: `testenv suites` files or is handed one).
    if mode.is_queued() && !branch.starts_with("spira/") {
        w.err(format!(
            "queue.sh submit: {branch}: queue mode requires a branch under spira/"
        ));
        return FAIL;
    }
    if idents(w, "submit", &[("branch", branch), ("repo", &name)]).is_err() {
        return FAIL;
    }
    if !w.git.ref_exists(&path, &format!("refs/heads/{branch}")) {
        w.err(format!("queue.sh submit: branch not found: {branch}"));
        return FAIL;
    }
    let mut tip = w.git.rev_parse(&path, branch).unwrap_or_default();
    let id = branch.strip_prefix("spira/").unwrap_or(branch).to_string();

    if !lint_passed(w, "submit", &path, &tip, false) {
        return FAIL;
    }

    let suites = c.s.certify_suites.clone();
    let start = w.clock.now();
    let (rc, out) = w.scripts.gate(branch, &name, &id, &suites);
    if rc != 0 {
        w.err(format!(
            "queue.sh submit: {branch} failed the gate ({})",
            gate_outcome(rc)
        ));
        w.err(out.trim_end_matches('\n'));
        landing_log(
            &c.s.run,
            &format!("QUEUE CAUGHT {} branch={id}", w.clock.now()),
        );
        return rc;
    }
    let cost = w.clock.now().saturating_sub(start);
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE GATE_COST {} branch={id} seconds={cost}",
            w.clock.now()
        ),
    );

    match mode {
        LandMode::Queue | LandMode::QueueLocal => {
            // The lifecycle row is the one certification record; the queue keeps none of
            // its own (sp-lck63).
            if lc_certify(w, &id, &tip).is_err() {
                return FAIL;
            }
            w.out(format!("queue.sh submit: certified {branch}"));
        }
        LandMode::Push => {
            let Some(base) = c.r.landref.clone() else {
                w.err(format!(
                    "queue.sh submit: cannot resolve landing ref for {name}"
                ));
                return FAIL;
            };
            let remote = c.r.ref_remote(&base);
            let base_branch = ref_branch(&base).to_string();
            if let Some(r) = &remote {
                let _ = w.git.fetch(&path, r, "");
            }
            if let Err(why) = w.lib.rebase(branch, &base, &path, &name) {
                let why = if why.is_empty() {
                    "conflict".to_string()
                } else {
                    why
                };
                w.err(format!(
                    "queue.sh submit: {branch} does not rebase onto {base} ({why})"
                ));
                return FAIL;
            }
            tip = w.git.rev_parse(&path, branch).unwrap_or_default();
            let r = remote.unwrap_or_else(|| "origin".into());
            if !w.lib.push(&path, &r, &format!("{branch}:{base_branch}")) {
                w.err(format!(
                    "queue.sh submit: push of {branch} to {base_branch} failed"
                ));
                return FAIL;
            }
            if lc_certify(w, &id, &tip).is_err()
                || !super::land::lc_deliver(
                    w,
                    &path,
                    &tip,
                    &crate::model::Member {
                        id: id.clone(),
                        tip: tip.clone(),
                    },
                )
            {
                return FAIL;
            }
            super::helpers::close_on_land(w, &c.s.submitted_label, &id, &tip);
            w.out(format!("queue.sh submit: landed {branch} (push)"));
        }
        LandMode::Pr | LandMode::Hold => {
            if lc_certify(w, &id, &tip).is_err() {
                return FAIL;
            }
            w.out(format!(
                "queue.sh submit: certified {branch} ({})",
                mode.as_str()
            ));
        }
    }
    OK
}

/// The tip's repository-declared compile-and-lint check, refused with its first error before a
/// bead is certified or re-tipped; with `verify`, only a pass already on record is accepted.
pub fn lint_passed(w: &World, verb: &str, path: &std::path::Path, tip: &str, verify: bool) -> bool {
    let (rc, out) = w.scripts.lint(tip, path, verify);
    if rc != 0 {
        w.err(format!("queue.sh {verb}: {}", out.trim_end_matches('\n')));
    }
    rc == 0
}

/// The checks an ejection named must each pass at `tip` before the bead is resubmitted: the
/// missing ones are run here, recorded with their summary line, and any still red or unrun
/// refuses by name.
fn checks_proven(
    w: &World,
    verb: &str,
    run: &std::path::Path,
    repo: &str,
    id: &str,
    tip: &str,
) -> bool {
    use spira_config::proof;
    let mut notes = Vec::new();
    for check in proof::unproven(run, id, tip) {
        if super::round::ROUND_ONLY_STEPS.contains(&check.as_str()) {
            notes.push(format!("{check} has no green round recorded at {tip}; it passes only in a round that contains this tip"));
            continue;
        }
        let (rc, out) = w.scripts.suite_run(&check, tip, repo);
        if let Err(e) = proof::record_run(run, id, tip, &check, rc, &out) {
            w.err(format!(
                "queue.sh {verb}: cannot record the run of {check}: {e} — refused"
            ));
            return false;
        }
        if rc != 0 {
            notes.push(format!(
                "{check} failed at {tip} (rc={rc}): {}",
                out.lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("no output")
            ));
        }
    }
    if notes.is_empty() {
        return true;
    }
    w.err(format!("queue.sh {verb}: {id} was ejected for failing checks that still do not pass at {tip} — refused:\n  {}", notes.join("\n  ")));
    false
}

/// Record the gate pass on spira-lc; a refusal is reported and the submit does not certify.
fn lc_certify(w: &World, id: &str, tip: &str) -> Result<(), ()> {
    w.lc.certify(id, tip, "queue-submit", &super::actor(w))
        .map_err(|(rc, e)| {
            w.err(format!(
                "queue.sh submit: spira-lc certify refused for {id} (rc={rc}): {e}"
            ));
        })
}

pub fn protect(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "protect", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "protect", &c) else {
        return FAIL;
    };
    if c.r.mode != LandMode::Queue {
        w.err(format!(
            "queue.sh protect: repo is not in queue mode (mode={})",
            c.r.mode.as_str()
        ));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!(
            "queue.sh protect: cannot resolve base ref for {}",
            c.r.name
        ));
        return FAIL;
    };
    let base_branch = ref_branch(&base).to_string();
    if idents(w, "protect", &[("branch", &base_branch)]).is_err() {
        return FAIL;
    }
    if !w.forge.branch_protect(&c.s.forge, &path, &base_branch) {
        w.err("queue.sh protect: forge branch-protect failed");
        return FAIL;
    }
    let _ = fs::create_dir_all(&c.s.run);
    let _ = fs::write(
        c.s.run.join(format!("queue-protected-{}", c.r.name)),
        format!("{base_branch}\n"),
    );
    w.out(format!(
        "queue.sh protect: protection set for repo:{} (branch: {base_branch})",
        c.r.name
    ));
    w.out("queue.sh protect: attribution note: suite-level attribution requires CI to emit");
    w.out("queue.sh protect:   failure annotations whose path matches spira/test-*.sh,");
    w.out("queue.sh protect:   or a check annotation titled \"red-twice suite\".");
    w.out(
        "queue.sh protect:   Without them a red batch PR is left for the batcher persona to judge.",
    );
    OK
}

pub fn stats(w: &World) -> i32 {
    let Ok(c) = resolve(w, "stats", None) else {
        return FAIL;
    };
    let log = fs::read_to_string(c.s.run.join("landing.log")).unwrap_or_default();
    w.io.out(&crate::stats::render(&crate::stats::tally(&log)));
    OK
}

/// `churn`: the mender's meter over a window (default 14 days) of the lifecycle's own rows.
pub fn churn(w: &World, since: Option<&str>, until: Option<&str>, repo: Option<&str>) -> i32 {
    use crate::churn;
    let Ok(c) = resolve(w, "churn", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "churn", &c) else {
        return FAIL;
    };
    let now = w.clock.now() as i64;
    let when = |flag: &str, v: &str| {
        churn::parse_when(v, now).ok_or_else(|| {
            w.err(format!(
                "queue.sh churn: --{flag} {v:?}: not a duration (14d) or date (2026-09-26)"
            ))
        })
    };
    let Ok(from) = when("since", since.unwrap_or("14d")) else {
        return USAGE;
    };
    let to = match until {
        None => now + 1,
        Some(u) => match when("until", u) {
            Ok(t) if !u.contains('T') && u.contains('-') => t + 86_400,
            Ok(t) => t,
            Err(()) => return USAGE,
        },
    };
    let facts = match w.lc.churn_facts(from).and_then(|j| churn::parse_facts(&j)) {
        Ok(f) => f,
        Err(e) => {
            w.err(format!("queue.sh churn: cannot tell: {e}"));
            return FAIL;
        }
    };
    let lines = |a: &str, b: &str| {
        w.git.numstat(&path, a, b).ok().map(|f| {
            f.iter()
                .map(|(_, add, del)| u64::from(*add) + u64::from(*del))
                .sum()
        })
    };
    let returned = churn::wasted(&churn::returns(&facts, from, to), &lines);
    let rows = churn::failures(&facts, from, to, &lines);
    let m = churn::measures(&facts, &rows, from, to, returned.identical);
    w.io.out(&churn::render(&returned, &rows, &m));
    OK
}

/// The batcher's cut (queue.sh _batch_cut). Used to run batch.sh's own pre-cut sweep first
/// (orphan-run reaping, closed-red-live mail, DIRTY-PR abandon, stale-certification
/// reconciliation, the queue-stuck alert) — dead weight since batcher-cut (sp-jzfog) took
/// over the round itself and no repo runs in `land=queue` mode (sp-uwhx0: batch.sh deleted,
/// its sweep retired with it, not ported — see queue/DESIGN.md "batch.sh retirement").
fn batch_cut(w: &World, c: &Ctx, wait_zero: bool) -> i32 {
    if c.s.batcher_off {
        w.out(format!(
            "queue.sh: SPIRA_BATCHER_ENABLE=0 — the operator cuts rounds; no cut for {}",
            c.r.name
        ));
        return OK;
    }
    let Some(bin) = c.s.batcher_bin.as_ref() else {
        w.err(format!(
            "queue.sh: no batcher program — cannot cut a round for {}",
            c.r.name
        ));
        return FAIL;
    };
    w.scripts.batcher_cut(bin, &c.r.name, wait_zero)
}

pub fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn flush(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "flush", repo) else {
        return FAIL;
    };
    if repo_path(w, "flush", &c).is_err() {
        return FAIL;
    }
    if c.r.mode != LandMode::Queue {
        w.err(format!(
            "queue.sh flush: repo is not in queue mode (mode={})",
            c.r.mode.as_str()
        ));
        return FAIL;
    }
    if idents(w, "flush", &[("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    if super::require_lc(w, "flush").is_err() {
        return FAIL;
    }
    batch_cut(w, &c, true)
}

/// Routes stderr to stdout (`cmd_publish "$name" 2>&1`).
struct Merged<'a>(&'a dyn Emit);
impl Emit for Merged<'_> {
    fn out(&self, s: &str) {
        self.0.out(s)
    }
    fn err(&self, s: &str) {
        self.0.out(s)
    }
}

/// One stepper per repository (queue-step-all.md): the step lock, never the queue lock —
/// the verdict and batch.sh take the queue lock themselves inside the step.
enum StepLock {
    Held(crate::lock::Guard),
    Busy,
    Unopenable,
}

fn step_lock(w: &World, c: &Ctx) -> StepLock {
    use crate::lock::{try_step_lock, Acquire};
    match try_step_lock(&c.s.queue_dir, &c.r.name) {
        Acquire::Held(g) => StepLock::Held(g),
        Acquire::Busy => {
            w.err(format!(
                "queue.sh step: another step holds the step lock for {} — skipped",
                c.r.name
            ));
            StepLock::Busy
        }
        Acquire::Unopenable => {
            w.err(format!(
                "queue.sh step: cannot open the step lock for {}",
                c.r.name
            ));
            StepLock::Unopenable
        }
    }
}

pub fn step(w: &World, repo: &str) -> i32 {
    let Ok(c) = resolve(w, "step", Some(repo)) else {
        return FAIL;
    };
    if idents(w, "step", &[("repo", &c.r.name)]).is_err() {
        return FAIL;
    }
    if super::require_lc(w, "step").is_err() {
        return FAIL;
    }
    match step_lock(w, &c) {
        // The other stepper is doing this work; a skipped step is not a failure.
        StepLock::Busy => OK,
        StepLock::Unopenable => FAIL,
        StepLock::Held(_g) => step_locked(w, &c),
    }
}

/// `step --all`: every queue-mode repository in spira_repos order, each under its own step
/// lock; a repository's own step status is not propagated (spira-verdict.sh's `|| true`).
/// Exit 1 only when the repository list cannot be read — absence is not success.
pub fn step_all(w: &World) -> i32 {
    let names = match w.lib.repos() {
        Ok(n) => n,
        Err(e) => {
            w.err(format!(
                "queue.sh step --all: cannot list repositories: {e}"
            ));
            return FAIL;
        }
    };
    // One switch for the whole pass: ON and unreachable refuses before any repo is stepped.
    if super::require_lc(w, "step --all").is_err() {
        return FAIL;
    }
    let (mut stepped, mut busy, mut unresolved) = (Vec::new(), Vec::new(), Vec::new());
    for name in names {
        let c = match w.lib.context(Some(&name)) {
            Ok((s, r)) => Ctx { s, r },
            Err(e) => {
                w.err(format!(
                    "queue.sh step: cannot resolve the harness configuration: {e}"
                ));
                unresolved.push(name);
                continue;
            }
        };
        if !c.r.mode.is_queued() {
            continue;
        }
        if idents(w, "step", &[("repo", &c.r.name)]).is_err() {
            unresolved.push(name);
            continue;
        }
        w.out(format!("queue.sh step --all: {}", c.r.name));
        match step_lock(w, &c) {
            StepLock::Held(g) => {
                let _ = step_locked(w, &c);
                drop(g);
                stepped.push(name);
            }
            StepLock::Busy => busy.push(name),
            StepLock::Unopenable => unresolved.push(name),
        }
    }
    let mut line = format!(
        "queue.sh step --all: stepped={} busy={} unresolved={}",
        stepped.len(),
        busy.len(),
        unresolved.len()
    );
    for (label, v) in [
        ("stepped", &stepped),
        ("busy", &busy),
        ("unresolved", &unresolved),
    ] {
        if !v.is_empty() {
            line.push_str(&format!(" {label}:{}", v.join(",")));
        }
    }
    w.out(line);
    OK
}

fn step_locked(w: &World, c: &Ctx) -> i32 {
    // The verdict runs in process (DESIGN-verdict.md); its status is not the step's, as
    // verdict.sh's was not.
    let _ = super::verdict::pass(w, c);
    batch_cut(w, c, false);
    if c.r.mode == LandMode::QueueLocal {
        let merged = Merged(w.io);
        let w2 = World { io: &merged, ..*w };
        return super::publish::publish(&w2, Some(&c.r.name));
    }
    // `if [ mode = queue.local ]; then …; fi` with a false test is status 0.
    OK
}

pub fn claim(w: &World, repo: Option<&str>, reason: &Text, force: bool) -> i32 {
    let reason = match read_text(w, reason) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh claim: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    if reason.is_empty() {
        w.err("queue.sh claim: --reason is required — say what the hand edit is for");
        return USAGE;
    }
    let Ok(c) = resolve(w, "claim", repo) else {
        return FAIL;
    };
    if repo_path(w, "claim", &c).is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "claim", &c, "") else {
        return FAIL;
    };
    let open = c.queue_file("open");
    let kv = match records::read_kv(&open) {
        Ok(Some(kv)) => kv,
        _ => {
            w.err(format!("queue.sh claim: no open batch for {}", c.r.name));
            return FAIL;
        }
    };
    let cur = kv.get("owner").unwrap_or("").to_string();
    if cur == "concierge" && !force {
        w.err(format!(
            "queue.sh claim: {} is already claimed by concierge — pass --force to reclaim",
            c.r.name
        ));
        return FAIL;
    }
    let new = records::claim_rewrite(&kv, &cur, &reason);
    if let Err(e) = write_atomic(&open, &new.render()) {
        w.err(format!("queue.sh claim: {e}"));
        return FAIL;
    }
    let was = if cur.is_empty() {
        "<none>".to_string()
    } else {
        cur
    };
    w.lib.notify(
        &c.s.mailbox,
        &c.r.name,
        "batch claimed for hand-edit",
        &format!(
            "Claimed the open batch for {} (was: {was}). Reason: {reason}",
            c.r.name
        ),
    );
    w.out(format!(
        "queue.sh claim: {} claimed for concierge (was: {was})",
        c.r.name
    ));
    OK
}

const SANCTIONED_RESUBMITTER: &str = "concierge";

fn is_full_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The concierge's door for a hand-built fix: record the new tip and move a manually held REWORK
/// row, or an unclaimed READY one, to SUBMITTED, so the round's full suite certifies it where the host gate cannot.
#[allow(clippy::too_many_arguments)]
pub fn resubmit(
    w: &World,
    id: &str,
    repo: Option<&str>,
    tip: &str,
    by: &str,
    reason: &Text,
    evidence: &Text,
    evidence_tip: &str,
) -> i32 {
    let reason = match read_text(w, reason) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh resubmit: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    if reason.trim().is_empty() {
        w.err("queue.sh resubmit: --reason is required — say why the normal path was bypassed");
        return USAGE;
    }
    let evidence = match read_text(w, evidence) {
        Ok(e) => e,
        Err(e) => {
            w.err(format!("queue.sh resubmit: cannot read the evidence: {e}"));
            return USAGE;
        }
    };
    if evidence.trim().is_empty() {
        w.err("queue.sh resubmit: --evidence is required — the checks run on the tip, command and result (break-glass carries provenance)");
        return USAGE;
    }
    if evidence_tip != tip {
        w.err(format!("queue.sh resubmit: --evidence-tip must equal --tip {tip} — the checks must have run on the tip admitted (got {evidence_tip:?}) — refused"));
        return FAIL;
    }
    if by != SANCTIONED_RESUBMITTER {
        w.err(format!("queue.sh resubmit: refused — only {SANCTIONED_RESUBMITTER} may resubmit a held bead (--by {by})"));
        return FAIL;
    }
    let reason = match statute_gate(w, "resubmit", &reason) {
        Ok(Some(line)) => format!("{reason}\n\n{line}"),
        Ok(None) => reason,
        Err(rc) => return rc,
    };
    let Ok(c) = resolve(w, "resubmit", repo) else {
        return FAIL;
    };
    let Ok(path) = repo_path(w, "resubmit", &c) else {
        return FAIL;
    };
    if idents(
        w,
        "resubmit",
        &[("bead id", id), ("repo", &c.r.name), ("by", by)],
    )
    .is_err()
    {
        return FAIL;
    }
    if !is_full_sha(tip) {
        w.err(format!(
            "queue.sh resubmit: --tip {tip}: not a full 40-hex commit name — refused"
        ));
        return FAIL;
    }
    let branch = format!("spira/{id}");
    if !w.git.commit_exists(&path, tip) {
        let remote =
            c.r.landref
                .as_deref()
                .and_then(|b| c.r.ref_remote(b))
                .unwrap_or_else(|| "origin".to_string());
        if !w.git.fetch(&path, &remote, &branch) || !w.git.commit_exists(&path, tip) {
            w.err(format!("queue.sh resubmit: --tip {tip}: no such commit in {} (fetching {branch} from {remote} did not bring it) — refused", c.r.name));
            return FAIL;
        }
    }
    if !lint_passed(w, "resubmit", &path, tip, false) {
        return FAIL;
    }
    if !checks_proven(w, "resubmit", &c.s.run, &c.r.name, id, tip) {
        return FAIL;
    }
    if super::require_lc(w, "resubmit").is_err() {
        return FAIL;
    }
    let Some(row) = w.lc.bead_row(id) else {
        w.err(format!(
            "queue.sh resubmit: {id}: no lifecycle row — refused"
        ));
        return FAIL;
    };
    match row.state.as_str() {
        "REWORK" if !row.holds.iter().any(|h| h == "manual") => {
            w.err(format!("queue.sh resubmit: {id} is held by no one (no manual hold) — hold it first with a reason, then resubmit; refused"));
            return FAIL;
        }
        "REWORK" | "READY" => {}
        "WORKING" => {
            let holder = row
                .holder
                .as_deref()
                .filter(|h| !h.is_empty())
                .unwrap_or("<unknown>");
            w.err(format!("queue.sh resubmit: {id} is held by a live aeon ({holder}) — refused, nothing changed"));
            return FAIL;
        }
        state => {
            w.err(format!("queue.sh resubmit: {id} is {state}, not REWORK or READY — refused, nothing changed"));
            return FAIL;
        }
    }
    let local = format!("refs/heads/{branch}");
    if !w.git.update_ref(&path, &local, tip, None)
        || w.git.rev_parse(&path, &local).as_deref() != Some(tip)
    {
        w.err(format!(
            "queue.sh resubmit: could not move {local} to {tip} in {} — refused, nothing changed",
            c.r.name
        ));
        return FAIL;
    }
    if let Err((rc, e)) =
        w.lc.retip(id, tip, by, reason.trim(), evidence.trim(), evidence_tip)
    {
        w.err(format!(
            "queue.sh resubmit: spira-lc refused {id} (rc={rc}): {e}"
        ));
        return FAIL;
    }
    let branch = format!("spira/{id}");
    if !w.git.branch_set(&path, &branch, tip, true) {
        w.err(format!("queue.sh resubmit: {id} re-tipped to {tip} but cannot point {branch} at it — set it by hand"));
        return FAIL;
    }
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE RESUBMIT {} branch={id} tip={tip} by={by} reason={}",
            w.clock.now(),
            reason.lines().next().unwrap_or("")
        ),
    );
    w.out(format!("queue.sh resubmit: {id} re-tipped to {tip}, {} -> SUBMITTED; admit it with `queue round open --members {id}:{tip}` — the round certifies it", row.state));
    OK
}

const SANCTIONED_REWORKERS: [&str; 2] = ["concierge", "operator"];

/// The door back to REWORK for a SUBMITTED or CERTIFIED bead that sits in no round or batch:
/// a GateRed on its current tip, so the red tip is barred and a builder claims it.
pub fn rework(w: &World, id: &str, repo: Option<&str>, by: &str, reason: &Text) -> i32 {
    let reason = match read_text(w, reason) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh rework: cannot read the reason: {e}"));
            return USAGE;
        }
    };
    if reason.trim().is_empty() {
        w.err("queue.sh rework: --reason is required — give the evidence the tip is unfit");
        return USAGE;
    }
    if !SANCTIONED_REWORKERS.contains(&by) {
        w.err(format!(
            "queue.sh rework: refused — only {} may send a bead to rework (--by {by})",
            SANCTIONED_REWORKERS.join(" or ")
        ));
        return FAIL;
    }
    let reason = match statute_gate(w, "rework", &reason) {
        Ok(Some(line)) => format!("{reason}\n\n{line}"),
        Ok(None) => reason,
        Err(rc) => return rc,
    };
    let Ok(c) = resolve(w, "rework", repo) else {
        return FAIL;
    };
    if repo_path(w, "rework", &c).is_err()
        || idents(
            w,
            "rework",
            &[("bead id", id), ("repo", &c.r.name), ("by", by)],
        )
        .is_err()
    {
        return FAIL;
    }
    if super::require_lc(w, "rework").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "rework", &c, "") else {
        return FAIL;
    };
    for record in ["open", super::round::RECORD, super::stage::STAGED] {
        let Ok(Some(kv)) = records::read_kv(&c.queue_file(record)) else {
            continue;
        };
        if kv.members().iter().any(|m| m.id == id) {
            let batch = kv.get_first("batch_id").unwrap_or("");
            w.err(format!("queue.sh rework: {id} is a member of the open {record} {batch} for {} — refused; eject it with `queue eject` or `queue round eject`", c.r.name));
            return FAIL;
        }
    }
    let Some(row) = w.lc.bead_row(id) else {
        w.err(format!("queue.sh rework: {id}: no lifecycle row — refused"));
        return FAIL;
    };
    if !matches!(row.state.as_str(), "SUBMITTED" | "CERTIFIED") {
        w.err(format!(
            "queue.sh rework: {id} is {}, not SUBMITTED or CERTIFIED — refused, nothing changed",
            row.state
        ));
        return FAIL;
    }
    let Some(tip) = row.tip.filter(|t| !t.is_empty()) else {
        w.err(format!(
            "queue.sh rework: {id} has no recorded tip — refused"
        ));
        return FAIL;
    };
    let Some((state, version)) = w.lc.bead_state(id) else {
        w.err(format!("queue.sh rework: {id}: no lifecycle row — refused"));
        return FAIL;
    };
    let kind = format!("{{\"GateRed\":{{\"tip\":\"{tip}\",\"reason\":\"suites-failed\"}}}}");
    if let Err((rc, e)) = w.lc.bead_event(id, &state, version.trim(), by, &kind) {
        w.err(format!(
            "queue.sh rework: spira-lc refused {id} (rc={rc}): {e}"
        ));
        return FAIL;
    }
    w.lib.comment(
        id,
        &format!("Returned to rework by {by} on tip {tip}.\n\n{reason}"),
    );
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE REWORK {} branch={id} tip={tip} by={by} reason={}",
            w.clock.now(),
            reason.lines().next().unwrap_or("")
        ),
    );
    w.out(format!(
        "queue.sh rework: {id} {state} -> REWORK on {tip}; a builder may claim it"
    ));
    OK
}

/// Shows the suites an eject forces on `id`'s recertification, or with `clear` drops them so the
/// next submit runs the normal gate.
pub fn forced_suites(
    w: &World,
    id: &str,
    repo: Option<&str>,
    clear: bool,
    by: &str,
    reason: &Text,
) -> i32 {
    let Ok(c) = resolve(w, "forced-suites", repo) else {
        return FAIL;
    };
    if idents(w, "forced-suites", &[("bead id", id)]).is_err() {
        return FAIL;
    }
    let forced = spira_config::proof::failing(&c.s.run, id);
    if !clear {
        w.out(if forced.is_empty() {
            format!("queue.sh forced-suites: {id} has none")
        } else {
            format!("queue.sh forced-suites: {id}: {}", forced.join(","))
        });
        return OK;
    }
    let reason = match read_text(w, reason) {
        Ok(r) if !r.trim().is_empty() => r,
        Ok(_) => {
            w.err("queue.sh forced-suites: --reason is required — say why the forced suites cannot be satisfied");
            return USAGE;
        }
        Err(e) => {
            w.err(format!(
                "queue.sh forced-suites: cannot read the reason: {e}"
            ));
            return USAGE;
        }
    };
    if !SANCTIONED_REWORKERS.contains(&by) {
        w.err(format!(
            "queue.sh forced-suites: refused — only {} may clear forced suites (--by {by})",
            SANCTIONED_REWORKERS.join(" or ")
        ));
        return FAIL;
    }
    spira_config::proof::clear_failing(&c.s.run, id);
    let sidecar = c.s.run.join("ejected").join(id);
    if sidecar.exists() && std::fs::remove_file(&sidecar).is_err() {
        w.err(format!(
            "queue.sh forced-suites: cannot remove {} — the sidecar still forces its suites",
            sidecar.display()
        ));
        return FAIL;
    }
    w.lib.comment(
        id,
        &format!(
            "Forced suites cleared by {by}: {}.\n\n{}",
            if forced.is_empty() {
                "none were recorded".to_string()
            } else {
                forced.join(",")
            },
            reason.trim()
        ),
    );
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE FORCED-SUITES-CLEAR {} branch={id} by={by} cleared={}",
            w.clock.now(),
            forced.join(",")
        ),
    );
    w.out(format!("queue.sh forced-suites: cleared {id}'s forced suites ({}); its next submit runs the normal gate", if forced.is_empty() { "none recorded".to_string() } else { forced.join(",") }));
    OK
}

/// The door for a red that was not the tree's fault: withdraw it from the lifecycle row and from the
/// batch red-tip record, so the same tip is admitted again. Evidence is mandatory and must name the tip.
pub fn absolve(
    w: &World,
    id: &str,
    repo: Option<&str>,
    by: &str,
    reason: &Text,
    evidence: &Text,
    evidence_tip: &str,
) -> i32 {
    let text = |t: &Text, what: &str| match read_text(w, t) {
        Ok(v) => Ok(v),
        Err(e) => {
            w.err(format!("queue.sh absolve: cannot read the {what}: {e}"));
            Err(USAGE)
        }
    };
    let (reason, evidence) = match (text(reason, "reason"), text(evidence, "evidence")) {
        (Ok(r), Ok(e)) => (r, e),
        (Err(rc), _) | (_, Err(rc)) => return rc,
    };
    if reason.trim().is_empty() {
        w.err("queue.sh absolve: --reason is required — say why the red was not the tree's fault");
        return USAGE;
    }
    if evidence.trim().is_empty() {
        w.err("queue.sh absolve: --evidence is required — the failing step and why it ran before any suite, or the clean rerun on the tip");
        return USAGE;
    }
    if !SANCTIONED_REWORKERS.contains(&by) {
        w.err(format!(
            "queue.sh absolve: refused — only {} may absolve a red tip (--by {by})",
            SANCTIONED_REWORKERS.join(" or ")
        ));
        return FAIL;
    }
    let reason = match statute_gate(w, "absolve", &reason) {
        Ok(Some(line)) => format!("{reason}\n\n{line}"),
        Ok(None) => reason,
        Err(rc) => return rc,
    };
    let Ok(c) = resolve(w, "absolve", repo) else {
        return FAIL;
    };
    if repo_path(w, "absolve", &c).is_err()
        || idents(
            w,
            "absolve",
            &[("bead id", id), ("repo", &c.r.name), ("by", by)],
        )
        .is_err()
    {
        return FAIL;
    }
    if super::require_lc(w, "absolve").is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "absolve", &c, "") else {
        return FAIL;
    };
    let Some(row) = w.lc.bead_row(id) else {
        w.err(format!(
            "queue.sh absolve: {id}: no lifecycle row — refused"
        ));
        return FAIL;
    };
    let Some(tip) = row.tip.clone().filter(|t| !t.is_empty()) else {
        w.err(format!(
            "queue.sh absolve: {id} has no recorded tip — refused"
        ));
        return FAIL;
    };
    if evidence_tip != tip {
        w.err(format!("queue.sh absolve: --evidence-tip must equal the bead's tip {tip} (got {evidence_tip:?}) — refused"));
        return FAIL;
    }
    let in_row = row.ejected_red_tip.as_deref() == Some(tip.as_str());
    let in_record = super::round::batch_red_at(&c, id, &tip);
    if !in_row && !in_record {
        w.err(format!(
            "queue.sh absolve: no red is recorded against {id} at {tip} — nothing to absolve"
        ));
        return FAIL;
    }
    if in_row {
        if let Err((rc, e)) =
            w.lc.absolve(id, &tip, by, reason.trim(), evidence.trim(), evidence_tip)
        {
            w.err(format!(
                "queue.sh absolve: spira-lc refused {id} (rc={rc}): {e}"
            ));
            return FAIL;
        }
    }
    if let Err(e) = super::round::forget_batch_red(&c, id, &tip) {
        w.err(format!("queue.sh absolve: {id} absolved on spira-lc but the batch red-tip record is unwritable: {e}"));
        return FAIL;
    }
    w.lib.comment(
        id,
        &format!(
            "Red at {tip} absolved by {by}.\n\n{reason}\n\nEvidence:\n{}",
            evidence.trim()
        ),
    );
    landing_log(
        &c.s.run,
        &format!(
            "QUEUE ABSOLVE {} branch={id} tip={tip} by={by} reason={}",
            w.clock.now(),
            reason.lines().next().unwrap_or("")
        ),
    );
    w.out(format!(
        "queue.sh absolve: {id} red at {tip} withdrawn; the tip is admissible again"
    ));
    OK
}

pub fn release(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "release", repo) else {
        return FAIL;
    };
    if repo_path(w, "release", &c).is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "release", &c, "") else {
        return FAIL;
    };
    let open = c.queue_file("open");
    let kv = match records::read_kv(&open) {
        Ok(Some(kv)) => kv,
        _ => {
            w.err(format!("queue.sh release: no open batch for {}", c.r.name));
            return FAIL;
        }
    };
    if kv.get("owner").unwrap_or("") != "concierge" {
        w.err(format!(
            "queue.sh release: {} is not claimed by concierge",
            c.r.name
        ));
        return FAIL;
    }
    let (new, restore) = records::release_rewrite(&kv);
    if let Err(e) = write_atomic(&open, &new.render()) {
        w.err(format!("queue.sh release: {e}"));
        return FAIL;
    }
    let shown = if restore.is_empty() {
        "<none>"
    } else {
        restore.as_str()
    };
    w.out(format!(
        "queue.sh release: {} released back to {shown}",
        c.r.name
    ));
    OK
}
