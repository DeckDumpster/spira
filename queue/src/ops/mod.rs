//! One module per subcommand family. Every operation returns the process exit code and
//! writes only through the ports in `World` (DESIGN.md §2.2).

pub mod batch;
pub mod land;
pub mod publish;
pub mod simple;
pub mod transition;

use std::path::{Path, PathBuf};

use crate::cli::Text;
use crate::lock::{self, Acquire, Guard};
use crate::ports::{RepoCtx, Settings, World};

pub const OK: i32 = 0;
pub const FAIL: i32 = 1;
pub const USAGE: i32 = 2;

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

/// `lifecycle_enforce` — THE switch for everything touching the lifecycle machine
/// (DESIGN.md §10). Resolved as the aeon crate resolves it: the process environment's
/// `SPIRA_LIFECYCLE_ENFORCE` wins (a unit or a fixture pins it; `1`/`true` = on), else
/// `spira.lifecycle_enforce` in the document conf.sh resolved, read through the spira-config
/// library; else off. Binary presence is never consulted.
pub fn lifecycle_on(w: &World) -> bool {
    if let Some(v) = w.env.var("SPIRA_LIFECYCLE_ENFORCE") {
        return v == "1" || v == "true";
    }
    w.config.lifecycle_enforce(w.lib.toml_path().as_deref())
}

/// With the switch ON, spira-lc is authoritative: an unreachable machine is a loud refusal
/// before anything changes. Never called with the switch OFF.
pub fn require_lc(w: &World, label: &str) -> Result<(), i32> {
    let why = if w.lc.available() { w.lc.probe().err() } else { Some("SPIRA_LC_BIN is not an executable".into()) };
    match why {
        None => Ok(()),
        Some(e) => {
            w.err(format!(
                "queue.sh {label}: lifecycle_enforce is on and spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine or turn lifecycle_enforce off"
            ));
            Err(FAIL)
        }
    }
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

/// Take the repo's queue lock, or print queue.sh's refusal. `held_by_caller` is
/// SPIRA_QUEUE_LOCK_HELD=1 (land-local, publish, rollback-local only).
pub fn take_lock(w: &World, label: &str, c: &Ctx, busy_suffix: &str) -> Result<Option<Guard>, i32> {
    match lock::try_lock(&c.s.queue_dir, &c.r.name) {
        Acquire::Held(g) => Ok(Some(g)),
        Acquire::Busy => {
            w.err(format!("queue.sh {label}: another queue operation holds the lock for {}{busy_suffix}", c.r.name));
            Err(FAIL)
        }
        Acquire::Unopenable => {
            w.err(format!("queue.sh {label}: cannot open lock file for {}", c.r.name));
            Err(FAIL)
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
