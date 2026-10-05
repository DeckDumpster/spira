//! submit, protect, stats, flush, step, claim, release (DESIGN.md §2.2).

use std::fs;

use super::{idents, landing_log, read_text, repo_path, resolve, take_lock, Ctx, World, FAIL, OK, USAGE};
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
    let Ok(c) = resolve(w, "submit", repo) else { return FAIL };
    let name = c.r.name.clone();
    // repo_root, else $SPIRA_REPO (queue.sh: `|| repo="$SPIRA_REPO"`).
    let Some(path) = c.r.path.clone().or_else(|| w.var("SPIRA_REPO").map(Into::into)) else {
        w.err(format!("queue.sh submit: no such repo: {name}"));
        return FAIL;
    };
    let mode = c.r.mode;
    if mode.is_queued() && !(branch.starts_with("spira/") || branch.starts_with("spira-suite-state/")) {
        w.err(format!("queue.sh submit: {branch}: queue mode requires a branch under spira/ or spira-suite-state/"));
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

    let suites = w.var("SPIRA_CERTIFY_SUITES").unwrap_or_else(|| "on".into());
    let start = w.clock.now();
    let (rc, out) = w.scripts.gate(branch, &name, &id, &suites);
    if rc != 0 {
        w.err(format!("queue.sh submit: {branch} failed the gate ({})", gate_outcome(rc)));
        w.err(out.trim_end_matches('\n'));
        landing_log(&c.s.run, &format!("QUEUE CAUGHT {} branch={id}", w.clock.now()));
        return rc;
    }
    let cost = w.clock.now().saturating_sub(start);
    landing_log(&c.s.run, &format!("QUEUE GATE_COST {} branch={id} seconds={cost}", w.clock.now()));

    match mode {
        LandMode::Queue | LandMode::QueueLocal => {
            if lc_certify(w, &id, &tip).is_err() {
                return FAIL;
            }
            let entry = c.s.queue_dir.join(&id);
            if write_atomic(&entry, &format!("CERTIFIED {tip} {}\n", w.clock.now())).is_err() {
                w.err(format!("queue.sh submit: failed to write queue entry for {branch}"));
                return FAIL;
            }
            w.out(format!("queue.sh submit: certified {branch}"));
        }
        LandMode::Push => {
            let Some(base) = c.r.landref.clone() else {
                w.err(format!("queue.sh submit: cannot resolve landing ref for {name}"));
                return FAIL;
            };
            let remote = c.r.ref_remote(&base);
            let base_branch = ref_branch(&base).to_string();
            if let Some(r) = &remote {
                let _ = w.git.fetch(&path, r, "");
            }
            if let Err(why) = w.lib.rebase(branch, &base, &path, &name) {
                let why = if why.is_empty() { "conflict".to_string() } else { why };
                w.err(format!("queue.sh submit: {branch} does not rebase onto {base} ({why})"));
                return FAIL;
            }
            tip = w.git.rev_parse(&path, branch).unwrap_or_default();
            let r = remote.unwrap_or_else(|| "origin".into());
            if !w.lib.push(&path, &r, &format!("{branch}:{base_branch}")) {
                w.err(format!("queue.sh submit: push of {branch} to {base_branch} failed"));
                return FAIL;
            }
            if lc_certify(w, &id, &tip).is_err() || !super::land::lc_deliver(w, &path, &tip, &crate::model::Member { id: id.clone(), tip: tip.clone() }) {
                return FAIL;
            }
            super::helpers::close_on_land(w, &c.s.submitted_label, &id, &tip);
            w.out(format!("queue.sh submit: landed {branch} (push)"));
        }
        LandMode::Pr | LandMode::Hold => {
            if lc_certify(w, &id, &tip).is_err() {
                return FAIL;
            }
            w.out(format!("queue.sh submit: certified {branch} ({})", mode.as_str()));
        }
    }
    OK
}

/// Record the gate pass on spira-lc; a refusal is reported and the submit does not certify.
/// A `spira-suite-state/` transition branch is no bead and has no lifecycle row to certify.
fn lc_certify(w: &World, id: &str, tip: &str) -> Result<(), ()> {
    if id.starts_with("spira-suite-state/") {
        return Ok(());
    }
    w.lc.certify(id, tip, "queue-submit", &super::actor(w)).map_err(|(rc, e)| {
        w.err(format!("queue.sh submit: spira-lc certify refused for {id} (rc={rc}): {e}"));
    })
}

pub fn protect(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "protect", repo) else { return FAIL };
    let Ok(path) = repo_path(w, "protect", &c) else { return FAIL };
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh protect: repo is not in queue mode (mode={})", c.r.mode.as_str()));
        return FAIL;
    }
    let Some(base) = c.r.landref.clone() else {
        w.err(format!("queue.sh protect: cannot resolve base ref for {}", c.r.name));
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
    let _ = fs::write(c.s.run.join(format!("queue-protected-{}", c.r.name)), format!("{base_branch}\n"));
    w.out(format!("queue.sh protect: protection set for repo:{} (branch: {base_branch})", c.r.name));
    w.out("queue.sh protect: attribution note: suite-level attribution requires CI to emit");
    w.out("queue.sh protect:   failure annotations whose path matches spira/test-*.sh,");
    w.out("queue.sh protect:   or a check annotation titled \"red-twice suite\".");
    w.out("queue.sh protect:   Without them a red batch PR is left for the batcher persona to judge.");
    OK
}

