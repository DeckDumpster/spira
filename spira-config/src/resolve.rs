//! `spira_config::resolve()` — the Rust home for `spira/conf.sh`'s `spira_conf_defaults`
//! (wave4-decomposition.md row C2, bead sp-eekjm/"wave 4.4"). One function, called
//! in-process by every Rust crate instead of each shelling out to a freshly-sourced
//! `conf.sh`; `spira-config resolve --sh` (`main.rs`) is the bash-facing form `conf.sh`
//! itself will `eval` once it is cut over (a LATER bead, sp-ubcgo — this one lands the
//! capability, not the cutover).
//!
//! THE PRECEDENCE, preserved exactly: the ENVIRONMENT, then the CONFIG FILE (`spira.toml`'s
//! `[spira]` table), then a DERIVED DEFAULT — and the first of those that speaks wins. Most
//! keys use bash's `:=` rule (a value counts as "spoken" only if it is non-empty); a dozen
//! hand-written keys (`SPIRA_DB`, `SPIRA_ACTIONABLE`, ...) use the no-colon `=` rule instead,
//! where an EXPLICITLY EMPTY value is itself an answer and survives untouched — see
//! `resolve_colon` vs `resolve_eq` below, and `spira/conf.sh`'s own comments at each key this
//! ports, which explain case by case why.
//!
//! TWO SOURCES OF DEFAULT:
//!   - About 30 keys (`SPIRA_HOME_REPO`, `SPIRA_INSTANCE`, `SPIRA_DB`, `SPIRA_WORKSPACES`,
//!     `SPIRA_REPO_MAP`, ...) have an ORDERING CONSTRAINT — they derive from a git call, a
//!     MANIFEST read, an instance suffix, or another of these same keys — and are hand-ported
//!     below, in the same order `spira_conf_defaults` computes them in bash. `conf.d`'s own
//!     registry marks each one `PROCEDURAL` for exactly this reason and carries no generated
//!     default for it.
//!   - The remaining ~220 keys have a plain `: "${KEY:=EXPR}"` statement in their own
//!     `spira/conf.d/<KEY>` file (sp-g3uwp) and are resolved generically, in the registry's
//!     own topological order, by [`crate::eval::eval_expr`] — see that module's doc for which
//!     bash shapes it understands and why that is deliberately narrow.
//!
//! `spira_containment_check` (wave4-decomposition.md (c) #5) runs as the LAST step, inside
//! this function, not as a separate call a caller could forget — exactly the property its own
//! design note asks for.
//!
//! THE GATE-OUTCOME CONSTANTS ARE HERE, AS FIXED VALUES (bead sp-wqj3o, "wave 4.10: small
//! conf.sh families") — `SPIRA_GATE_NOVERDICT`/`SPIRA_GATE_BASEFAIL` are computed below, not
//! read from `env`/`toml_map` the way every other key in this function is: `conf.sh`'s own
//! comment on them is explicit that they are "DELIBERATELY NOT SETTABLE, and so not in
//! SPIRA_CONF_KEYS" — a configurable NO_VERDICT could be set to 0, turning every withheld
//! verdict into a pass. They were never part of `spira_conf_defaults`'s own contract either
//! (bash computed them as top-level assignments, not inside that function); this bead gives
//! them a home anyway, since conf.sh no longer carries ANY hand-written config value once
//! this lands.
//!
//! WHAT IS STILL DELIBERATELY NOT HERE: `spira_unit`/`watch_unit_name` and the deps.toml
//! family (also bead sp-wqj3o, but living in [`crate::unit`]/[`crate::deps`] instead) —
//! both do their own I/O (a `systemctl` query, a second file read), which is exactly why
//! [`crate::env_bootstrap`] is already a separate module rather than part of this pure
//! function. Config WRITES (`spira_config_set`/`unset`, bead sp-ksrss) are a separate
//! family for the same reason.
//!
//! THE PATH TAIL AND `SPIRA_BD`/schema-preflight bootstrap this doc used to list here moved instead, in
//! sp-kfimz ("wave 4.6"), to [`crate::env_bootstrap`] — a separate module rather than part of
//! `resolve()` itself, because both are side-effecting (PATH depends on the process's own
//! ambient PATH, not only `spira.toml`; the schema preflight shells out to `bd` and writes a
//! cache stamp) where `resolve()` is a pure function of its [`ResolveInput`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{registry, spira_string_map, SpiraToml};

/// What `resolve` was given. Every per-copy fact (`home`, `repo`) is an explicit input, never
/// self-located — a compiled binary has no `BASH_SOURCE` to stand in for "where conf.sh
/// sits", and the caller (today, a human; after sp-ubcgo, `conf.sh` itself) already knows
/// both.
pub struct ResolveInput<'a> {
    /// The process environment — not only `SPIRA_*`/`COCKPIT_*` keys: `HOME`,
    /// `XDG_CONFIG_HOME` and `XDG_DATA_HOME` are read from here too, exactly as bash reads
    /// its own ambient environment.
    pub env: &'a BTreeMap<String, String>,
    /// `SPIRA_HOME` — the directory holding `conf.sh`'s own tree (`spira/`), i.e. the
    /// directory this call's `conf_d` normally sits under.
    pub home: &'a Path,
    /// `SPIRA_REPO` — the checkout (or release directory) `home` lives inside.
    pub repo: &'a Path,
    /// The already-validated `[spira]` document, or `None` when no `spira.toml` resolves —
    /// `resolve` does no locating of its own (`spira-config locate` already owns that).
    pub toml: Option<&'a SpiraToml>,
    /// `spira/conf.d` — normally [`default_conf_d`]`(home)`, broken out as its own field so a
    /// test can point it at a scratch registry without a scratch `SPIRA_HOME` to match.
    pub conf_d: &'a Path,
}

/// `home.join("conf.d")` — the registry directory `conf.sh` itself always uses relative to
/// its own file (never relative to a test's fixture symlink; see that file's own comment on
/// `_spira_conf_gen_ensure`).
pub fn default_conf_d(home: &Path) -> std::path::PathBuf {
    home.join("conf.d")
}

/// `SPIRA_REPO_DERIVED` (`spira/conf.sh`, right after `SPIRA_HOME` is settled, BEFORE the
/// env-override line) — the filesystem-only half of `SPIRA_REPO`'s derivation, kept separate
/// from [`derive_home_repo`] because `spira-config`'s own [`crate::repos::Registry`] reads
/// the two as DIFFERENT facts: an explicit `SPIRA_REPO` is a caller saying "this tree is the
/// harness in force" (a fixture, the gate's scratch checkout), while the derived value is
/// just where this file happens to sit, and `repo_root`'s own "self" match needs to tell
/// them apart. `git -C home rev-parse --show-toplevel`, with the git HOOK environment
/// scrubbed (`GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`/`GIT_PREFIX`) so a git hook that
/// sourced conf.sh does not resolve one level too deep (conf.sh's own comment on this exact
/// bug); otherwise `home`'s own parent directory, canonicalized the way `cd "$HOME/.." &&
/// pwd -P` is — NOT a plain `Path::parent()`, which would leave a trailing `..`-relative
/// symlink unresolved where bash's `pwd -P` does not. An unresolvable parent (home itself
/// does not exist) returns `home` itself, matching bash's `SPIRA_REPO_DERIVED=""` as a
/// non-empty placeholder a caller can still pass on rather than inventing its own
/// empty-path special case.
///
/// `env` is the EXPLICIT environment the git subprocess runs with (`.env_clear()` plus
/// exactly this map, scrubbed) — never this process's own ambient environment. Two
/// reasons: it is the same per-copy-fact discipline every other function here follows
/// (never reading `std::env` straight), and it is what makes the scrub itself testable —
/// a unit test can hand in a `GIT_DIR`-poisoned map without mutating the real process
/// environment, which a parallel `cargo test` run shares across every thread (the exact
/// race a `std::env::set_var`-based version of this test caused: another thread's own
/// unrelated `git` spawn, running concurrently with the lock this test held, inherited
/// the poison anyway, because inheritance does not consult any Rust-level lock).
pub fn derive_repo_filesystem(home: &Path, env: &BTreeMap<String, String>) -> std::path::PathBuf {
    let scrubbed = env
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "GIT_DIR" | "GIT_WORK_TREE" | "GIT_INDEX_FILE" | "GIT_PREFIX"));
    let toplevel = std::process::Command::new("git")
        .env_clear()
        .envs(scrubbed)
        .arg("-C")
        .arg(home)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    toplevel.unwrap_or_else(|| {
        home.join("..")
            .canonicalize()
            .unwrap_or_else(|_| home.to_path_buf())
    })
}

