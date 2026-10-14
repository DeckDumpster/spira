//! One module per subcommand family. Every operation returns the process exit code and
//! writes only through the ports in `World` (DESIGN.md §2.2).

pub mod batch;
pub mod deploy;
pub mod helpers;
pub mod land;
pub mod mend;
pub mod publish;
pub mod reconcile;
pub mod rebase;
pub mod round;
pub mod simple;
pub mod stage;
pub mod transition;
pub mod verdict;

use std::path::{Path, PathBuf};

use crate::cli::Text;
use crate::lock::{self, Acquire, Guard};
use crate::ports::{RepoCtx, Settings, World};

pub const OK: i32 = 0;
pub const FAIL: i32 = 1;
pub const USAGE: i32 = 2;
/// land-local's exit when the landing is recorded but its release was not activated.
pub const DEPLOY_FAULT: i32 = 3;

/// Resolved settings plus the named (or home) repository.
pub struct Ctx {
    pub s: Settings,
    pub r: RepoCtx,
}

impl Ctx {
    pub fn queue_file(&self, name: &str) -> PathBuf {
        self.s.queue_dir.join(&self.r.name).join(name)
    }
}

/// Resolve through seam R1; a failure of the seam itself is reported and is exit 1.
pub fn resolve(w: &World, label: &str, repo: Option<&str>) -> Result<Ctx, i32> {
    match w.lib.context(repo) {
        Ok((s, r)) => Ok(Ctx { s, r }),
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot resolve the harness configuration: {e}"));
            Err(FAIL)
        }
    }
}

/// The repository's checkout, or "no such repo" (exit 1).
pub fn repo_path(w: &World, label: &str, c: &Ctx) -> Result<PathBuf, i32> {
    match &c.r.path {
        Some(p) => Ok(p.clone()),
        None => {
            w.err(format!("queue.sh {label}: no such repo: {}", c.r.name));
            Err(FAIL)
        }
    }
}

/// spira-lc is the only record: an unreachable machine is a loud refusal before anything
/// changes.
pub fn require_lc(w: &World, label: &str) -> Result<(), i32> {
    let why = if w.lc.available() { w.lc.probe().err() } else { Some("no spira-lc program".into()) };
    match why {
        None => Ok(()),
        Some(e) => {
            w.err(format!(
                "queue.sh {label}: spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine"
            ));
            Err(FAIL)
        }
    }
}

/// Ask spira-lc for the batch's fresh (state, version) and run `f` against it; rc 1 with no
/// output when the batch row does not exist there.
pub fn lc_cas<F>(w: &World, batch_id: &str, f: F) -> Result<(), (i32, String)>
where
    F: FnOnce(&str, &str) -> Result<(), (i32, String)>,
{
    if !w.lc.available() {
        return Err((2, "no spira-lc program".into()));
    }
    match w.lc.batch_state(batch_id) {
        Some((state, version)) if !state.is_empty() => f(&state, &version),
        _ => Err((1, format!("spira-lc has no readable lifecycle row for batch {batch_id} (show-batch returned none)"))),
    }
}

const DEADLINE_TRIES: u32 = 3;

fn timed_out(e: &(i32, String)) -> bool {
    e.0 == 2
}

/// `lc_cas` that survives a deadline: a write that answered "cannot tell" may have landed, so
/// each retry re-reads the row first and `applied(state)` says whether the first try took.
pub fn lc_cas_settled<F, A>(w: &World, batch_id: &str, f: F, applied: A) -> Result<(), (i32, String)>
where
    F: Fn(&str, &str) -> Result<(), (i32, String)>,
    A: Fn(&World) -> bool,
{
    let mut last = Err((2, "no attempt".to_string()));
    for attempt in 0..DEADLINE_TRIES {
        if attempt > 0 {
            w.clock.sleep(u64::from(attempt));
            if applied(w) {
                return Ok(());
            }
        }
        last = lc_cas(w, batch_id, &f);
        match &last {
            Err(e) if timed_out(e) => continue,
            Err(_) if attempt > 0 && applied(w) => return Ok(()),
            _ => return last,
        }
    }
    last
}

pub fn lc_abandon(w: &World, batch_id: &str, actor: &str, reason: &str) -> Result<(), (i32, String)> {
    lc_cas_settled(
        w,
        batch_id,
        |s, v| w.lc.abandon_batch(batch_id, s, v, actor, reason),
        |w| matches!(w.lc.batch_state(batch_id), Some((s, _)) if matches!(s.as_str(), "ABANDONED" | "DISCARDED")),
    )
}