pub fn stats(w: &World) -> i32 {
    let Ok(c) = resolve(w, "stats", None) else { return FAIL };
    let log = fs::read_to_string(c.s.run.join("landing.log")).unwrap_or_default();
    w.io.out(&crate::stats::render(&crate::stats::tally(&log)));
    OK
}

/// The batcher's cut (queue.sh _batch_cut). Used to run batch.sh's own pre-cut sweep first
/// (orphan-run reaping, closed-red-live mail, DIRTY-PR abandon, stale-certification
/// reconciliation, the queue-stuck alert) — dead weight since batcher-cut (sp-jzfog) took
/// over the round itself and no repo runs in `land=queue` mode (sp-uwhx0: batch.sh deleted,
/// its sweep retired with it, not ported — see queue/DESIGN.md "batch.sh retirement").
fn batch_cut(w: &World, c: &Ctx, wait_zero: bool) -> i32 {
    if c.s.batcher_off {
        w.out(format!("queue.sh: SPIRA_BATCHER_ENABLE=0 — the operator cuts rounds; no cut for {}", c.r.name));
        return OK;
    }
    let Some(bin) = c.s.batcher_bin.as_ref() else {
        w.err(format!("queue.sh: no batcher program — cannot cut a round for {}", c.r.name));
        return FAIL;
    };
    w.scripts.batcher_cut(bin, &c.r.name, wait_zero)
}

pub fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub fn flush(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "flush", repo) else { return FAIL };
    if repo_path(w, "flush", &c).is_err() {
        return FAIL;
    }
    if c.r.mode != LandMode::Queue {
        w.err(format!("queue.sh flush: repo is not in queue mode (mode={})", c.r.mode.as_str()));
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
            w.err(format!("queue.sh step: another step holds the step lock for {} — skipped", c.r.name));
            StepLock::Busy
        }
        Acquire::Unopenable => {
            w.err(format!("queue.sh step: cannot open the step lock for {}", c.r.name));
            StepLock::Unopenable
        }
    }
}

pub fn step(w: &World, repo: &str) -> i32 {
    let Ok(c) = resolve(w, "step", Some(repo)) else { return FAIL };
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
            w.err(format!("queue.sh step --all: cannot list repositories: {e}"));
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
                w.err(format!("queue.sh step: cannot resolve the harness configuration: {e}"));
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
    for (label, v) in [("stepped", &stepped), ("busy", &busy), ("unresolved", &unresolved)] {
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
    let Ok(c) = resolve(w, "claim", repo) else { return FAIL };
    if repo_path(w, "claim", &c).is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "claim", &c, "") else { return FAIL };
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
        w.err(format!("queue.sh claim: {} is already claimed by concierge — pass --force to reclaim", c.r.name));
        return FAIL;
    }
    let new = records::claim_rewrite(&kv, &cur, &reason);
    if let Err(e) = write_atomic(&open, &new.render()) {
        w.err(format!("queue.sh claim: {e}"));
        return FAIL;
    }
    let was = if cur.is_empty() { "<none>".to_string() } else { cur };
    w.lib.notify(&c.r.name, "batch claimed for hand-edit", &format!("Claimed the open batch for {} (was: {was}). Reason: {reason}", c.r.name));
    w.out(format!("queue.sh claim: {} claimed for concierge (was: {was})", c.r.name));
    OK
}

pub fn release(w: &World, repo: Option<&str>) -> i32 {
    let Ok(c) = resolve(w, "release", repo) else { return FAIL };
    if repo_path(w, "release", &c).is_err() {
        return FAIL;
    }
    let Ok(_g) = take_lock(w, "release", &c, "") else { return FAIL };
    let open = c.queue_file("open");
    let kv = match records::read_kv(&open) {
        Ok(Some(kv)) => kv,
        _ => {
            w.err(format!("queue.sh release: no open batch for {}", c.r.name));
            return FAIL;
        }
    };
    if kv.get("owner").unwrap_or("") != "concierge" {
        w.err(format!("queue.sh release: {} is not claimed by concierge", c.r.name));
        return FAIL;
    }
    let (new, restore) = records::release_rewrite(&kv);
    if let Err(e) = write_atomic(&open, &new.render()) {
        w.err(format!("queue.sh release: {e}"));
        return FAIL;
    }
    let shown = if restore.is_empty() { "<none>" } else { restore.as_str() };
    w.out(format!("queue.sh release: {} released back to {shown}", c.r.name));
    OK
}