/// `SPIRA_REPO`'s own derivation (`spira/conf.sh`'s `SPIRA_REPO="${SPIRA_REPO:-$SPIRA_REPO_DERIVED}"`)
/// — ported so an in-process caller can build [`ResolveInput::repo`] itself instead of
/// shelling into bash for it (wave4-decomposition.md bead sp-mz7dn, "wave 4.8: retire conf
/// re-import seams"). `env`'s own `SPIRA_REPO` wins if set and non-empty — a caller's
/// explicit override; otherwise [`derive_repo_filesystem`]. A caller that also needs the
/// undiluted derived value (`repos::Registry` does — see [`derive_repo_filesystem`]'s own
/// doc) calls that function directly rather than trying to recover it from this one's
/// result.
pub fn derive_home_repo(home: &Path, env: &BTreeMap<String, String>) -> std::path::PathBuf {
    match env.get("SPIRA_REPO").filter(|s| !s.is_empty()) {
        Some(r) => std::path::PathBuf::from(r),
        None => derive_repo_filesystem(home, env),
    }
}

/// Everything a Rust crate needs to call [`resolve`] from inside its own process in one
/// step, replacing a `bash -c '. conf.sh; ...'` or `compgen -v` re-import seam
/// (wave4-decomposition.md bead sp-mz7dn, "wave 4.8"). `home`/`repo` are the caller's own
/// per-copy facts — [`resolve`] never self-locates them, and neither does this (see
/// [`derive_home_repo`] for `repo`, when the caller does not already have it). The
/// `spira.toml` in force is located the same way `spira-config locate` reports (no explicit
/// file pin): [`crate::discover`]`(None)` then [`crate::load`]; a config file that fails to
/// parse is a `String` error a caller can print and bail on, the same as every other
/// `spira_config` entry point already does, rather than a new enum variant [`resolve`]
/// itself would have to carry for an IO step it otherwise never performs.
pub fn resolve_for_process(
    home: &Path,
    repo: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Resolved, String> {
    resolve_process(home, repo, env, resolve)
}

fn resolve_process(
    home: &Path,
    repo: &Path,
    env: &BTreeMap<String, String>,
    run: fn(ResolveInput<'_>) -> Result<Resolved, ResolveError>,
) -> Result<Resolved, String> {
    let toml_path = crate::discover(None);
    let doc = match &toml_path {
        Some(p) => Some(crate::load(p)?),
        None => None,
    };
    let conf_d = default_conf_d(home);
    run(ResolveInput {
        env,
        home,
        repo,
        toml: doc.as_ref(),
        conf_d: &conf_d,
    })
    .map_err(|e| e.to_string())
}

/// The harness home used ONLY to judge an explicit `SPIRA_RUN`'s containment (sp-bp249
/// follow-up) — never for the ordinary derived-default path below, which keeps trusting the
/// caller's own `home` exactly as it always has. Climbs the same three rungs the rest of the
/// codebase climbs when `SPIRA_HOME` is absent: `$SPIRA_HOME` itself (if it carries a real
/// registry), else `$SPIRA_RELEASE/spira` (every unit that sets `SPIRA_RUN` without
/// `SPIRA_HOME` still carries `SPIRA_RELEASE` — law-a-binary-resolves-the-config-it-reads),
/// else the `home` the caller already handed in (its own best-effort locate). NEVER falls
/// through to an empty/relative path for [`registry::load`] to turn into the literal
/// "no config registry at conf.d" — when none of the three carries a `conf.d`, the refusal
/// names exactly which were missing or empty.
fn containment_home(env: &BTreeMap<String, String>, home: &Path) -> Result<PathBuf, String> {
    let has_registry = |p: &Path| !p.as_os_str().is_empty() && p.join("conf.d").is_dir();
    let spira_home = env.get("SPIRA_HOME").filter(|v| !v.is_empty());
    if let Some(h) = spira_home.map(PathBuf::from) {
        if has_registry(&h) {
            return Ok(h);
        }
    }
    let spira_release = env.get("SPIRA_RELEASE").filter(|v| !v.is_empty());
    if let Some(r) = spira_release {
        let h = PathBuf::from(r).join("spira");
        if has_registry(&h) {
            return Ok(h);
        }
    }
    if has_registry(home) {
        return Ok(home.to_path_buf());
    }
    Err(format!(
        "cannot resolve the harness home to check spira.run's containment: SPIRA_HOME is {}, \
         SPIRA_RELEASE is {}, and the fallback home ({}) has no conf.d",
        match spira_home {
            Some(h) => format!("set to {h} but it has no conf.d"),
            None => "unset".to_string(),
        },
        match spira_release {
            Some(r) => format!("set to {r} but {r}/spira has no conf.d"),
            None => "unset".to_string(),
        },
        if home.as_os_str().is_empty() { "none given".to_string() } else { home.display().to_string() },
    ))
}

/// [`resolve_for_process`] for a long-lived binary that must stay live on a bad config:
/// the error is named on stderr, then the empty [`Resolved`] is returned. Call once per start.
pub fn resolve_or_say(who: &str, home: &Path, repo: &Path, env: &BTreeMap<String, String>) -> Resolved {
    resolve_for_process(home, repo, env).unwrap_or_else(|e| {
        eprintln!("{who}: config resolve failed, running on built-in defaults: {e}");
        Resolved::default()
    })
}

/// `spira.run`, resolved the way every caller that needs the run directory now gets it:
/// [`resolve_for_process`] (env `SPIRA_RUN` wins outright, else `spira.toml`'s `run`, else
/// the derived XDG default) — never a bare literal
/// (law-a-binary-resolves-the-config-it-reads, sp-ivfu3: eight binaries read `env SPIRA_RUN
/// || "/tmp/spira"` and silently wrote/read `/tmp/spira` from a bare shell while every
/// systemd unit, which sets `SPIRA_RUN` itself, stayed correct). `home` is the caller's own
/// already-located `SPIRA_HOME` (or whatever fallback the caller's own `locate_home`
/// produces) — this function does no locating of its own, the same discipline
/// [`resolve_for_process`] itself keeps. Returns a named refusal, never a guessed path,
/// when `spira_config` cannot resolve at all (a malformed `spira.toml`) or resolves `run`
/// to an empty string (the derived default never does this, but a hand-written `spira.toml`
/// with `run = ""` could).
pub fn resolve_run_dir(env: &BTreeMap<String, String>, home: &Path) -> Result<PathBuf, String> {
    // An explicit SPIRA_RUN skips the repo-map walk (unrelated checkouts are not this key's
    // business) but is still judged itself: a confined instance may not name a run dir
    // outside its workspaces root. That judgement needs SPIRA_INSTANCE/SPIRA_WORKSPACES,
    // which means a REAL config registry — [`containment_home`], not the bare `home` the
    // caller handed in, since that is routinely whatever a caller's own best-effort
    // ancestor-walk produced (sp-bp249 follow-up: a release's `bin/<tool>` is routinely a
    // symlink into a build-artifacts tree with no `spira/` anywhere nearby, so the walk
    // comes back empty — and every caller of THIS function already had `SPIRA_RELEASE` in
    // its own environment the whole time, per law-a-binary-resolves-the-config-it-reads).
    if let Some(v) = env.get("SPIRA_RUN").filter(|v| !v.is_empty()) {
        let home = containment_home(env, home)?;
        let repo = derive_home_repo(&home, env);
        let resolved = resolve_process(&home, &repo, env, resolve_unchecked)
            .map_err(|e| format!("cannot resolve spira.run: {e}"))?;
        crate::containment::check_path(
            resolved.get("SPIRA_INSTANCE"),
            resolved.get("SPIRA_WORKSPACES"),
            "SPIRA_RUN",
            v,
        )?;
        return Ok(PathBuf::from(v));
    }
    let repo = derive_home_repo(home, env);
    let resolved = resolve_for_process(home, &repo, env).map_err(|e| format!("cannot resolve spira.run: {e}"))?;
    let run = resolved.get("SPIRA_RUN");
    if run.is_empty() {
        return Err("spira.run resolved empty — refusing to guess a run directory".to_string());
    }
    Ok(PathBuf::from(run))
}

/// The ask label, resolved in-process: a non-empty `SPIRA_ASK_LABEL` in `env` overrides,
/// otherwise `spira.toml` / the registry default. Never a literal fallback — a systemd unit
/// does not export it, and a guessed label hides asks from the operator's queue.
pub fn resolve_ask_label(env: &BTreeMap<String, String>, home: &Path) -> Result<String, String> {
    if let Some(v) = env.get("SPIRA_ASK_LABEL").filter(|v| !v.is_empty()) {
        return Ok(v.clone());
    }
    let repo = derive_home_repo(home, env);
    let resolved = resolve_for_process(home, &repo, env).map_err(|e| format!("cannot resolve ask label: {e}"))?;
    let v = resolved.get("SPIRA_ASK_LABEL");
    if v.is_empty() {
        return Err("ask label resolved empty — refusing to guess one".to_string());
    }
    Ok(v.to_string())
}

/// `spira.instance`, resolved the same way [`resolve_run_dir`] resolves `spira.run` — in
/// process, through [`resolve_for_process`], never read as a bare `$SPIRA_INSTANCE` with a
/// literal default (sp-ivfu3's widening: a bare shell with `SPIRA_INSTANCE` unset named
/// `spira-sentinel.timer` instead of the installed `spira-sentinel-prod.timer`, because the
/// caller's own fallback skipped config and read the environment variable alone). The
/// registry's own default is `"prod"` (`resolve`'s `ok_str!("prod")`), so this only refuses
/// when `spira_config` cannot resolve at all or resolves to an empty string.
pub fn resolve_instance(env: &BTreeMap<String, String>, home: &Path) -> Result<String, String> {
    // The env override alone, first — same reasoning as [`resolve_run_dir`]'s own doc:
    // `SPIRA_INSTANCE`'s own rule is also `:=` (`resolve_colon`), and a caller that
    // already has it explicitly has no need of the containment check the rest of
    // `resolve` runs unconditionally, over repos this key never touches.
    if let Some(v) = env.get("SPIRA_INSTANCE").filter(|v| !v.is_empty()) {
        return Ok(v.clone());
    }
    let repo = derive_home_repo(home, env);
    let resolved = resolve_for_process(home, &repo, env).map_err(|e| format!("cannot resolve spira.instance: {e}"))?;
    let instance = resolved.get("SPIRA_INSTANCE");
    if instance.is_empty() {
        return Err("spira.instance resolved empty — refusing to guess".to_string());
    }
    Ok(instance.to_string())
}

/// Every resolved key, plus any warning `resolve` itself produced (today, only the
/// `SPIRA_CLAUDE` deprecation notice) — a caller prints these to stderr; `resolve` itself
/// never writes anywhere.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub values: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

impl Resolved {
    /// The value for `key`, or `""` — matching bash's own "an unset or never-computed
    /// variable reads as empty" rule, not an `Option`, because every caller of a resolved
    /// config key already treats absence and emptiness the same way.
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }

    /// `KEY='value'` lines, one per name in `keys` that this resolution actually produced a
    /// value for — `spira-config resolve --sh`'s whole output. A name in `keys` that was
    /// never resolved (an export this bead does not yet cover; see the module doc's "what is
    /// deliberately not here") is silently skipped rather than emitted empty, so a later bead
    /// extending the export set cannot be mistaken for this one having already covered it.
    pub fn to_sh(&self, keys: &[&str]) -> String {
        let mut out = String::new();
        for key in keys {
            if let Some(v) = self.values.get(*key) {
                out.push_str(key);
                out.push('=');
                out.push_str(&crate::shell_quote(v));
                out.push('\n');
            }
        }
        out
    }

    /// Every resolved key, not just [`EXPORT_KEYS`] — `KEY='value'` lines in `values`'s own
    /// (sorted) order. This is `conf.sh`'s own `eval` target (sp-ubcgo, "wave 4.5"): a bash
    /// caller that SOURCES this process's output, rather than one that INHERITS it across an
    /// exec boundary, is exactly the "read in-process" case `EXPORT_KEYS`'s own doc carves
    /// out for `SPIRA_REPO_MAP`, `SPIRA_FAYTHS` and `SPIRA_MAX_AEONS` — so this method is the
    /// one place those three (and every other registry key `to_sh(EXPORT_KEYS)` leaves out)
    /// do reach a bash reader. `conf.sh` itself decides, separately and explicitly, which of
    /// these it then re-exports to its OWN children — that decision is `EXPORT_KEYS`, applied
    /// in bash after this `eval`, not here.
    pub fn to_sh_all(&self) -> String {
        let mut out = String::new();
        for (key, v) in &self.values {
            out.push_str(key);
            out.push('=');
            out.push_str(&crate::shell_quote(v));
            out.push('\n');
        }
        out
    }
}

/// `spira-config resolve --sh`'s typed export set — the Rust replacement for `conf.sh`'s own
/// hand-maintained `export KEY \ KEY \ ...` block (wave4-decomposition.md row C6). Every name
/// here is exported there TODAY. `SPIRA_GATE_NOVERDICT`/`SPIRA_GATE_BASEFAIL` joined this
/// list in bead sp-wqj3o ("wave 4.10"), now that `resolve` itself computes them (see the
/// module doc above) — the other three names `conf.sh`'s block exports but this function
/// still never computes (`SPIRA_CONF_FILE`, `SPIRA_TOML_FILE`, `SPIRA_SYSTEMCTL` — set
/// elsewhere in `conf.sh`) stay deliberately left out: adding them here would claim a
/// contract this function does not keep.
///
/// DELIBERATELY ABSENT, even though `resolve` computes them: `SPIRA_REPO_MAP`, `SPIRA_FAYTHS`
/// and `SPIRA_MAX_AEONS` are host policy, read in-process, never exported to a child
/// (`SPIRA_FAYTHS` leaking to a test's own sentinel is the scar `law-gates-run-in-a-clean-
/// environment` is named for). `SPIRA_HOME` and `SPIRA_REPO` are per-copy facts and are not
/// even IN `resolve`'s output (see [`ResolveInput`]) — exporting "where this copy of the
/// harness happens to sit" is exactly what broke the eleven tests `conf.sh`'s own comment
/// describes.
pub const EXPORT_KEYS: &[&str] = &[
    "COCKPIT_BOTTOM_PCT",
    "COCKPIT_CLIENT_IDLE_SECS",
    "COCKPIT_CLIPBOARD",
    "COCKPIT_CWD",
    "COCKPIT_DB",
    "COCKPIT_MAIL",
    "COCKPIT_MOUSE",
    "COCKPIT_RIGHT_PCT",
    "SPIRA_ALERT_GLOB",
    "SPIRA_ASK_LABEL",
    "SPIRA_BD",
    "SPIRA_CI_LABEL",
    "SPIRA_CI_PARK_MAX",
    "SPIRA_CTRL",
    "SPIRA_CUTOVER_ROUND_LABEL",
    "SPIRA_DB",
    "SPIRA_DESIGN",
    "SPIRA_DOLT_DATA",
    "SPIRA_EXPORTER",
    "SPIRA_EXPRESS_LABEL",
    "SPIRA_FLAKY_GH_REPO",
    "SPIRA_GATE_BASEFAIL",
    "SPIRA_GATE_NOVERDICT",
    "SPIRA_GH_INTAKE_BEAD_REPO",
    "SPIRA_GH_INTAKE_PRIORITY",
    "SPIRA_GH_INTAKE_REPO",
    "SPIRA_GROOM_ASK_LABEL",
    "SPIRA_ID_PREFIX",
    "SPIRA_INCIDENT_LABEL",
    "SPIRA_INCIDENT_PRIORITY",
    "SPIRA_INSTANCE",
    "SPIRA_LAND_GATE_RESERVE",
    "SPIRA_LAND_MAXSEC",
    "SPIRA_LC_PASSWORD_FILE",
    "SPIRA_LC_SOCKET",
    "SPIRA_LC_TESTDB_DATA",
    "SPIRA_LC_TESTDB_PORT",
    "SPIRA_LC_UNIX_GROUP",
    "SPIRA_LC_UNIX_USER",
    "SPIRA_LIFECYCLE_ENFORCE",
    "SPIRA_LOOM_ADDR",
    "SPIRA_LOOM_BUDGET_MS",
    "SPIRA_LOOM_CACHE_S",
    "SPIRA_LOOM_READY_GRACE",
    "SPIRA_MAIL",
    "SPIRA_MAIL_INDEX",
    "SPIRA_MAIL_KINDS",
    "SPIRA_MAIL_MUTE",
    "SPIRA_MAIL_REPEAT_WINDOW",
    "SPIRA_MAIL_SESSION_MAILBOX",
    "SPIRA_MAIL_TIDY_FRESH",
    "SPIRA_MIRROR",
    "SPIRA_NO_LOOP_LABEL",
    "SPIRA_OPERATOR",
    "SPIRA_OPERATOR_ACTOR",
    "SPIRA_PATH",
    "SPIRA_PLAN_LABEL",
    "SPIRA_PROD",
    "SPIRA_RECLAIM_GRACE_SECS",
    "SPIRA_RELEASES",
    "SPIRA_RELEASE_REPO",
    "SPIRA_REVIEWER_MODEL",
    "SPIRA_REVIEWER_VERDICTS",
    "SPIRA_REVIEW_LABEL",
    "SPIRA_RUN",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_SPIKE_DIR",
    "SPIRA_SPIKE_LABEL",
    "SPIRA_SPIKE_PATHS",
    "SPIRA_SUBMITTED_LABEL",
    "SPIRA_TESTDB_BD",
    "SPIRA_TESTDB_DATA",
    "SPIRA_TESTDB_PORT",
    "SPIRA_TESTENV_CPUS",
    "SPIRA_TESTENV_MAX_CONCURRENT",
    "SPIRA_TESTENV_QUEUE_POLL",
    "SPIRA_TESTENV_QUEUE_TIMEOUT",
    "SPIRA_TESTENV_REGISTRY",
    "SPIRA_TOWN",
    "SPIRA_TZ",
    "SPIRA_WIKI",
    "SPIRA_WIKI_HOOK",
    "SPIRA_WORKSPACES",
    "SPIRA_WORK_CLOSE_TYPES",
];

/// A containment violation (`spira_containment_check` failed) is the one way `resolve` itself
/// refuses outright, matching bash's `exit 1` — everything else about a key that cannot be
/// computed (an unsupported registry expression, a cyclic registry) is also a hard refusal,
/// never a silent empty string, so a broken `conf.d` file fails the resolve that reads it
/// instead of quietly shipping a wrong default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `spira_containment_check` found one or more registered checkouts this confined
    /// instance may not touch.
    Containment(Vec<String>),
    /// The registry itself, or one key's default expression, could not be read or evaluated.
    Registry(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Containment(v) => write!(f, "{}", v.join("\n")),
            ResolveError::Registry(e) => write!(f, "{e}"),
        }
    }
}

