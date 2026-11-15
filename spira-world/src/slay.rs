//! slay: stop one dwarf cleanly and make its bead say what is true. The decisions live here,
//! behind four ports (git, bead store, lifecycle, the dwarf's process), so every mode and
//! refusal runs in-process; `bin/slay.rs` holds only the real adapters.

use std::path::{Path, PathBuf};

pub const DEFAULT_WHY: &str = "slain by the operator";

pub fn usage_text() -> &'static str {
    r#"slay.sh — stop one dwarf cleanly and make its bead say what is true.

  slay.sh --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]

REQUIRED
  --bead <id>        The bead whose dwarf is to be stopped. Must exist in SPIRA_DB;
                     an id no bead carries is refused rather than acted on.

OPTIONAL
  --why "<text>"     What the note on the bead records as the operator's reason.
                     Default: "slain by the operator". This is where a sentence goes.
  --keep-work        Leave the branch and worktree exactly as they are.
  --close "<reason>" Close the bead with this reason instead of releasing it.
  --reopen           Release the bead as open and unassigned. This is the default.
  -h, --help         This text.

WHAT HAPPENS TO THE WORK WITHOUT --keep-work
  It is RETIRED, not lost. Uncommitted changes are salvaged to SPIRA_RUN/reaped as a
  patch and committed to the branch. With --reopen (the default) a branch carrying commits
  the base lacks stays at refs/heads/spira/<id> with its branch: label, so the next builder
  continues from it; the worktree is removed. With --close such a branch is parked at
  refs/slain/<id>, a real ref that survives gc; if parking fails the deletion is REFUSED.
  A branch holding nothing the base lacks is simply removed. Use --keep-work to leave the
  worktree in place too.

EXIT
  0  the dwarf was stopped and the bead says what is true
  1  something could not be done — the message names it
  2  usage: no --bead, an unknown flag, a positional argument, two --bead values,
     or a bead id the store does not carry
"#
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Reopen,
    Close(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub id: String,
    pub mode: Mode,
    pub keep: bool,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    Help,
}

/// A refusal of the command line: exit 2, nothing touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    Usage(String),
    Positional(String),
}

impl ArgError {
    /// What goes to stderr.
    pub fn message(&self) -> String {
        match self {
            ArgError::Usage(m) => format!("slay.sh: {m}\n{}", usage_text()),
            ArgError::Positional(w) => format!(
                "slay.sh: unexpected argument \"{w}\" — this tool takes named arguments only.\nslay.sh: the bead goes in --bead, a reason goes in --why.\n"
            ),
        }
    }
}

pub fn parse_args(argv: &[String]) -> Result<Parsed, ArgError> {
    let usage = |m: &str| ArgError::Usage(m.to_string());
    let mut id: Option<String> = None;
    let mut mode = Mode::Reopen;
    let mut keep = false;
    let mut why = DEFAULT_WHY.to_string();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bead" => {
                if let Some(prev) = &id {
                    return Err(ArgError::Usage(format!("--bead given twice ({prev}, {})", argv.get(i + 1).cloned().unwrap_or_default())));
                }
                id = Some(argv.get(i + 1).ok_or_else(|| usage("--bead needs a bead id"))?.clone());
                i += 2;
            }
            "--close" => {
                mode = Mode::Close(argv.get(i + 1).ok_or_else(|| usage("--close needs a reason"))?.clone());
                i += 2;
            }
            "--reopen" => {
                mode = Mode::Reopen;
                i += 1;
            }
            "--keep-work" => {
                keep = true;
                i += 1;
            }
            "--why" => {
                why = argv.get(i + 1).ok_or_else(|| usage("--why needs text"))?.clone();
                i += 2;
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            other if other.starts_with('-') => return Err(ArgError::Usage(format!("unknown flag {other}"))),
            other => return Err(ArgError::Positional(other.to_string())),
        }
    }
    let id = id.ok_or_else(|| usage("--bead is required"))?;
    Ok(Parsed::Run(Args { id, mode, keep, why }))
}

/// What the bead store says about one bead. Who holds it, and whether, is the lifecycle's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeadView {
    pub id: String,
    pub labels: Vec<String>,
}

pub trait Beads {
    /// `None` when the store has no answer.
    fn view(&self, id: &str) -> Option<BeadView>;
    fn clear_assignee(&self, id: &str);
    fn label_add(&self, id: &str, label: &str);
    fn label_remove(&self, id: &str, label: &str);
    fn note(&self, id: &str, text: &str);
    /// The checkout of the repository the bead names (or the home repository).
    fn repo_root_of(&self, id: &str) -> Option<PathBuf>;
}

pub trait Lifecycle {
    /// The lifecycle row's state (`WORKING`, `READY`, ...), `None` when it has no answer.
    fn state(&self, id: &str) -> Option<String>;
    fn disposition(&self, id: &str, why: &str);
    fn holder_dead(&self, id: &str);
    fn drop_bead(&self, id: &str, text: &str);
}