pub fn lc_eject_member(w: &World, batch_id: &str, bead: &str, actor: &str, reason: &str) -> Result<(), (i32, String)> {
    lc_cas_settled(
        w,
        batch_id,
        |s, v| w.lc.eject_member(batch_id, bead, s, v, actor, reason),
        |w| matches!(w.lc.bead_row(bead), Some(r) if r.state != "IN_DELIVERY"),
    )
}

/// The acting identity: SPIRA_QUEUE_ACTOR, BEADS_ACTOR, aeon-$SPIRA_AEON, USER, unknown.
pub fn actor(w: &World) -> String {
    w.var("SPIRA_QUEUE_ACTOR")
        .or_else(|| w.var("BEADS_ACTOR"))
        .or_else(|| w.var("SPIRA_AEON").map(|a| format!("aeon-{a}")))
        .or_else(|| w.var("USER"))
        .unwrap_or_else(|| "unknown".into())
}

/// The czar fence: a czar session with a class set runs czar-fence.sh first.
pub fn czar_ok(w: &World) -> bool {
    if w.var("SPIRA_FAYTH").as_deref() == Some("czar") {
        if let Some(class) = w.var("SPIRA_CZAR_CLASS") {
            if let Err(e) = crate::ident::check("czar class", &class) {
                w.err(format!("queue.sh: {e}"));
                return false;
            }
            return w.scripts.czar_fence(&class);
        }
    }
    true
}

/// Take the repo's queue lock, waiting up to SPIRA_QUEUE_LOCK_WAIT seconds, or print queue.sh's
/// refusal naming the recorded holder. `held_by_caller` is SPIRA_QUEUE_LOCK_HELD=1 (land-local,
/// publish, rollback-local only).
pub fn take_lock(w: &World, label: &str, c: &Ctx, busy_suffix: &str) -> Result<Option<Guard>, i32> {
    let mut waited = 0;
    loop {
        match lock::try_lock_as(&c.s.queue_dir, &c.r.name, label) {
            Acquire::Held(g) => return Ok(Some(g)),
            Acquire::Busy if waited < c.s.verdict.lock_wait => {
                w.clock.sleep(1);
                waited += 1;
            }
            Acquire::Busy => {
                let held = lock::holder(&c.s.queue_dir, &c.r.name);
                let held = if held.is_empty() { "holder not recorded".to_string() } else { held };
                w.err(format!(
                    "queue.sh {label}: another queue operation holds the lock for {}{busy_suffix} (waited {waited}s; {held})",
                    c.r.name
                ));
                return Err(FAIL);
            }
            Acquire::Unopenable => {
                w.err(format!("queue.sh {label}: cannot open lock file for {}", c.r.name));
                return Err(FAIL);
            }
        }
    }
}

pub fn lock_held_by_caller(w: &World) -> bool {
    w.env.var("SPIRA_QUEUE_LOCK_HELD").as_deref() == Some("1")
}

/// queue_owner_refused: refused (message printed) when the batch is concierge-owned and
/// the actor is not the concierge, unless SPIRA_QUEUE_OWNER_OVERRIDE=1.
pub fn owner_refused(w: &World, owner: &str, actor: &str, label: &str) -> bool {
    if w.env.var("SPIRA_QUEUE_OWNER_OVERRIDE").as_deref() == Some("1") {
        return false;
    }
    if owner != "concierge" || actor == "concierge" {
        return false;
    }
    w.err(format!("{label}: refused — this batch is claimed by concierge; override with SPIRA_QUEUE_OWNER_OVERRIDE=1"));
    true
}

/// The text a `--reason`/`--members` option named: argv, a file, or stdin.
pub fn read_text(w: &World, t: &Text) -> Result<String, String> {
    match t {
        Text::None => Ok(String::new()),
        Text::Arg(s) => Ok(s.clone()),
        Text::File(p) => w.env.read_file(p),
        Text::Stdin => w.env.read_stdin(),
    }
}

/// Validate identifiers before any of them reaches an argv (DESIGN.md §5).
pub fn idents(w: &World, label: &str, pairs: &[(&str, &str)]) -> Result<(), i32> {
    for (k, v) in pairs {
        if let Err(e) = crate::ident::check(k, v) {
            w.err(format!("queue.sh {label}: {e} — refused"));
            return Err(FAIL);
        }
    }
    Ok(())
}

/// Append one line to `$SPIRA_RUN/landing.log`, best-effort (as `>> … || true`).
pub fn landing_log(run: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(run.join("landing.log")) {
        let _ = writeln!(f, "{line}");
    }
}

/// A bead title for a PR body line, cut to 120 characters (queue.sh's python).
pub fn title_line(rows: &[crate::model::BeadRow], id: &str) -> String {
    let t = rows.iter().find(|r| r.id == id).and_then(|r| r.title.clone()).unwrap_or_default();
    if t.is_empty() {
        "(title unavailable)".into()
    } else {
        t.chars().take(120).collect()
    }
}