/// [`spira_string_map`]'s raw `UPPER_FIELD -> value` map, renamed to the `SPIRA_*`/`COCKPIT_*`
/// spelling every other key in this module uses — `conf.sh`'s own `spira_toml_read` does this
/// same rename (`case "$SPIRA_CONF_KEYS" in *" SPIRA_$key "*) key="SPIRA_$key" ;; esac`) because
/// `spira-config export --sh` names a `[spira]` field as-is, with no prefix, and this file's
/// own convention is that every key is `SPIRA_`-prefixed except the `COCKPIT_*` ones already
/// spelled that way in the struct. No SpiraSection field is itself named `spira_*`, so the
/// rename is unconditional here rather than gated on allowlist membership as bash's is — a
/// renamed key nothing in this module ever looks up by name is simply never read, which is
/// no different from bash's own gate silently doing nothing for the same key.
fn toml_key_map(doc: &SpiraToml) -> BTreeMap<String, String> {
    spira_string_map(doc)
        .into_iter()
        .map(|(k, v)| if k.starts_with("COCKPIT_") { (k, v) } else { (format!("SPIRA_{k}"), v) })
        .collect()
}

fn seed(key: &str, env: &BTreeMap<String, String>, toml_map: &BTreeMap<String, String>) -> Option<String> {
    env.get(key).cloned().or_else(|| toml_map.get(key).cloned())
}