/// git, and the destruction chokepoint that owns `worktree remove` and `branch -D`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SalvageFailed;

pub trait Git {
    fn is_dir(&self, p: &Path) -> bool;
    fn branch_exists(&self, repo: &Path, br: &str) -> bool;
    fn short_tip(&self, repo: &Path, br: &str) -> Option<String>;
    fn landref(&self, repo: &Path) -> Option<String>;
    fn names_bead_between(&self, repo: &Path, base: &str, br: &str, id: &str) -> bool;
    fn ahead(&self, repo: &Path, base: &str, br: &str) -> u64;
    fn content_landed(&self, repo: &Path, br: &str, base: &str) -> bool;
    fn abort_inflight(&self, wt: &Path);
    fn commit_all(&self, wt: &Path, msg: &str) -> bool;
    fn park(&self, repo: &Path, refname: &str, br: &str) -> bool;
    /// `Ok(None)` nothing to save; `Err` salvage failed and the removal must not proceed.
    fn salvage(&self, id: &str, wt: &Path) -> Result<Option<PathBuf>, SalvageFailed>;
    fn destroy_worktree(&self, id: &str, wt: &Path, repo: &Path, why: &str) -> bool;
    fn destroy_branch(&self, id: &str, br: &str, repo: &Path, why: &str) -> Result<(), String>;
}

/// The dwarf (or manual hold) being stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stopped {
    pub name: String,
    pub pid: String,
    pub unit: String,
    pub fail: bool,
}

pub trait Dwarf {
    fn stop(&self, id: &str, say: &mut Vec<String>) -> Stopped;
    fn alive(&self, pid: &str) -> bool;
    fn pidfile_remains(&self, id: &str) -> bool;
}

pub struct Ports<'a> {
    pub git: &'a dyn Git,
    pub beads: &'a dyn Beads,
    pub lc: &'a dyn Lifecycle,
    pub dwarf: &'a dyn Dwarf,
}

pub struct Ctx<'a> {
    pub run: &'a Path,
    pub submitted_label: &'a str,
    pub store: &'a str,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub out: Vec<String>,
    pub err: Vec<String>,
    pub code: i32,
}

pub fn slay(ctx: &Ctx, p: &Ports, a: &Args) -> Report {
    let id = a.id.as_str();
    let mut r = Report::default();

    let found = p.beads.view(id).map(|v| v.id).unwrap_or_default();
    if found != id {
        r.err.push(format!("slay.sh: no bead {id} in {} — refusing to act", ctx.store));
        if !found.is_empty() {
            r.err.push(format!("slay.sh: the store answered with {found} instead (prefix match)"));
        }
        r.code = 2;
        return r;
    }

    p.lc.disposition(id, &a.why);

    let stopped = p.dwarf.stop(id, &mut r.out);
    let mut fail = stopped.fail;
    fail |= finish(ctx, p, a, &stopped, &mut r.out);

    if !stopped.pid.is_empty() && p.dwarf.alive(&stopped.pid) {
        fail = true;
    }
    if p.dwarf.pidfile_remains(id) {
        r.out.push(format!("verify: a pid file for {id} remains"));
        fail = true;
    }

    if fail {
        r.out.push(format!("slay: INCOMPLETE for {id} — see above"));
        r.code = 1;
    } else {
        r.out.push(format!("slain: {id}"));
    }
    r
}