/// The `:=` rule: the seed counts only if it is non-empty: an env or toml value of `""`
/// defaults exactly as if neither had spoken.
fn resolve_colon(
    key: &str,
    env: &BTreeMap<String, String>,
    toml_map: &BTreeMap<String, String>,
    default: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    match seed(key, env, toml_map) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => default(),
    }
}

/// The `=` (no-colon) rule: an explicitly empty seed is itself an answer and the default is
/// never consulted.
fn resolve_eq(
    key: &str,
    env: &BTreeMap<String, String>,
    toml_map: &BTreeMap<String, String>,
    default: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    match seed(key, env, toml_map) {
        Some(v) => Ok(v),
        None => default(),
    }
}

fn xdg_config_home(env: &BTreeMap<String, String>) -> String {
    match env.get("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => format!("{}/.config", env.get("HOME").cloned().unwrap_or_default()),
    }
}

/// Where the same-user `spira_lc` credential lives when the config does not say; its
/// read-only sibling is this path with `-ro` appended.
pub fn lc_credential_default(env: &BTreeMap<String, String>) -> String {
    format!("{}/spira/spira-lc.credential", xdg_config_home(env))
}

fn xdg_data_home(env: &BTreeMap<String, String>) -> String {
    match env.get("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => format!("{}/.local/share", env.get("HOME").cloned().unwrap_or_default()),
    }
}

fn basename(p: &str) -> String {
    Path::new(p.trim_end_matches('/'))
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn run_git_stdout(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_manifest_field(repo: &Path, field: &str) -> Option<String> {
    let text = std::fs::read_to_string(repo.join("MANIFEST")).ok()?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(k) = parts.next() else { continue };
        if k == field {
            return parts.next().map(str::to_string);
        }
    }
    None
}

/// `*-YYYYMMDDTHHMMSSZ` — an installed release directory's own name, which changes on every
/// upgrade and so cannot stand in for `SPIRA_HOME_REPO`'s identity (`conf.sh`'s own comment,
/// `law-scope-is-a-runtime-key`).
fn looks_like_release_timestamp(s: &str) -> bool {
    let Some(dash) = s.rfind('-') else { return false };
    let suffix = &s[dash + 1..];
    let b = suffix.as_bytes();
    b.len() == 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn compute_home_repo_default(repo: &Path) -> String {
    let repo_str = repo.to_string_lossy().to_string();
    if let Some(gcd) = run_git_stdout(&["-C", &repo_str, "rev-parse", "--git-common-dir"]) {
        return if gcd.starts_with('/') {
            basename(&crate::eval::sh_dirname(&gcd))
        } else {
            basename(&repo_str)
        };
    }
    if let Some(v) = read_manifest_field(repo, "repo") {
        if !v.is_empty() {
            return v;
        }
    }
    let base = basename(&repo_str);
    if looks_like_release_timestamp(&base) {
        String::new()
    } else {
        base
    }
}

/// `_spira_repo_file` (bash `spira_conf_file`): the legacy `spira.conf` path in force, or
/// `None` — an explicit `SPIRA_CONF` pin that does not exist means "no file", not "keep
/// looking" (same exclusive-pin rule `spira-config locate` already enforces for
/// `SPIRA_TOML`).
fn legacy_conf_file(env: &BTreeMap<String, String>) -> Option<String> {
    if let Some(p) = env.get("SPIRA_CONF") {
        return Path::new(p).is_file().then(|| p.clone());
    }
    for c in [
        format!("{}/spira/spira.conf", xdg_config_home(env)),
        "/etc/spira/spira.conf".to_string(),
    ] {
        if Path::new(&c).is_file() {
            return Some(c);
        }
    }
    None
}

/// `_spira_repo_map_candidate`: an explicit `SPIRA_HOME` (every Rust caller's is, by
/// construction — see [`ResolveInput`]'s own doc) puts its own `repo-map` first, then the
/// legacy conf file's directory, then the tracked `repo-map.example`.
fn repo_map_candidate(env: &BTreeMap<String, String>, home: &str) -> Option<String> {
    let legacy_dir = legacy_conf_file(env).map(|c| crate::eval::sh_dirname(&c));
    let mut candidates = vec![format!("{home}/repo-map")];
    if let Some(d) = legacy_dir {
        candidates.push(format!("{d}/repo-map"));
    }
    candidates.push(format!("{home}/repo-map.example"));
    candidates.into_iter().find(|c| Path::new(c).is_file())
}

/// `spira_conf_defaults` plus `spira_containment_check`, ported.
pub fn resolve(input: ResolveInput<'_>) -> Result<Resolved, ResolveError> {
    let resolved = resolve_unchecked(input)?;
    let repo_map_text = crate::containment::read_repo_map(Path::new(resolved.get("SPIRA_REPO_MAP")));
    crate::containment::check(
        resolved.get("SPIRA_INSTANCE"),
        resolved.get("SPIRA_WORKSPACES"),
        repo_map_text.as_deref(),
    )
    .map_err(ResolveError::Containment)?;
    Ok(resolved)
}

/// Every key, with no containment judgement; callers go through [`resolve`].
fn resolve_unchecked(input: ResolveInput<'_>) -> Result<Resolved, ResolveError> {
    let env = input.env;
    let toml_map = input.toml.map(toml_key_map).unwrap_or_default();
    let home_str = input.home.to_string_lossy().to_string();
    let repo_str = input.repo.to_string_lossy().to_string();

    // `known` backs every `$VAR` substitution the hand-written defaults and the registry's
    // own expressions make. SPIRA_HOME/SPIRA_REPO seed it so those substitutions resolve —
    // but neither is ever inserted into `values` (see EXPORT_KEYS's doc and ResolveInput's).
    let mut known: BTreeMap<String, String> = BTreeMap::new();
    for ambient in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(v) = env.get(ambient) {
            known.insert(ambient.to_string(), v.clone());
        }
    }
    known.insert("SPIRA_HOME".to_string(), home_str.clone());
    known.insert("SPIRA_REPO".to_string(), repo_str.clone());

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    macro_rules! set {
        ($key:expr, $val:expr) => {{
            let v = $val;
            known.insert($key.to_string(), v.clone());
            values.insert($key.to_string(), v);
        }};
    }
    macro_rules! ok_str {
        ($s:expr) => {
            || Ok::<String, String>($s.to_string())
        };
    }

    // GATE OUTCOME CONSTANTS. Fixed, never read from `env` or `toml_map` — see this
    // module's own doc. 75 is EX_TEMPFAIL ("try again"), 76 is EX_UNAVAILABLE ("the service
    // is not available"); both outside the range a gate command of its own would return.
    set!("SPIRA_GATE_NOVERDICT", "75".to_string());
    set!("SPIRA_GATE_BASEFAIL", "76".to_string());

    // 1. SPIRA_HOME_REPO
    let home_repo = resolve_colon("SPIRA_HOME_REPO", env, &toml_map, || {
        Ok(compute_home_repo_default(input.repo))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_HOME_REPO", home_repo.clone());

    // 2. SPIRA_INSTANCE
    let instance =
        resolve_colon("SPIRA_INSTANCE", env, &toml_map, ok_str!("prod")).map_err(ResolveError::Registry)?;
    set!("SPIRA_INSTANCE", instance.clone());
    let inst_sfx = if instance == "prod" { String::new() } else { format!("-{instance}") };

    // 3. SPIRA_DB (preserve-empty)
    let db = resolve_eq("SPIRA_DB", env, &toml_map, || {
        Ok(format!("{}/spira{inst_sfx}/db", xdg_data_home(env)))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_DB", db);

    // 4. SPIRA_RUN
    let run = resolve_colon("SPIRA_RUN", env, &toml_map, || {
        Ok(format!("{}/spira{inst_sfx}/run", xdg_data_home(env)))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_RUN", run.clone());

    // 5. SPIRA_MEMORIES_CACHE (preserve-empty; not exported — see EXPORT_KEYS)
    let mc = resolve_eq("SPIRA_MEMORIES_CACHE", env, &toml_map, || {
        Ok(format!("{run}/memories-cache.json"))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_MEMORIES_CACHE", mc);

    // 6. SPIRA_MEMORIES_CACHE_AGE
    let mca = resolve_colon("SPIRA_MEMORIES_CACHE_AGE", env, &toml_map, ok_str!("300"))
        .map_err(ResolveError::Registry)?;
    set!("SPIRA_MEMORIES_CACHE_AGE", mca);

    // 7. SPIRA_WORKSPACES
    let is_git_checkout = input.repo.join(".git").exists();
    let workspaces = resolve_colon("SPIRA_WORKSPACES", env, &toml_map, || {
        Ok(if is_git_checkout {
            crate::eval::sh_dirname(&repo_str)
        } else {
            crate::eval::sh_dirname(&crate::eval::sh_dirname(&repo_str))
        })
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_WORKSPACES", workspaces.clone());

    // 8. SPIRA_ACTIONABLE (preserve-empty)
    let actionable = resolve_eq("SPIRA_ACTIONABLE", env, &toml_map, ok_str!(
        "ANSWERED|COMMENTED|ESCALAT|STRANDED|POISON|DEGRADED|BLOCKED|UNREACHABLE|FAIL|ERROR|LANDED|\u{26a0} BRANCH"
    ))
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_ACTIONABLE", actionable);

    // 9. SPIRA_WAKE (preserve-empty; conditional default)
    let wake = match seed("SPIRA_WAKE", env, &toml_map) {
        Some(v) => v,
        None => {
            let candidate = format!("{repo_str}/concierge.sh");
            if is_executable(&candidate) {
                format!("{candidate} wake")
            } else {
                String::new()
            }
        }
    };
    set!("SPIRA_WAKE", wake);

    // 10-14. The spira-lc family (fixed literals; no cross-reference but SPIRA_WORKSPACES).
    let lc_testdb_data = resolve_colon("SPIRA_LC_TESTDB_DATA", env, &toml_map, || {
        Ok(crate::eval::spira_join(&workspaces, "lc-test"))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_LC_TESTDB_DATA", lc_testdb_data);
    set!(
        "SPIRA_LC_TESTDB_PORT",
        resolve_colon("SPIRA_LC_TESTDB_PORT", env, &toml_map, ok_str!("3309"))
            .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_LC_UNIX_USER",
        resolve_colon("SPIRA_LC_UNIX_USER", env, &toml_map, ok_str!("spira-lc"))
            .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_LC_UNIX_GROUP",
        resolve_colon("SPIRA_LC_UNIX_GROUP", env, &toml_map, ok_str!("spira"))
            .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_LC_PASSWORD_FILE",
        resolve_colon("SPIRA_LC_PASSWORD_FILE", env, &toml_map, || {
            let path = lc_credential_default(env);
            Ok(if Path::new(&path).is_file() { path } else { String::new() })
        })
        .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_LC_SOCKET",
        resolve_colon("SPIRA_LC_SOCKET", env, &toml_map, ok_str!("/run/spira-lc/sock"))
            .map_err(ResolveError::Registry)?
    );

    // 15. SPIRA_GH_INTAKE_REPO
    let gh_intake_repo =
        resolve_colon("SPIRA_GH_INTAKE_REPO", env, &toml_map, ok_str!("")).map_err(ResolveError::Registry)?;
    set!("SPIRA_GH_INTAKE_REPO", gh_intake_repo.clone());

    // 16. SPIRA_RELEASE_REPO (+ MANIFEST fallback)
    let mut release_repo = resolve_colon("SPIRA_RELEASE_REPO", env, &toml_map, || Ok(gh_intake_repo.clone()))
        .map_err(ResolveError::Registry)?;
    if release_repo.is_empty() {
        if let Some(v) = read_manifest_field(input.repo, "release-repo") {
            release_repo = v;
        }
    }
    set!("SPIRA_RELEASE_REPO", release_repo);

    set!(
        "SPIRA_SCCACHE_DAV_ADDR",
        resolve_colon("SPIRA_SCCACHE_DAV_ADDR", env, &toml_map, ok_str!("")).map_err(ResolveError::Registry)?
    );

    // 17-19. Rebase / certify literals.
    set!(
        "SPIRA_CERTIFY_SUITES",
        resolve_colon("SPIRA_CERTIFY_SUITES", env, &toml_map, ok_str!("on")).map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_REBASE_DECOMPOSE_FILES",
        resolve_colon("SPIRA_REBASE_DECOMPOSE_FILES", env, &toml_map, ok_str!("4"))
            .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_REBASE_GENERATED_FILES",
        resolve_colon(
            "SPIRA_REBASE_GENERATED_FILES",
            env,
            &toml_map,
            ok_str!("coverage.json COVERAGE.md standard-operating-procedures.md spira-config/schema")
        )
        .map_err(ResolveError::Registry)?
    );

    // 20. SPIRA_SCOPE_LABEL (preserve-empty)
    let scope_label = resolve_eq("SPIRA_SCOPE_LABEL", env, &toml_map, || Ok(home_repo.clone()))
        .map_err(ResolveError::Registry)?;
    set!("SPIRA_SCOPE_LABEL", scope_label);

    // 21-22. Czar outcome windows.
    set!(
        "SPIRA_CZAR_OUTCOME_MINS",
        resolve_colon("SPIRA_CZAR_OUTCOME_MINS", env, &toml_map, ok_str!("30"))
            .map_err(ResolveError::Registry)?
    );
    set!(
        "SPIRA_CZAR_UNCLAIMED_MINS",
        resolve_colon("SPIRA_CZAR_UNCLAIMED_MINS", env, &toml_map, ok_str!("10"))
            .map_err(ResolveError::Registry)?
    );

    // 23-24. Cockpit mail pane.
    set!(
        "COCKPIT_MAIL",
        resolve_eq("COCKPIT_MAIL", env, &toml_map, ok_str!("aerc")).map_err(ResolveError::Registry)?
    );
    set!(
        "COCKPIT_CLIENT_IDLE_SECS",
        resolve_colon("COCKPIT_CLIENT_IDLE_SECS", env, &toml_map, ok_str!("21600"))
            .map_err(ResolveError::Registry)?
    );

    // 25. SPIRA_DOLT_DATA (preserve-empty; NOT instance-suffixed, unlike SPIRA_DB/SPIRA_RUN)
    let dolt_data = resolve_eq("SPIRA_DOLT_DATA", env, &toml_map, || {
        Ok(format!("{}/spira/dolt", xdg_data_home(env)))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_DOLT_DATA", dolt_data);

    // 26. SPIRA_TESTDB_LIB
    let testdb_lib = resolve_colon("SPIRA_TESTDB_LIB", env, &toml_map, || {
        Ok(format!("{}/testdb.sh", basename(&home_str)))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_TESTDB_LIB", testdb_lib);

    // 27. SPIRA_RELEASES
    let releases = resolve_colon("SPIRA_RELEASES", env, &toml_map, || {
        Ok(crate::eval::spira_join(&workspaces, "spira-releases"))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_RELEASES", releases.clone());

    // 28. SPIRA_PROD (preserve-empty)
    let prod = resolve_eq("SPIRA_PROD", env, &toml_map, || {
        Ok(crate::eval::spira_join(&releases, "current/spira"))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_PROD", prod);

    // 29. SPIRA_AGENT (+ SPIRA_CLAUDE deprecation)
    let agent_seed = seed("SPIRA_AGENT", env, &toml_map).filter(|v| !v.is_empty());
    let claude_val = env.get("SPIRA_CLAUDE").cloned().unwrap_or_default();
    let agent = match agent_seed {
        Some(v) => v,
        None if !claude_val.is_empty() => {
            warnings.push("spira: SPIRA_CLAUDE is deprecated; rename it to SPIRA_AGENT in spira.conf".to_string());
            claude_val
        }
        None => "claude".to_string(),
    };
    set!("SPIRA_AGENT", agent);

    // 30. SPIRA_REPO_MAP
    let repo_map = resolve_colon("SPIRA_REPO_MAP", env, &toml_map, || {
        Ok(repo_map_candidate(env, &home_str).unwrap_or_else(|| format!("{home_str}/repo-map")))
    })
    .map_err(ResolveError::Registry)?;
    set!("SPIRA_REPO_MAP", repo_map.clone());

    // The generic registry pass: every remaining `spira/conf.d/<KEY>` not already resolved
    // above, in topological order.
    let registry = registry::load(input.conf_d).map_err(ResolveError::Registry)?;
    let order = registry::topo_order(&registry).map_err(ResolveError::Registry)?;
    for key in &order {
        if known.contains_key(key) {
            continue; // already hand-resolved above
        }
        let rk = &registry[key];
        let expr = rk.default_expr().expect("topo_order only names defaulted keys");
        let value = resolve_colon(key, env, &toml_map, || crate::eval::eval_expr(expr, &known))
            .map_err(|e| ResolveError::Registry(format!("{key}: {e}")))?;
        set!(key.as_str(), value);
    }
    // Every other registered key — a PROCEDURAL stub already covered above, or a genuine
    // NO-DEFAULT key — resolves to env/toml or the empty string.
    for key in registry.keys() {
        if known.contains_key(key) {
            continue;
        }
        let value =
            resolve_colon(key, env, &toml_map, ok_str!("")).map_err(ResolveError::Registry)?;
        set!(key.as_str(), value);
    }

    Ok(Resolved { values, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn resolve_or_say_returns_empty_on_a_missing_conf_d() {
        let ws = testkit::TempDir::new("spira-config-resolve-or-say");
        let home = ws.path().join("no-such-home");
        let env = BTreeMap::new();
        assert!(resolve_for_process(&home, &home, &env).is_err(), "positive control: this home must fail to resolve");
        assert_eq!(resolve_or_say("test", &home, &home, &env), Resolved::default());
    }

    fn fixture_home_repo(workspaces: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let repo = workspaces.join("spira-harness");
        std::fs::create_dir_all(repo.join("spira/conf.d")).unwrap();
        let home = repo.join("spira");
        (home, repo)
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn hand_written_defaults_derive_from_home_and_repo() {
        let ws = testkit::TempDir::new("spira-config-resolve-hand");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput {
            env: &e,
            home: &home,
            repo: &repo,
            toml: None,
            conf_d: &home.join("conf.d"),
        })
        .unwrap();
        assert_eq!(r.get("SPIRA_HOME_REPO"), "spira-harness");
        assert_eq!(r.get("SPIRA_INSTANCE"), "prod");
        assert_eq!(r.get("SPIRA_DB"), "/h/.local/share/spira/db");
        assert_eq!(r.get("SPIRA_RUN"), "/h/.local/share/spira/run");
        assert_eq!(r.get("SPIRA_WORKSPACES"), ws.display().to_string());
        assert_eq!(r.get("SPIRA_CERTIFY_SUITES"), "on");
        assert_eq!(r.get("SPIRA_AGENT"), "claude");
        assert_eq!(r.get("SPIRA_SCOPE_LABEL"), "spira-harness");
        assert_eq!(r.get("SPIRA_RELEASES"), format!("{}/spira-releases", ws.display()));
        assert_eq!(r.get("SPIRA_PROD"), format!("{}/spira-releases/current/spira", ws.display()));
    }

    #[test]
    fn gate_outcome_constants_resolve_fixed_and_ignore_env_and_toml() {
        let ws = testkit::TempDir::new("spira-config-resolve-gate-constants");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        // An attempt to override either constant through the environment — the one seam
        // every other key in this module honours — must be ignored outright: a
        // configurable NO_VERDICT could be set to 0, turning every withheld verdict into a
        // pass (this module's own doc).
        let e = env(&[("HOME", "/h"), ("SPIRA_GATE_NOVERDICT", "0"), ("SPIRA_GATE_BASEFAIL", "0")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_GATE_NOVERDICT"), "75");
        assert_eq!(r.get("SPIRA_GATE_BASEFAIL"), "76");
        assert!(EXPORT_KEYS.contains(&"SPIRA_GATE_NOVERDICT"));
        assert!(EXPORT_KEYS.contains(&"SPIRA_GATE_BASEFAIL"));
    }

    #[test]
    fn non_prod_instance_suffixes_db_and_run_but_not_dolt_data() {
        let ws = testkit::TempDir::new("spira-config-resolve-instance");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", "test")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_DB"), "/h/.local/share/spira-test/db");
        assert_eq!(r.get("SPIRA_RUN"), "/h/.local/share/spira-test/run");
        assert_eq!(r.get("SPIRA_DOLT_DATA"), "/h/.local/share/spira/dolt");
    }

    #[test]
    fn env_wins_outright_even_when_toml_also_sets_it() {
        let ws = testkit::TempDir::new("spira-config-resolve-env-wins");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let toml_text = "[spira]\ninstance = \"from-toml\"\n";
        let doc = crate::validate(toml_text).unwrap();
        let e = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", "from-env")]);
        let r = resolve(ResolveInput {
            env: &e,
            home: &home,
            repo: &repo,
            toml: Some(&doc),
            conf_d: &home.join("conf.d"),
        })
        .unwrap();
        assert_eq!(r.get("SPIRA_INSTANCE"), "from-env");
    }

    #[test]
    fn toml_wins_over_default_when_env_is_silent() {
        let ws = testkit::TempDir::new("spira-config-resolve-toml-wins");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let toml_text = "[spira]\ninstance = \"from-toml\"\n";
        let doc = crate::validate(toml_text).unwrap();
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput {
            env: &e,
            home: &home,
            repo: &repo,
            toml: Some(&doc),
            conf_d: &home.join("conf.d"),
        })
        .unwrap();
        assert_eq!(r.get("SPIRA_INSTANCE"), "from-toml");
    }

    #[test]
    fn preserve_empty_keys_keep_an_explicit_empty_env_value() {
        let ws = testkit::TempDir::new("spira-config-resolve-preserve-empty");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h"), ("SPIRA_DB", "")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_DB"), "");
    }

    #[test]
    fn home_repo_falls_back_to_manifest_then_to_the_directory_name() {
        let ws = testkit::TempDir::new("spira-config-resolve-manifest");
        let repo = ws.join("spira-releases").join("spira-20261001T000000Z");
        std::fs::create_dir_all(repo.join("spira/conf.d")).unwrap();
        std::fs::write(repo.join("MANIFEST"), "repo brain\nrelease-repo rgantt/spira\n").unwrap();
        let home = repo.join("spira");
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_HOME_REPO"), "brain");
        assert_eq!(r.get("SPIRA_RELEASE_REPO"), "rgantt/spira");
    }

    #[test]
    fn home_repo_is_empty_for_a_timestamped_release_dir_with_no_manifest() {
        let ws = testkit::TempDir::new("spira-config-resolve-timestamp");
        let repo = ws.join("spira-20261001T000000Z");
        std::fs::create_dir_all(repo.join("spira/conf.d")).unwrap();
        let home = repo.join("spira");
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_HOME_REPO"), "");
    }

    #[test]
    fn spira_claude_deprecation_warns_and_fills_agent() {
        let ws = testkit::TempDir::new("spira-config-resolve-claude");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h"), ("SPIRA_CLAUDE", "my-claude")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert_eq!(r.get("SPIRA_AGENT"), "my-claude");
        assert!(r.warnings.iter().any(|w| w.contains("SPIRA_CLAUDE is deprecated")), "{:?}", r.warnings);
    }

    #[test]
    fn home_and_repo_are_never_in_the_resolved_values() {
        let ws = testkit::TempDir::new("spira-config-resolve-no-home-repo");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        assert!(!r.values.contains_key("SPIRA_HOME"));
        assert!(!r.values.contains_key("SPIRA_REPO"));
    }

    #[test]
    fn registry_keys_resolve_generically_including_dependent_ones() {
        let ws = testkit::TempDir::new("spira-config-resolve-registry");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let conf_d = home.join("conf.d");
        std::fs::write(
            conf_d.join("SPIRA_QUEUE_BATCH_MAX"),
            "TYPE=u32\nGROUP=queue\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_QUEUE_BATCH_MAX:=8}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::fs::write(
            conf_d.join("SPIRA_QUEUE_THROTTLE_DEPTH_AT"),
            "TYPE=u32\nGROUP=queue\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_QUEUE_THROTTLE_DEPTH_AT:=$(( ${SPIRA_QUEUE_BATCH_MAX:-8} * 2 ))}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::fs::write(
            conf_d.join("SPIRA_COCKPIT"),
            "TYPE=string\nGROUP=cockpit\nDOC=x\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_COCKPIT:=$(dirname \"$SPIRA_HOME\")/cockpit}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &conf_d }).unwrap();
        assert_eq!(r.get("SPIRA_QUEUE_BATCH_MAX"), "8");
        assert_eq!(r.get("SPIRA_QUEUE_THROTTLE_DEPTH_AT"), "16");
        // SPIRA_COCKPIT := $(dirname "$SPIRA_HOME")/cockpit, and SPIRA_HOME is `repo/spira`.
        assert_eq!(r.get("SPIRA_COCKPIT"), format!("{}/cockpit", repo.display()));
    }

    #[test]
    fn a_containment_violation_refuses_the_whole_resolve() {
        let ws = testkit::TempDir::new("spira-config-resolve-containment");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        // Genuinely outside the workspaces root (a sibling of it), not merely a subdirectory
        // named "elsewhere" — ws itself IS the workspaces root in this fixture.
        let outside_root = testkit::TempDir::new("spira-config-resolve-containment-outside");
        let outside = outside_root.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        // The repo-map lives where SPIRA_REPO_MAP's own default would look: $SPIRA_HOME/repo-map.
        std::fs::write(home.join("repo-map"), format!("x|{}\n", outside.display())).unwrap();
        let e = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", "test")]);
        let err = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap_err();
        match err {
            ResolveError::Containment(v) => assert!(v[0].contains("is confined to"), "{v:?}"),
            other => panic!("expected Containment, got {other:?}"),
        }
    }

    #[test]
    fn to_sh_never_emits_a_per_copy_key() {
        let ws = testkit::TempDir::new("spira-config-resolve-to-sh");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env(&[("HOME", "/h")]);
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") })
            .unwrap();
        let sh = r.to_sh(EXPORT_KEYS);
        for forbidden in ["SPIRA_HOME=", "SPIRA_REPO=", "SPIRA_REPO_MAP=", "SPIRA_FAYTHS=", "SPIRA_MAX_AEONS="] {
            assert!(!sh.contains(forbidden), "to_sh leaked {forbidden}:\n{sh}");
        }
        assert!(sh.contains("SPIRA_RUN="), "{sh}");
        assert!(sh.contains("SPIRA_DB="), "{sh}");
    }

    #[test]
    fn derive_home_repo_env_override_wins() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-env");
        let (home, _repo) = fixture_home_repo(&ws);
        let e = env(&[("SPIRA_REPO", "/explicit/override")]);
        assert_eq!(derive_home_repo(&home, &e), Path::new("/explicit/override"));
    }

    // derive_repo_filesystem runs its git subprocess with exactly this map (`.env_clear()`
    // plus it) — PATH must be in it for the real `git` binary to be found at all.
    fn env_with_path(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = env(pairs);
        m.entry("PATH".to_string()).or_insert_with(|| std::env::var("PATH").unwrap_or_default());
        m
    }

    #[test]
    fn derive_home_repo_uses_git_toplevel() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-git");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let e = env_with_path(&[]);
        let got = derive_home_repo(&home, &e).canonicalize().unwrap();
        let want = repo.canonicalize().unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn derive_home_repo_falls_back_to_parent_outside_a_checkout() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-noparent");
        let (home, repo) = fixture_home_repo(&ws);
        // deliberately NOT a git repo: no `git init`
        let e = env_with_path(&[]);
        let got = derive_home_repo(&home, &e).canonicalize().unwrap();
        let want = repo.canonicalize().unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn derive_home_repo_scrubs_the_git_hook_environment() {
        // conf.sh's own scar: a git hook runs with GIT_DIR exported, and --show-toplevel
        // under that answers the -C directory itself (<repo>/spira) instead of climbing to
        // <repo> — one level too deep — unless GIT_DIR (and friends) are scrubbed first.
        //
        // NEVER std::env::set_var here: derive_repo_filesystem runs its git subprocess
        // with `.env_clear().envs(env)` — the EXPLICIT map this test hands it, never this
        // process's own ambient environment — specifically so a "GIT_DIR is poisoned" case
        // like this one is testable without mutating real process-global state that a
        // PARALLEL test's own unrelated `git` spawn would otherwise inherit regardless of
        // any Rust-level lock (the race an earlier, `std::env::set_var`-based version of
        // this test actually caused: `cargo test -p spira-config` at its default 32-wide
        // thread pool flaked across unrelated repos.rs tests that never touch GIT_DIR
        // themselves — only `--test-threads=1` hid it, by accident).
        let ws = testkit::TempDir::new("spira-config-derive-repo-hook");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let fake_git_dir = home.join(".git-hook-fake");
        std::fs::create_dir_all(&fake_git_dir).unwrap();
        let e = env_with_path(&[
            ("GIT_DIR", fake_git_dir.to_str().unwrap()),
            ("GIT_WORK_TREE", home.to_str().unwrap()),
        ]);
        let got = derive_home_repo(&home, &e).canonicalize().unwrap();
        let want = repo.canonicalize().unwrap();
        assert_eq!(got, want, "a scrubbed GIT_DIR must still climb to the real toplevel");
    }

    #[test]
    fn resolve_for_process_matches_resolve_given_the_same_inputs() {
        // discover(None), unlike resolve() itself, reads THIS PROCESS's real environment
        // (SPIRA_TOML/XDG_CONFIG_HOME/HOME/$SPIRA_CONF), never the `env` map passed to
        // resolve() — exactly the box this machine's own operator spira.toml under
        // ~/.config/spira would otherwise fall into. Clear and repoint all four so this
        // test gets the same "nothing resolves" answer on every machine, not only one
        // with no real config, the hazard spira-config's own locate.rs tests guard the
        // same way.
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> =
            names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-for-process");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let e = env(&[("HOME", "/h")]);
        let direct = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: None, conf_d: &home.join("conf.d") }).unwrap();
        let via_wrapper = resolve_for_process(&home, &repo, &e).unwrap();

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert_eq!(direct, via_wrapper);
    }

    /// sp-ivfu3: `resolve_run_dir`/`resolve_instance` are the one place a Rust caller gets
    /// `spira.run`/`spira.instance` instead of a bare `env SPIRA_RUN || "/tmp/spira"` (or
    /// `SPIRA_INSTANCE` unset-means-unqualified) literal. The derived default (no explicit
    /// env or toml value) still produces a real path/instance, not a refusal.
    #[test]
    fn resolve_run_dir_and_instance_use_the_derived_default_with_no_literal_fallback() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let e = env(&[("HOME", "/h")]);
        let run = resolve_run_dir(&e, &home).unwrap();
        let instance = resolve_instance(&e, &home).unwrap();

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert_eq!(run, PathBuf::from("/h/.local/share/spira/run"), "never the /tmp/spira literal");
        assert_eq!(instance, "prod");
    }

    /// An env override still wins outright, exactly as `SPIRA_RUN` always has — this is
    /// what makes a systemd unit, which sets it explicitly, keep working unchanged.
    #[test]
    fn resolve_ask_label_env_override_wins_and_empty_env_falls_to_config() {
        let mut env = BTreeMap::new();
        env.insert("SPIRA_ASK_LABEL".to_string(), "ask-x".to_string());
        assert_eq!(resolve_ask_label(&env, Path::new("/nonexistent")).unwrap(), "ask-x");
        env.insert("SPIRA_ASK_LABEL".to_string(), String::new());
        let got = resolve_ask_label(&env, Path::new("/nonexistent"));
        assert!(got.map(|v| !v.is_empty()).unwrap_or(true));
    }

    #[test]
    fn resolve_run_dir_env_override_wins() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-override");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let override_run = ws.join("run-override");
        let e = env(&[("HOME", "/h"), ("SPIRA_RUN", override_run.to_str().unwrap()), ("SPIRA_INSTANCE", "test")]);
        let run = resolve_run_dir(&e, &home).unwrap();
        let instance = resolve_instance(&e, &home).unwrap();

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert_eq!(run, override_run);
        assert_eq!(instance, "test");
    }

    /// A `spira.toml` that fails to parse is the named refusal `resolve_run_dir` returns —
    /// never a literal path a caller could mistake for a real answer.
    #[test]
    fn resolve_run_dir_refuses_named_on_a_malformed_toml() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-bad-toml");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        let toml_path = ws.join("spira.toml");
        std::fs::write(&toml_path, "this is not [valid toml").unwrap();
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());
        std::env::set_var("SPIRA_TOML", toml_path.to_str().unwrap());

        let e = env(&[("HOME", "/h")]);
        let got = resolve_run_dir(&e, &home);

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        let err = got.expect_err("a malformed spira.toml must refuse, not guess /tmp/spira");
        assert!(err.contains("cannot resolve spira.run"), "{err}");
    }

    /// sp-ivfu3-2: an explicit `SPIRA_RUN` must win OUTRIGHT, before `resolve_for_process`
    /// ever runs — `spira_containment_check` is `resolve`'s own unconditional last step,
    /// judging every OTHER registered checkout against the confined workspace, and has
    /// nothing to do with this key. `mail`'s own test fixture pins `SPIRA_RUN` but runs
    /// inside a containment-confined instance whose ambient repo-map names a checkout
    /// outside it; before this fix, `resolve_run_dir` called full resolution regardless
    /// and that UNRELATED violation refused the whole thing — "mail: FATAL: cannot
    /// resolve spira.run" even though `SPIRA_RUN` itself was never in doubt.
    #[test]
    fn resolve_run_dir_env_override_bypasses_an_unrelated_containment_violation() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-containment");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        // A genuine containment violation: a repo-map entry outside the workspaces root —
        // the exact fixture `a_containment_violation_refuses_the_whole_resolve` (above)
        // already proves fails resolve() on its own.
        let outside_root = testkit::TempDir::new("spira-config-resolve-run-dir-containment-outside");
        let outside = outside_root.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(home.join("repo-map"), format!("x|{}\n", outside.display())).unwrap();
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let explicit = ws.join("explicit-run");
        let with_override = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", "test"), ("SPIRA_RUN", explicit.to_str().unwrap())]);
        let got = resolve_run_dir(&with_override, &home);
        let without_override = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", "test")]);
        let err = resolve_run_dir(&without_override, &home);

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert_eq!(got.unwrap(), explicit, "the override must bypass containment entirely");
        // Positive control: without the override, the SAME fixture still refuses — the
        // violation is real, not vacuously absent (law-absence-needs-a-positive-control).
        assert!(err.is_err(), "the fixture's own containment violation must still refuse with no override");
    }

    fn run_dir_under_instance(instance: &str, run: impl Fn(&Path) -> String) -> (Result<PathBuf, String>, String) {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-confined");
        let (home, repo) = fixture_home_repo(&ws);
        run_git(&repo, &["init", "-q"]);
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());
        let value = run(ws.path());
        let e = env(&[("HOME", "/h"), ("SPIRA_INSTANCE", instance), ("SPIRA_RUN", &value)]);
        let got = resolve_run_dir(&e, &home);
        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        (got, value)
    }

    #[test]
    fn a_confined_instance_refuses_an_explicit_run_outside_its_confinement() {
        let (got, value) = run_dir_under_instance("test", |_| "/outside/spira/run".to_string());
        let err = got.expect_err("an explicit SPIRA_RUN outside the confinement must refuse");
        assert!(err.contains("is confined to") && err.contains("SPIRA_RUN") && err.contains(&value), "{err}");
    }

    #[test]
    fn a_confined_instance_resolves_an_explicit_run_inside_its_confinement() {
        let (got, value) = run_dir_under_instance("test", |ws| ws.join("run").display().to_string());
        assert_eq!(got.unwrap(), PathBuf::from(value));
    }

    #[test]
    fn an_unconfined_instance_resolves_an_explicit_run_anywhere() {
        let (got, value) = run_dir_under_instance("prod", |_| "/outside/spira/run".to_string());
        assert_eq!(got.unwrap(), PathBuf::from(value));
    }

    /// sp-bp249 follow-up: `ctrl`'s own best-effort `home` (an ancestor-walk off its own
    /// exe) comes back empty whenever `bin/ctrl` is really a symlink into a build-artifacts
    /// tree with no `spira/` nearby — exactly what testenv's staged release layout does.
    /// `SPIRA_HOME` is unset too (every unit, and testenv's own `suspend_request`, exports
    /// only `SPIRA_RELEASE` and `PATH`), so the containment check this function now has to
    /// run must still find a real registry via `$SPIRA_RELEASE/spira`, never by handing
    /// `registry::load` the caller's empty `home` and letting it build a bare relative
    /// `"conf.d"`.
    #[test]
    fn resolve_run_dir_with_explicit_run_falls_back_to_spira_release_when_spira_home_is_unset() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-release-home");
        let (_release_home, release_repo) = fixture_home_repo(&ws.join("release"));
        run_git(&release_repo, &["init", "-q"]);
        std::env::set_var("HOME", "/h");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let run_target = ws.join("run").display().to_string();
        let e = env(&[("HOME", "/h"), ("SPIRA_RELEASE", release_repo.to_str().unwrap()), ("SPIRA_RUN", &run_target)]);
        // The caller's own `home` is empty — its ancestor-walk already failed, exactly as
        // `ctrl::spira_home()`'s `unwrap_or_default()` leaves it.
        let got = resolve_run_dir(&e, Path::new(""));

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert_eq!(
            got.unwrap(),
            PathBuf::from(&run_target),
            "must resolve via $SPIRA_RELEASE/spira/conf.d, not refuse on the caller's empty home"
        );
    }

    /// The other half: when NOTHING can supply a registry — no `SPIRA_HOME`, no
    /// `SPIRA_RELEASE`, and the caller's own `home` is empty — this must refuse with a
    /// message naming what was missing, never silently hand `registry::load` a relative
    /// `"conf.d"` the way the un-fixed code did.
    #[test]
    fn resolve_run_dir_with_explicit_run_and_no_resolvable_home_names_what_was_missing() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = ["SPIRA_TOML", "SPIRA_CONF", "SPIRA_REPO", "XDG_CONFIG_HOME", "HOME"];
        let saved: Vec<(&str, Option<String>)> = names.iter().map(|n| (*n, std::env::var(n).ok())).collect();
        for n in names {
            std::env::remove_var(n);
        }
        std::env::set_var("HOME", "/h");
        let ws = testkit::TempDir::new("spira-config-resolve-run-dir-no-home");
        std::env::set_var("XDG_CONFIG_HOME", ws.join("no-such-xdg").to_str().unwrap());

        let e = env(&[("HOME", "/h"), ("SPIRA_RUN", "/tmp/some-run")]);
        let err = resolve_run_dir(&e, Path::new("")).expect_err("no home anywhere must refuse, not guess");

        for (n, v) in saved {
            match v {
                Some(v) => std::env::set_var(n, v),
                None => std::env::remove_var(n),
            }
        }
        assert!(err.contains("SPIRA_HOME") && err.contains("SPIRA_RELEASE"), "{err}");
        assert!(
            !err.contains("at conf.d") && err != "no config registry at conf.d",
            "must never surface the bare relative path: {err}"
        );
    }
}