/// Release the claim, retire the work, write the note. Returns whether anything failed.
fn finish(ctx: &Ctx, p: &Ports, a: &Args, s: &Stopped, out: &mut Vec<String>) -> bool {
    let id = a.id.as_str();
    let reopen = matches!(a.mode, Mode::Reopen);
    let mut fail = false;
    let mut say = |m: String| out.push(m);

    let repo = p.beads.repo_root_of(id);
    let br = format!("spira/{id}");
    let wt = ctx.run.join("worktree").join(id);

    let working = p.lc.state(id).is_some_and(|s| s == "WORKING");
    let carries_own = working
        && reopen
        && a.keep
        && repo.as_deref().is_some_and(|rp| p.git.landref(rp).is_some_and(|base| p.git.names_bead_between(rp, &base, &br, id)));
    if working {
        p.lc.holder_dead(id);
    }
    p.beads.clear_assignee(id);
    if carries_own {
        p.beads.label_add(id, ctx.submitted_label);
        say(format!("bead: {br} already carries a commit of {id} — reopened as submitted, not for redo"));
    }

    let mut tip = repo.as_deref().filter(|rp| p.git.branch_exists(rp, &br)).and_then(|rp| p.git.short_tip(rp, &br)).unwrap_or_default();
    let (mut nuked, mut saved) = (String::new(), String::new());
    let (mut wip, mut branch_kept) = (false, false);

    if a.keep {
        let at = if tip.is_empty() { String::new() } else { format!(" at {tip}") };
        say(format!("work: kept — branch {br}{at}, worktree {}", wt.display()));
    } else if let Some(repo) = repo.as_deref() {
        if p.git.is_dir(&wt) {
            p.git.abort_inflight(&wt);
            match p.git.salvage(id, &wt) {
                Ok(patch) => {
                    saved = patch.map(|x| x.display().to_string()).unwrap_or_default();
                    if !saved.is_empty() {
                        say(format!("work: uncommitted changes salvaged to {saved}"));
                        if p.git.commit_all(&wt, &format!("{id}: wip — salvaged at slay ({})", a.why)) {
                            wip = true;
                            tip = p.git.short_tip(repo, &br).unwrap_or_else(|| if tip.is_empty() { "?".into() } else { tip.clone() });
                            say(format!("work: wip committed to {br} at {tip} — branch stays in refs/heads"));
                        } else {
                            say("work: could not make wip commit — patch is the only copy".into());
                        }
                    }
                }
                Err(SalvageFailed) => {
                    say(format!("work: could not salvage {} — leaving it in place", wt.display()));
                    fail = true;
                }
            }
            if !fail {
                if p.git.destroy_worktree(id, &wt, repo, &format!("slain: {}", a.why)) {
                    say(format!("work: worktree {} removed", wt.display()));
                } else {
                    say(format!("work: could not remove {} — see {}/reap.log", wt.display(), ctx.run.display()));
                    fail = true;
                }
            }
        }
        let mut parked = String::new();
        if !tip.is_empty() && !fail && !wip {
            match p.git.landref(repo) {
                Some(base) => {
                    if p.git.ahead(repo, &base, &br) > 0 && !p.git.content_landed(repo, &br, &base) {
                        if reopen {
                            branch_kept = true;
                            say(format!("work: {br} carries work {base} does not — kept in refs/heads for the next builder"));
                        } else if p.git.park(repo, &format!("refs/slain/{id}"), &br) {
                            parked = format!("refs/slain/{id}");
                            say(format!("work: {br} carries work {base} does not — parked at {parked}"));
                        } else {
                            say(format!("work: could not park {br} at refs/slain/{id} — REFUSING to delete it"));
                            fail = true;
                        }
                    }
                }
                None if reopen => {
                    branch_kept = true;
                    say(format!("work: no base to compare {br} against — kept in refs/heads"));
                }
                None => {}
            }
        }
        if !tip.is_empty() && !fail && !wip && !branch_kept {
            match p.git.destroy_branch(id, &br, repo, &format!("slain: {}", a.why)) {
                Ok(()) => {
                    let kept = if parked.is_empty() { String::new() } else { format!(", kept at {parked}") };
                    nuked = format!("branch {br} deleted at {tip}{kept}");
                    say(format!("work: {nuked}"));
                }
                Err(e) => {
                    let why = if e.is_empty() { String::new() } else { format!(" — {e}") };
                    say(format!("work: could not delete {br}{why}"));
                    fail = true;
                }
            }
        }
    } else {
        say("work: bead names no resolvable repository — nothing to remove".into());
    }

    if !nuked.is_empty() {
        p.beads.label_remove(id, &format!("branch:{br}"));
    }
    let mut work_msg = format!("{}{}", if nuked.is_empty() { "work kept" } else { &nuked }, if saved.is_empty() { String::new() } else { format!("; uncommitted changes salvaged to {saved}") });
    if branch_kept {
        work_msg = format!("branch {br} kept in refs/heads with its commits");
    }
    if wip {
        work_msg = format!("wip committed to {br}; branch kept in refs/heads; patch at {saved}");
    }
    let dwarf = if s.name.is_empty() { "?" } else { &s.name };
    let pid = if s.pid.is_empty() { String::new() } else { format!(" (pid {})", s.pid) };
    let unit = if s.unit.is_empty() { String::new() } else { format!(" via {}", s.unit) };
    let note = format!("Slain by the operator: {}. Dwarf {dwarf}{pid} stopped{unit}. {work_msg}. No attempt charged.", a.why);
    match &a.mode {
        Mode::Close(reason) => {
            p.lc.drop_bead(id, &format!("{reason} — {note}"));
            p.beads.note(id, &format!("{reason} — {note}"));
        }
        Mode::Reopen => p.beads.note(id, &note),
    }

    if let Some(v) = p.beads.view(id) {
        let state = p.lc.state(id).unwrap_or_else(|| "?".into());
        say(format!("bead: {} state={state} labels={}", v.id, v.labels.join(",")));
    }

    let survives = repo.as_deref().is_some_and(|rp| p.git.branch_exists(rp, &br));
    if !(a.keep || wip || branch_kept || repo.is_none() || !survives) {
        say(format!("verify: {br} still exists"));
        fail = true;
    }
    fail
}

#[cfg(test)]
mod tests;
