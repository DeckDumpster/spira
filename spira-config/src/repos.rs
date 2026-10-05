//! The repo registry — `spira/lib.sh`'s `repo_field`/`repo_root`/`repo_land`/`spira_repos`/
//! `spira_home_repo` family, ported (wave4-decomposition.md row U, bead sp-37rmg, "wave
//! 4.11"). The most-called family in the whole decomposition (16 Rust seams, 20 bash
//! scripts) — moving it in-process is the biggest single wall-clock win the plan names.
//!
//! WHAT THIS READS: the legacy pipe-delimited `SPIRA_REPO_MAP` file, not yet `spira.toml`'s
//! typed `[repo.*]` table. `conf.sh` still derives `SPIRA_REPO_MAP` itself (the "conf.sh
//! becomes an eval of resolve" cutover, wave4-decomposition.md row 5, has not landed), so
//! the pipe file remains the one live source of truth for which repositories this harness
//! manages — porting the READER faithfully, not changing what it reads, is the whole job of
//! this bead. `queue`'s own `[repo.<name>].mode`/`.base` read (`real.rs`) is a different,
//! narrower thing: the land-mode transition's own target of record, not a drop-in substitute
//! for this map.
//!
//! UNKNOWN NAMES FAIL CLOSED, exactly as lib.sh's own comment insists: every lookup here
//! returns `None`/empty for a name the map does not carry, and a caller that defaults a
//! repository to the home checkout is precisely how another repository's bead gets "fixed"
//! in the home repo while reporting success.
//!
//! COLUMNS ARE NAMED, NEVER NUMBERED, for the same reason lib.sh's own `repo_field` insists
//! on it: the row shape decides where the optional columns live (four fields predates both
//! `format` and `base`; five predates `base`; six is the current shape; a seventh is an
//! explicit lanes column), and reading a fixed position is how a format change fails
//! silently in both directions at once — see [`parse_row`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One `repo-map` column `repo_field` can be asked for. `Lanes` is still read HERE — lane
/// *expansion* is family V (`spira_repo_lanes`/`_spira_expand_lanes`), owned by
/// `maechen-trigger`, not this registry — but `spira_repo_lanes` itself still calls
/// `repo_field "$name" lanes` to get the raw column, and that call is now a shim onto this
/// enum: dropping the variant would refuse every repo-map row that declares lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Path,
    Land,
    Base,
    Format,
    Gate,
    Lanes,
}

/// One valid row of the map — a comment or a row naming no repository (`NF < 2` in lib.sh's
/// own awk) never becomes one. Every field is the empty string when the row's shape does not
/// carry that column, matching `repo_field`'s own return convention: "absent" and "present
/// but blank" are the same answer to every caller of this map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    pub land: String,
    pub base: String,
    pub format: String,
    pub gate: String,
    /// The lanes column, raw and unexpanded (family V's business).
    pub lanes: String,
}

impl Row {
    pub fn field(&self, col: Column) -> &str {
        match col {
            Column::Path => &self.path,
            Column::Land => &self.land,
            Column::Base => &self.base,
            Column::Format => &self.format,
            Column::Gate => &self.gate,
            Column::Lanes => &self.lanes,
        }
    }
}

/// A lanes-column value looks like a bare identifier list (letters, digits, commas,
/// underscores, hyphens) — a gate fragment always contains spaces, slashes, dollars or other
/// shell characters, so this is how `repo_field`'s awk tells a trailing lanes column from the
/// tail of a gate command apart, in practice. An empty trailing field also counts: it is an
/// explicit (if blank) lanes column, not a shorter gate.
fn looks_like_lanes(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '_' | '-'))
}

/// `repo_field`'s own awk program, one row at a time. `line` is one line of the map file,
/// NOT yet filtered for comments/blanks — this returns `None` for both, exactly as the awk
/// skips them (`/^[ \t]*#/ { next }`, then `if (n == "" || NF < 2) next`).
fn parse_row(line: &str) -> Option<Row> {
    if line.trim_start().starts_with('#') {
        return None;
    }
    let fields: Vec<&str> = line.split('|').collect();
    let nf = fields.len();
    let name = fields[0].trim();
    if name.is_empty() || nf < 2 {
        return None;
    }
    let path = fields[1].trim().to_string();
    let land = if nf >= 3 { fields[2].trim().to_string() } else { String::new() };

    // `_lanes_col_idx`: only a row with a genuine 7th column can carry lanes at all.
    let last = fields[nf - 1].trim();
    let has_lanes = nf >= 7 && looks_like_lanes(last);
    let lanes = if has_lanes { last.to_string() } else { String::new() };

    let base = if nf >= 6 { fields[3].trim().to_string() } else { String::new() };
    let format = if nf >= 6 {
        fields[4].trim().to_string()
    } else if nf == 5 {
        fields[3].trim().to_string()
    } else {
        String::new()
    };

    // The gate is every fixed column from `s` to `e` (1-based, inclusive), rejoined with
    // "|" — it is the one column that may itself contain pipes. `s` depends on how many
    // fixed columns precede it; `e` stops one short of the lanes column when one is present.
    let s1 = if nf >= 6 { 6 } else if nf == 5 { 5 } else { 4 };
    let e1 = if has_lanes { nf - 1 } else { nf };
    let gate = if s1 >= 1 && s1 <= e1 && e1 <= nf {
        fields[(s1 - 1)..e1].join("|").trim().to_string()
    } else {
        String::new()
    };

    Some(Row { name: name.to_string(), path, land, base, format, gate, lanes })
}

/// `repo_field`/`repo_names`' shared row filter, over a whole map file. Order is preserved
/// (first-match-wins lookups, like awk's own `print; exit`, read the first element whose
/// name matches).
pub fn parse(text: &str) -> Vec<Row> {
    text.lines().filter_map(parse_row).collect()
}

/// The `[repo.<name>]` tables as map rows, for a box whose `spira.toml` is the only source:
/// no `repo-map` file is read when this applies, and a map file that exists always wins.
/// `gate` is retired from the schema, so that column is empty.
pub fn rows_from_toml(doc: &crate::SpiraToml) -> Vec<Row> {
    use crate::{Lane, LandMode};
    doc.repo
        .iter()
        .map(|(name, r)| Row {
            name: name.clone(),
            path: r.path.clone(),
            land: match r.mode {
                LandMode::Push => "push",
                LandMode::Pr => "pr",
                LandMode::Hold => "hold",
                LandMode::Queue => "queue",
                LandMode::QueueForge => "queue.forge",
                LandMode::QueueLocal => "queue.local",
            }
            .to_string(),
            base: r.base.clone().unwrap_or_default(),
            format: r.format.clone().unwrap_or_default(),
            gate: String::new(),
            lanes: r
                .lanes
                .iter()
                .map(|l| match l {
                    Lane::Plan => "plan",
                    Lane::Incident => "incident",
                    Lane::Groom => "groom",
                    // literal-ok: the repo-map lane token, emitted verbatim
                    Lane::MaechenSweep => "maechen-sweep",
                    Lane::Spike => "spike",
                    Lane::CzarTrigger => "czar-trigger",
                })
                .collect::<Vec<_>>()
                .join(","),
        })
        .collect()
}

/// `repo_field <name> <col>` — the first row named `name`'s column, or `""` when no row
/// matches (matching bash: not-found and found-but-blank are the same empty answer).
pub fn field(rows: &[Row], name: &str, col: Column) -> String {
    rows.iter().find(|r| r.name == name).map(|r| r.field(col).to_string()).unwrap_or_default()
}

/// `repo_names` — every valid row's name, in file order (duplicates included, exactly as
/// lib.sh's own `awk … print n` would emit one line per matching row).
pub fn names(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|r| r.name.clone()).collect()
}

/// `basename` of `p`, with any trailing slash stripped first — `${p%/}` then `basename`, the
/// same two-step lib.sh/conf.sh take rather than trusting a single call on a path that might
/// end in `/`.
fn basename(p: &str) -> String {
    Path::new(p.trim_end_matches('/')).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `spira_home_repo` — the repo name a bead means when it names none. `env` is read
/// directly, matching the bash original's own direct reads of `$SPIRA_HOME_REPO`/
/// `$SPIRA_REPO`/`$SPIRA_HOME`: conf.sh (once cut over) exports `SPIRA_HOME_REPO` as its own
/// derived default, so the `basename` fallback below only fires for a process that never ran
/// conf.sh's resolution at all (a raw `lib.sh` source, or today's equivalent: this port asked
/// for neither of the per-copy facts and only reads them out of `env`, as
/// [`crate::resolve::resolve`] already does for `SPIRA_HOME`/`SPIRA_REPO`).
pub fn home_repo(env: &BTreeMap<String, String>, home: &Path) -> String {
    if let Some(v) = env.get("SPIRA_HOME_REPO") {
        if !v.is_empty() {
            return v.clone();
        }
    }
    let base = match env.get("SPIRA_REPO") {
        Some(r) if !r.is_empty() => r.as_str(),
        _ => home.to_str().unwrap_or(""),
    };
    basename(base)
}

/// Whether `$SPIRA_REPO` counts as a deliberate override of the home repo's path rather than
/// conf.sh's own derived value — `repo_root`/`repo_name_at`'s own test, lifted out so both
/// share it: `$SPIRA_REPO` is set AND it differs from `$SPIRA_REPO_DERIVED` (conf.sh's cached
/// `git rev-parse --show-toplevel` of `$SPIRA_HOME`, or its dirname fallback). An unset
/// `SPIRA_REPO_DERIVED` reads as `""`, same as bash's `${SPIRA_REPO_DERIVED:-}`.
fn repo_override(env: &BTreeMap<String, String>) -> Option<String> {
    let r = env.get("SPIRA_REPO").filter(|v| !v.is_empty())?;
    let derived = env.get("SPIRA_REPO_DERIVED").map(String::as_str).unwrap_or("");
    (r.as_str() != derived).then(|| r.clone())
}

/// The repo-map, resolved once per process — the in-process replacement for repeatedly
/// shelling to `repo_field`/`repo_root`/... (wave4-decomposition.md's own cost note: this
/// family is the single largest wall-clock win from moving in-process, since a pass can make
/// 20 or more seam calls into it). Built from the SAME inputs `resolve::resolve` takes
/// (`env`, `home`) plus the map file's own text, so a caller that already has a
/// `ResolveInput` in hand supplies the same `env`/`home` here.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    rows: Vec<Row>,
    home: String,
    root_override: Option<String>,
    map_present: bool,
}

impl Registry {
    /// TEST-ONLY. `env` here is read VERBATIM — no `registry_env` backfill, no resolve.
    /// Production code must never call this: conf.sh resolves `SPIRA_REPO_MAP`/
    /// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED` but exports NONE of them, so a
    /// process started by a unit (env = `SPIRA_RELEASE` + `PATH` only) or a bare shell
    /// sees none of them either, and a `Registry` built straight from that bare env has no
    /// map and no landref — every `land-local` refused in production (sp-z3eyk) before this
    /// was caught. [`Registry::from_env`] is the one production door onto a `Registry`; this
    /// stays `pub` only because tests across several crates build deterministic registries
    /// from explicit, already-complete env maps (no resolve wanted, no filesystem touched).
    /// `map_text` is `None` when `SPIRA_REPO_MAP` does not exist — "no map" is not a parse
    /// error (lib.sh: `[ -f "$SPIRA_REPO_MAP" ] || return 0/1`, depending on the caller; see
    /// [`Registry::map_present`]).
    #[doc(hidden)]
    pub fn new(map_text: Option<&str>, env: &BTreeMap<String, String>, home: &Path) -> Registry {
        Registry {
            rows: map_text.map(parse).unwrap_or_default(),
            home: home_repo(env, home),
            root_override: repo_override(env),
            map_present: map_text.is_some(),
        }
    }

    /// THE production constructor (sp-k6lku, following through on sp-z3eyk's
    /// `registry_env`): fills `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED` in-process via [`registry_env`] (itself built on
    /// [`crate::resolve::resolve_for_process`]) when `env` lacks them — which it always
    /// does for a bare process env, since conf.sh exports none of the four — then reads
    /// the map file and builds the `Registry` exactly as [`Registry::new`] would. A caller
    /// holding a genuine conf.sh-resolved snapshot (a bash subprocess that sourced
    /// lib.sh/conf.sh and read its own variables back, or another `resolve_for_process`
    /// answer already merged in) can pass that snapshot here too: `registry_env` leaves
    /// every key already present untouched, so this never re-resolves work already done.
    /// ONE way to get a registry — nothing outside this module calls `new` in production.
    pub fn from_env(env: BTreeMap<String, String>, home: &Path) -> Registry {
        let env = registry_env(env, home);
        let map_text = env.get("SPIRA_REPO_MAP").filter(|p| !p.is_empty()).and_then(|p| std::fs::read_to_string(p).ok());
        if map_text.is_none() {
            if let Some(rows) = crate::locate::locate(None).found().and_then(|p| crate::load(&p).ok()).map(|d| rows_from_toml(&d)) {
                return Registry { rows, home: home_repo(&env, home), root_override: repo_override(&env), map_present: true };
            }
        }
        Registry::new(map_text.as_deref(), &env, home)
    }

    /// Whether `SPIRA_REPO_MAP` existed — `repo_field`-based lookups ([`Registry::field`],
    /// [`Registry::root`], [`Registry::gate`], [`Registry::format`], [`Registry::base`])
    /// refuse (bash: `return 1`) when it did not, even though a missing map and an empty map
    /// parse identically into no [`Row`]s. [`Registry::names`]/[`Registry::all`] do not
    /// consult this — `repo_names`/`spira_repos` succeed either way.
    pub fn map_present(&self) -> bool {
        self.map_present
    }

    /// `spira_home_repo`.
    pub fn home_repo(&self) -> &str {
        &self.home
    }

    /// `repo_field <name> <col>`. `None` when the map file itself was absent — distinct from
    /// "present but the name/column is blank", which is `Some("")`.
    pub fn field(&self, name: &str, col: Column) -> Option<String> {
        self.map_present.then(|| field(&self.rows, name, col))
    }

    /// `repo_names`.
    pub fn names(&self) -> Vec<String> {
        names(&self.rows)
    }

    /// `spira_repos` — the home repo first, always, then every other mapped name
    /// (`repo_names | grep -vx -- "$home"`: an exact-line exclusion, so a mapped row that
    /// happens to share the home repo's name is dropped rather than duplicated).
    pub fn all(&self) -> Vec<String> {
        let mut out = vec![self.home.clone()];
        out.extend(self.names().into_iter().filter(|n| n != &self.home));
        out
    }

    /// `repo_root <name>` — `name` defaults to the home repo when empty. `$SPIRA_REPO`
    /// answers directly, without consulting the map at all (or caring whether it is even
    /// present), exactly when it is a deliberate override of the home repo's path AND the
    /// resolved name is the home repo; every other name is a plain map lookup, and an unmapped
    /// name (or a missing map) is `None` — never a guessed default (lib.sh's own comment:
    /// "a default of the home checkout is precisely how another repository's bead gets
    /// 'fixed' in the home repo").
    pub fn root(&self, name: &str) -> Option<String> {
        let name = if name.is_empty() { self.home.clone() } else { name.to_string() };
        if name == self.home {
            if let Some(ov) = &self.root_override {
                return Some(ov.clone());
            }
        }
        self.field(&name, Column::Path).filter(|p| !p.is_empty())
    }

    /// `repo_land <name>` — `queue.forge` normalizes to `queue`; an unmapped name, a blank
    /// column, or an absent map all read as the same default, `push`. Unlike [`Registry::root`]
    /// this never refuses: lib.sh's own `repo_land` ignores `repo_field`'s exit status outright
    /// (the `$(...)` capture is unchecked), so a missing map behaves exactly like an unmapped
    /// name rather than an error.
    pub fn land(&self, name: &str) -> String {
        let m = self.map_present.then(|| field(&self.rows, name, Column::Land)).unwrap_or_default();
        match m.as_str() {
            "" => "push".to_string(),
            "queue.forge" => "queue".to_string(),
            other => other.to_string(),
        }
    }

    /// `repo_land_queued <name>` — a boolean reading of [`Registry::land`], not a refusal:
    /// `queue` and `queue.local` are both "yes", everything else (including unmapped) is "no".
    pub fn land_queued(&self, name: &str) -> bool {
        matches!(self.land(name).as_str(), "queue" | "queue.local")
    }

    /// `repo_gate <name>`.
    pub fn gate(&self, name: &str) -> Option<String> {
        self.field(name, Column::Gate)
    }

    /// `repo_format <name>`. Absence means do nothing — lib.sh's own comment: running a
    /// formatter a repository never asked for turns one bead's rebase into a diff nobody
    /// requested.
    pub fn format(&self, name: &str) -> Option<String> {
        self.field(name, Column::Format)
    }

    /// `repo_base <name>` — the raw declared base column. A caller wanting the ref to
    /// actually land on wants `spira_landref` (family W, a later bead), not this.
    pub fn base(&self, name: &str) -> Option<String> {
        self.field(name, Column::Base)
    }

    /// `repo_name_at <path>` — the reverse of [`Registry::root`]: the map name for a checkout
    /// path, or `None`. `path` is compared literally against the override and against each
    /// row's own `path` column — never canonicalized — matching lib.sh exactly: callers that
    /// need canonical identity want [`same_repo`] instead.
    pub fn name_at(&self, path: &str) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        if let Some(ov) = &self.root_override {
            if path == ov {
                return Some(self.home.clone());
            }
        }
        self.names().into_iter().find(|n| field(&self.rows, n, Column::Path) == path)
    }
}

// ========================================================================================
// BASE REFS (wave4-decomposition.md row W, sp-o88bx "wave 4.12"): `spira_landref`,
// `ref_remote`, `ref_branch`, `qualify_base_ref`, `spira_landrefs`, `spira_publish_forge`.
// Ported from lib.sh's own essay (see its "THE BASE" comment, preserved there) — the
// rationale is reproduced only in brief here:
//
// Every branch this harness creates or lands is measured against the REMOTE-TRACKING ref of
// its repository's default branch, resolved per repository in this order, refusing rather
// than guessing when none of them answers: (1) the repo-map's declared `base` column —
// verified to exist, since shipping an unverified guess only moves the guess into the map;
// (2) `refs/remotes/origin/HEAD`, the remote's own default as already cached locally; (3)
// asking the remote once (`git remote set-head --auto`, which also WRITES the ref rung 2
// reads, so this is paid at most once per repository, not once per call) — reached only when
// there is exactly one remote, or a remote literally named `origin` among several, and
// refused outright when there is some other number of candidates; (4) a repository with NO
// remote at all falls back to its own current branch, since nothing can be stale against a
// remote that does not exist — this is the ONLY rung that ever reads a checkout's HEAD, and
// it is unreached whenever any remote exists at all, however rung 3 turns out.
//
// `main` is never assumed: three of a real seven-row repo-map had no `main` at all (their
// default was `master`), so guessing breaks worktree creation, CHECK 6's rebase and
// `gh pr create --base` all at once.
// ========================================================================================

fn git_status_ok(repo: &str, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(repo).args(args).output().map(|o| o.status.success()).unwrap_or(false)
}

fn git_stdout(repo: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `git -C <repo> remote` — every configured remote, one per line. A command failure reads
/// as "no remotes," exactly as bash's own `$(git remote 2>/dev/null)` does (a failed
/// substitution is just an empty string, and nothing downstream distinguishes the two).
fn git_remotes(repo: &str) -> Vec<String> {
    match Command::new("git").arg("-C").arg(repo).arg("remote").output() {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
        }
        _ => Vec::new(),
    }
}

/// `git rev-parse --verify -q <r>` — whether `r` resolves to an object in `repo`, discarding
/// git's own error text exactly as bash's `>/dev/null 2>&1` does.
fn verify_ref(repo: &str, r: &str) -> bool {
    git_status_ok(repo, &["rev-parse", "--verify", "-q", r])
}

fn symbolic_ref_short(repo: &str, r: &str) -> Option<String> {
    git_stdout(repo, &["symbolic-ref", "-q", "--short", r])
}

/// `spira_landref [repo-path-or-name]` -> the base ref, or `None` if unresolvable. `arg`
/// containing a `/` is a repository PATH (lib.sh's own `*/*` case pattern); otherwise it is a
/// NAME (empty meaning the home repo), resolved through `reg.root`.
pub fn landref(reg: &Registry, arg: &str) -> Option<String> {
    let mut name = String::new();
    let repo = if arg.contains('/') {
        arg.to_string()
    } else {
        name = if arg.is_empty() { reg.home_repo().to_string() } else { arg.to_string() };
        reg.root(&name)?
    };
    if name.is_empty() {
        name = reg.name_at(&repo).unwrap_or_default();
    }
    // `[ -e "$repo/.git" ]` — exists, not is-dir: a worktree's `.git` is a file.
    if !Path::new(&repo).join(".git").exists() {
        return None;
    }

    // 1 — declared. A row with no `base` column, an unmapped name, or an absent map all read
    // as "" here, same as repo_field's own return convention.
    if !name.is_empty() {
        let r = reg.base(&name).unwrap_or_default();
        if !r.is_empty() {
            return verify_ref(&repo, &r).then_some(r);
        }
    }

    // 2 — the remote's own declared default, as cached locally.
    if let Some(r) = symbolic_ref_short(&repo, "refs/remotes/origin/HEAD") {
        if verify_ref(&repo, &r) {
            return Some(r);
        }
    }

    // 3 — ask the remote once: `origin` if present, the lone remote if there is only one,
    // refused on any other count. Reaching this rung with any remote at all means rung 4 is
    // never consulted, win or lose — a checkout's HEAD is never trusted while a remote
    // exists.
    let remotes = git_remotes(&repo);
    if !remotes.is_empty() {
        let remote = if remotes.iter().any(|r| r == "origin") {
            Some("origin".to_string())
        } else if remotes.len() == 1 {
            Some(remotes[0].clone())
        } else {
            None
        };
        if let Some(remote) = remote {
            if git_status_ok(&repo, &["remote", "set-head", &remote, "--auto"]) {
                if let Some(r) = symbolic_ref_short(&repo, &format!("refs/remotes/{remote}/HEAD")) {
                    if verify_ref(&repo, &r) {
                        return Some(r);
                    }
                }
            }
        }
        return None;
    }

    // 4 — no remote at all: the repository's own current branch, the only rung that reads
    // a checkout's HEAD at all.
    if let Some(r) = symbolic_ref_short(&repo, "HEAD") {
        if verify_ref(&repo, &r) {
            return Some(r);
        }
    }
    None
}

/// `ref_remote <ref> [repo]` -> the remote name before the first `/`, or `None` when `ref`
/// has no `/` at all, or (when `repo` is given) that prefix does not actually name one of
/// `repo`'s remotes — a `queue.local` row's ref like `local/main` is a local branch that
/// happens to contain a slash, not a remote-tracking one, and `repo` is what tells the two
/// apart. Without a repo (a caller that predates that distinction), the split is unconditional.
pub fn ref_remote(ref_: &str, repo: Option<&str>) -> Option<String> {
    let prefix = ref_.split_once('/')?.0.to_string();
    if let Some(repo) = repo {
        if !git_remotes(repo).iter().any(|r| r == &prefix) {
            return None;
        }
    }
    Some(prefix)
}

/// `ref_branch <ref>` -> everything after the first `/`, or `ref` unchanged when it has none.
pub fn ref_branch(ref_: &str) -> String {
    match ref_.split_once('/') {
        Some((_, rest)) => rest.to_string(),
        None => ref_.to_string(),
    }
}

/// `qualify_base_ref <ref> <repo>` -> the fully-qualified `refs/remotes/<remote>/<branch>`
/// when `ref` names one of `repo`'s remotes and that ref actually exists there, else `ref`
/// unchanged — a bare `origin/main` is ambiguous when `refs/heads/origin/main` also exists,
/// so a caller that has resolved a remote wants the qualified form to be unambiguous.
pub fn qualify_base_ref(ref_: &str, repo: &str) -> String {
    let Some(remote) = ref_remote(ref_, Some(repo)) else {
        return ref_.to_string();
    };
    let branch = ref_branch(ref_);
    let fq = format!("refs/remotes/{remote}/{branch}");
    if verify_ref(repo, &fq) {
        fq
    } else {
        ref_.to_string()
    }
}

/// `spira_landrefs <repo>` -> the land ref, plus its local counterpart when one exists. The
/// commit graph is read across BOTH because a commit can be on the local branch and not yet
/// pushed, or pushed and never pulled into this checkout.
pub fn landrefs(reg: &Registry, repo: &str) -> Option<(String, Option<String>)> {
    let base = landref(reg, repo)?;
    let lo = ref_branch(&base);
    let local = if lo != base && verify_ref(repo, &lo) { Some(lo) } else { None };
    Some((base, local))
}

/// `spira_publish_forge <name>` -> `(remote, branch)`, the forge target a `queue.local`
/// repository's publish queue fast-forwards on a green publish PR. Never `ref_remote` of the
/// land ref — under `queue.local` that ref is a bare local branch by design, so the forge
/// target must be named instead: the remote defaults to `origin`
/// (`SPIRA_PUBLISH_REMOTE`/per-repo `SPIRA_PUBLISH_REMOTE_<NAME>` override it); the branch
/// defaults to the land ref's own name with a leading `local/` stripped
/// (`SPIRA_PUBLISH_BRANCH_<NAME>` overrides that). `<NAME>` is `name` uppercased with `-`
/// mapped to `_` (bash: `tr 'a-z-' 'A-Z_'` — only lowercase letters and `-` are touched).
pub fn publish_forge(reg: &Registry, name: &str, env: &BTreeMap<String, String>) -> Option<(String, String)> {
    let base = landref(reg, name)?;
    let key: String = name
        .chars()
        .map(|c| match c {
            'a'..='z' => c.to_ascii_uppercase(),
            '-' => '_',
            other => other,
        })
        .collect();
    let remote = env
        .get(&format!("SPIRA_PUBLISH_REMOTE_{key}"))
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(|| env.get("SPIRA_PUBLISH_REMOTE").filter(|v| !v.is_empty()).cloned())
        .unwrap_or_else(|| "origin".to_string());
    let branch = env
        .get(&format!("SPIRA_PUBLISH_BRANCH_{key}"))
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| base.strip_prefix("local/").unwrap_or(&base).to_string());
    if branch.is_empty() {
        return None;
    }
    Some((remote, branch))
}

/// `_spira_gitstore <path>` — its shared git directory, canonicalized, or `None` when `path`
/// is not inside a git checkout at all. `--git-common-dir` and not `--git-dir`: a worktree has
/// a private git dir and a shared common one, and only the shared one identifies the
/// repository (see [`same_repo`]'s own doc).
fn gitstore(path: &str) -> Option<PathBuf> {
    let out = Command::new("git").args(["-C", path, "rev-parse", "--git-common-dir"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let d = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if d.is_empty() {
        return None;
    }
    // bash: `cd "$path" && … && cd "$d" && pwd -P` — a relative git-common-dir resolves
    // against `path` (the directory bash was already in when it asked), never against the
    // process's own cwd.
    let full = if Path::new(&d).is_absolute() { PathBuf::from(&d) } else { Path::new(path).join(&d) };
    std::fs::canonicalize(&full).ok()
}

/// `spira_same_repo <a> <b>` — by object store, never by path string: a worktree and the
/// checkout it was cut from are one repository under two paths, and every aeon works in a
/// worktree while the landing gate extracts one, so a string comparison calls the copy in
/// force "some other repository" and a fence keyed on it fires on every branch.
pub fn same_repo(a: &str, b: &str) -> bool {
    match (gitstore(a), gitstore(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// The registry's inputs — `SPIRA_REPO_MAP`, `SPIRA_HOME_REPO`, `SPIRA_REPO`,
/// `SPIRA_REPO_DERIVED` — filled in when this process's environment lacks them. conf.sh
/// resolves all four but exports none, and a binary run from a unit or a shell that never
/// sourced it (queue, first), so reading the bare environment found no map, no `spira` row, and no landing
/// ref: every land-local refused (sp-z3eyk). Resolve them in-process exactly as conf.sh
/// would; a key the environment already sets wins, and a failed resolve leaves the
/// environment as it was, so the lookup still refuses rather than guessing.
///
/// `SPIRA_REPO_DERIVED` is the one exception to "the environment already sets wins"
/// (sp-8bhnr, LOOP-STOPPING): it is a pure filesystem fact about THIS process's own
/// `home` (`derive_repo_filesystem`'s `git rev-parse --show-toplevel` of `home`, or its
/// parent) — never a caller's to set, and never safe to inherit across a process
/// boundary. CHECK6's own dispatch (`sentinel/src/dispatch.rs`) forwards ITS OWN resolved
/// `SPIRA_REPO`/`SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` into the landing-pass worker it starts,
/// correct for SENTINEL's own `home` (a release's bundled, non-checkout `spira/`, so
/// `SPIRA_REPO` derives to the release ROOT) — but it does not also forward
/// `SPIRA_REPO_DERIVED`. Before this fix, [`repo_override`] read that asymmetry (a
/// forwarded `SPIRA_REPO` with no `SPIRA_REPO_DERIVED` alongside it) as THIS process's own
/// deliberate override and returned the release root for the home repo outright,
/// bypassing the repo-map's real row entirely — the home repo 'spira' resolved to the
/// release directory instead of its configured checkout, and every closed bead's
/// landing record pruned next, reading "no branch" from a registry that could not look.
/// Recomputing it fresh, always, from THIS process's own `home` is the same
/// law-a-binary-resolves-the-config-it-reads fix sp-hh599 made for spira-claim's own
/// `fayth_home`: a derived fact is resolved in-process, never trusted from the
/// environment. When `home` genuinely matches what forwarded it (the ordinary case: a
/// unit's own worker, not a dispatcher's borrowed context), the freshly-derived value
/// equals the forwarded one and nothing changes.
pub fn registry_env(mut env: std::collections::BTreeMap<String, String>, home: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    env.insert(
        "SPIRA_REPO_DERIVED".to_string(),
        crate::resolve::derive_repo_filesystem(home, &env).to_string_lossy().into_owned(),
    );
    const KEYS: [&str; 4] = ["SPIRA_REPO_MAP", "SPIRA_HOME_REPO", "SPIRA_REPO", "SPIRA_REPO_DERIVED"];
    if KEYS.iter().all(|k| env.get(*k).is_some_and(|v| !v.is_empty())) {
        return env;
    }
    // sp-mz7dn ("wave 4.8") gave every crate a proper, one-call way to do this —
    // `resolve_for_process`, with `derive_home_repo` for the `repo` it needs — so this no
    // longer hand-rolls its own locate/load/resolve (and no longer falls back to
    // `home.parent()` for `repo`, which guessed wrong wherever `home` was not a direct
    // child of the checkout root; `derive_repo_filesystem`'s `git rev-parse
    // --show-toplevel` is the correct derivation conf.sh itself uses).
    let repo = env
        .get("SPIRA_REPO")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::resolve::derive_home_repo(home, &env));
    if let Ok(r) = crate::resolve::resolve_for_process(home, &repo, &env) {
        for k in KEYS {
            if env.get(k).is_none_or(|v| v.is_empty()) && !r.get(k).is_empty() {
                env.insert(k.to_string(), r.get(k).to_string());
            }
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_reads_repo_tables_from_spira_toml_when_no_map_file_exists() {
        let doc = crate::validate("[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"queue.local\"\nbase = \"local/main\"\nlanes = [\"plan\", \"spike\"]\n").unwrap();
        let rows = rows_from_toml(&doc);
        assert_eq!(names(&rows), vec!["alpha".to_string()]);
        assert_eq!(field(&rows, "alpha", Column::Path), "/tmp/alpha");
        assert_eq!(field(&rows, "alpha", Column::Land), "queue.local");
        assert_eq!(field(&rows, "alpha", Column::Base), "local/main");
        assert_eq!(field(&rows, "alpha", Column::Lanes), "plan,spike");
        assert_eq!(field(&rows, "alpha", Column::Gate), "");
        assert_eq!(field(&rows, "nope", Column::Path), "");
    }

    /// The repo map is the one spira.toml declares (spira.repo_map) — never discovered from
    /// XDG, a legacy spira.conf or a shipped example (per Ryan 2026-10-05).
    #[test]
    fn registry_env_takes_the_declared_map() {
        let t = testkit::TempDir::new("repos-registry-env");
        let map = t.path().join("my-repo-map");
        std::fs::write(&map, "spira|/nowhere|local|local/main\n").unwrap();
        let toml = crate::fixture_toml_file(t.path(), &[("SPIRA_REPO_MAP".to_string(), map.display().to_string())].into_iter().collect());
        let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let mut env = std::collections::BTreeMap::new();
        env.insert("HOME".to_string(), t.path().display().to_string());
        env.insert("SPIRA_REPO".to_string(), t.path().display().to_string());
        env.insert("SPIRA_TOML".to_string(), toml.display().to_string());
        let repo = std::path::PathBuf::from(env.get("SPIRA_REPO").unwrap());
        if let Err(e) = crate::resolve::resolve_for_process(&home, &repo, &env) { panic!("resolve: {e}"); }
        let out = registry_env(env, &home);
        assert_eq!(out.get("SPIRA_REPO_MAP").map(String::as_str), Some(map.to_str().unwrap()), "{out:?}");
    }

    /// `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO` still win when the environment
    /// already sets them — but `SPIRA_REPO_DERIVED` (sp-8bhnr) is never one of those three:
    /// it is always recomputed fresh from `home`, overwriting whatever the ambient value
    /// was ("0" here is not a real filesystem fact and must not survive).
    #[test]
    fn registry_env_keeps_what_the_environment_already_sets_except_the_derived_fact() {
        let mut env = std::collections::BTreeMap::new();
        for (k, v) in [("SPIRA_REPO_MAP", "/a"), ("SPIRA_HOME_REPO", "h"), ("SPIRA_REPO", "/r"), ("SPIRA_REPO_DERIVED", "0")] {
            env.insert(k.to_string(), v.to_string());
        }
        let home = std::path::Path::new("/nonexistent/spira");
        let out = registry_env(env.clone(), home);
        assert_eq!(out.get("SPIRA_REPO_MAP").map(String::as_str), Some("/a"));
        assert_eq!(out.get("SPIRA_HOME_REPO").map(String::as_str), Some("h"));
        assert_eq!(out.get("SPIRA_REPO").map(String::as_str), Some("/r"));
        // A nonexistent `home` canonicalizes to nothing, so `derive_repo_filesystem` falls
        // back to `home` itself (its own doc) — never the stale "0".
        assert_eq!(out.get("SPIRA_REPO_DERIVED").map(String::as_str), Some("/nonexistent/spira"));
    }

    /// sp-8bhnr (P0, LOOP-STOPPING): CHECK6's own dispatch forwards its OWN resolved
    /// `SPIRA_REPO`/`SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` into the landing-pass worker it
    /// starts, but never `SPIRA_REPO_DERIVED` (`sentinel/src/dispatch.rs`'s `setenv` list
    /// omits it). Sentinel's own `home` under its unit is a release's bundled, non-checkout
    /// `spira/`, so its own `SPIRA_REPO` derives to the release ROOT — and when that gets
    /// forwarded into landing-pass, which shares the SAME `SPIRA_HOME`,
    /// [`repo_override`] used to read the missing `SPIRA_REPO_DERIVED` as a deliberate
    /// override and hand back the release root for the home repo outright, instead of its
    /// real configured checkout. Seen red against the pre-fix code (which never backfills
    /// `SPIRA_REPO_DERIVED` at all — `resolve()` never puts it in `Resolved::values`).
    #[test]
    fn registry_resolves_the_configured_checkout_despite_a_forwarded_repo_without_its_derived_pair() {
        let t = testkit::TempDir::new("repos-registry-sp-8bhnr");

        // The REAL configured checkout for the home repo "spira" (production: spira.toml
        // and repo-map both name the real harness checkout; here, a fixture equivalent).
        let checkout = t.path().join("checkouts/spira");
        std::fs::create_dir_all(&checkout).unwrap();
        let map = t.path().join("repo-map");
        std::fs::write(&map, format!("spira | {} | queue.local | local/main |  |\n", checkout.display())).unwrap();

        // The release's own bundled `spira/` — NOT a git checkout (a release layout, not a
        // clone), exactly sp-8bhnr's own repro. Only `conf.d` needs to be real (resolve()
        // reads it unconditionally); borrow this checkout's own, real one.
        let release = t.path().join("spira-releases/deadbeef");
        let release_spira = release.join("spira");
        std::fs::create_dir_all(&release_spira).unwrap();
        std::os::unix::fs::symlink(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"),
            release_spira.join("conf.d"),
        )
        .unwrap();

        // CHECK6's own forwarded env: SPIRA_REPO = sentinel's own derived value for THIS
        // SAME home (the release root — `derive_repo_filesystem` falls back to `home`'s
        // parent when `git -C home rev-parse --show-toplevel` fails), SPIRA_REPO_MAP and
        // SPIRA_HOME_REPO forwarded too, SPIRA_REPO_DERIVED deliberately absent.
        let home_dir = t.path().join("userhome");
        let mut env = std::collections::BTreeMap::new();
        env.insert("HOME".to_string(), home_dir.display().to_string());
        env.insert("SPIRA_REPO".to_string(), release.canonicalize().unwrap_or_else(|_| release.clone()).display().to_string());
        env.insert("SPIRA_REPO_MAP".to_string(), map.display().to_string());
        env.insert("SPIRA_HOME_REPO".to_string(), "spira".to_string());

        let reg = Registry::from_env(env, &release_spira);
        assert_eq!(
            reg.root("spira"),
            Some(checkout.display().to_string()),
            "the home repo 'spira' must resolve to its configured checkout, not the forwarded SPIRA_REPO (the release root)"
        );
    }


    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const MAP: &str = "\
# a comment line, skipped
home | /h/home | push | origin/main |  | .claude/gate-home.sh
narrow | /h/narrow
service | /h/service | pr | origin/master | cargo fmt --all | cargo fmt --all -- --check
lanes-on | /h/lanes | push | origin/main | | bash x.sh | plan,incident
lanes-empty | /h/lanesempty | push | origin/main | | bash x.sh |
oldshape | /h/old | pr | fmt-tool | gate --flag
";

    #[test]
    fn parse_skips_comments_and_short_rows_but_keeps_a_two_field_row() {
        let rows = parse(MAP);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["home", "narrow", "service", "lanes-on", "lanes-empty", "oldshape"]);
        let narrow = rows.iter().find(|r| r.name == "narrow").unwrap();
        assert_eq!(narrow.path, "/h/narrow");
        assert_eq!(narrow.land, "");
        assert_eq!(narrow.base, "");
        assert_eq!(narrow.format, "");
        assert_eq!(narrow.gate, "");
    }

    #[test]
    fn six_field_row_reads_base_format_and_gate_by_name_not_position() {
        let rows = parse(MAP);
        let home = rows.iter().find(|r| r.name == "home").unwrap();
        assert_eq!(home.path, "/h/home");
        assert_eq!(home.land, "push");
        assert_eq!(home.base, "origin/main");
        assert_eq!(home.format, "");
        assert_eq!(home.gate, ".claude/gate-home.sh");
    }

    #[test]
    fn gate_may_itself_contain_pipes() {
        let rows = parse(MAP);
        let svc = rows.iter().find(|r| r.name == "service").unwrap();
        assert_eq!(svc.gate, "cargo fmt --all -- --check");
    }

    #[test]
    fn five_field_row_predating_base_reads_format_at_column_four() {
        let rows = parse(MAP);
        let old = rows.iter().find(|r| r.name == "oldshape").unwrap();
        assert_eq!(old.base, "");
        assert_eq!(old.format, "fmt-tool");
        assert_eq!(old.gate, "gate --flag");
    }

    #[test]
    fn a_trailing_identifier_field_is_lanes_not_gate() {
        let rows = parse(MAP);
        let r = rows.iter().find(|r| r.name == "lanes-on").unwrap();
        assert_eq!(r.gate, "bash x.sh");
        assert_eq!(r.lanes, "plan,incident");
    }

    #[test]
    fn an_explicit_empty_trailing_field_is_still_a_lanes_column() {
        let rows = parse(MAP);
        let r = rows.iter().find(|r| r.name == "lanes-empty").unwrap();
        assert_eq!(r.gate, "bash x.sh");
        assert_eq!(r.lanes, "");
    }

    #[test]
    fn field_is_blank_for_an_unknown_name_not_an_error() {
        let rows = parse(MAP);
        assert_eq!(field(&rows, "ghost", Column::Path), "");
    }

    #[test]
    fn names_preserves_file_order() {
        let rows = parse(MAP);
        assert_eq!(names(&rows), vec!["home", "narrow", "service", "lanes-on", "lanes-empty", "oldshape"]);
    }

    #[test]
    fn home_repo_prefers_the_explicit_key_then_falls_back_to_a_basename() {
        assert_eq!(home_repo(&env(&[("SPIRA_HOME_REPO", "brain")]), Path::new("/x/y")), "brain");
        assert_eq!(home_repo(&env(&[("SPIRA_REPO", "/w/harness")]), Path::new("/x/y")), "harness");
        assert_eq!(home_repo(&env(&[]), Path::new("/x/spira")), "spira");
        assert_eq!(home_repo(&env(&[("SPIRA_HOME_REPO", "")]), Path::new("/x/spira/")), "spira");
    }

    #[test]
    fn registry_root_prefers_the_map_and_refuses_an_unmapped_name() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.root("home"), Some("/h/home".to_string()));
        assert_eq!(reg.root("ghost"), None);
        assert_eq!(reg.root(""), Some("/h/home".to_string()), "empty name defaults to the home repo");
    }

    #[test]
    fn registry_root_takes_an_explicit_spira_repo_override_for_the_home_repo_only() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/override/path"), ("SPIRA_REPO_DERIVED", "/derived")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.root("home"), Some("/override/path".to_string()));
        // a non-home name is never overridden, even though SPIRA_REPO is set
        assert_eq!(reg.root("service"), Some("/h/service".to_string()));
    }

    #[test]
    fn registry_root_treats_spira_repo_equal_to_derived_as_no_override() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/derived"), ("SPIRA_REPO_DERIVED", "/derived")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        // falls through to the map, which has home at /h/home, not /derived
        assert_eq!(reg.root("home"), Some("/h/home".to_string()));
    }

    #[test]
    fn registry_land_defaults_to_push_and_normalizes_queue_forge() {
        let env = env(&[]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.land("home"), "push");
        assert_eq!(reg.land("ghost"), "push");
        let qf = "q | /q | queue.forge | origin/main | |\n";
        let reg2 = Registry::new(Some(qf), &env, Path::new("/h/home"));
        assert_eq!(reg2.land("q"), "queue");
        assert!(reg2.land_queued("q"));
        assert!(!reg2.land_queued("ghost"));
    }

    #[test]
    fn registry_land_ignores_a_missing_map_rather_than_refusing() {
        let env = env(&[]);
        let reg = Registry::new(None, &env, Path::new("/h/home"));
        assert_eq!(reg.land("anything"), "push");
        assert!(!reg.map_present());
    }

    #[test]
    fn registry_field_lookups_refuse_outright_when_the_map_is_absent() {
        let base_env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(None, &base_env, Path::new("/h/home"));
        assert_eq!(reg.field("home", Column::Path), None);
        assert_eq!(reg.gate("home"), None);
        assert_eq!(reg.format("home"), None);
        assert_eq!(reg.base("home"), None);
        // root is unaffected when the override already answers it...
        let env_ov = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/ov"), ("SPIRA_REPO_DERIVED", "/d")]);
        let reg_ov = Registry::new(None, &env_ov, Path::new("/h/home"));
        assert_eq!(reg_ov.root("home"), Some("/ov".to_string()));
        // ...but refuses otherwise, same as any other repo_field-based lookup.
        assert_eq!(reg.root("home"), None);
    }

    #[test]
    fn registry_all_lists_the_home_repo_first_and_excludes_it_from_the_rest() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        let all = reg.all();
        assert_eq!(all[0], "home");
        assert_eq!(all.iter().filter(|n| n.as_str() == "home").count(), 1);
        assert!(all.contains(&"service".to_string()));
    }

    #[test]
    fn registry_all_always_lists_the_home_repo_even_with_no_map_at_all() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(None, &env, Path::new("/h/home"));
        assert_eq!(reg.all(), vec!["home".to_string()]);
    }

    #[test]
    fn registry_name_at_matches_literally_and_the_override_maps_to_home() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/ov"), ("SPIRA_REPO_DERIVED", "/d")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.name_at("/ov"), Some("home".to_string()));
        assert_eq!(reg.name_at("/h/service"), Some("service".to_string()));
        assert_eq!(reg.name_at("/nowhere"), None);
        assert_eq!(reg.name_at(""), None);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
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
    fn same_repo_is_true_for_a_worktree_of_the_same_checkout() {
        let dir = testkit::TempDir::new("spira-config-repos-same-repo");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        run_git(&repo, &["add", "f"]);
        run_git(&repo, &["commit", "-q", "-m", "x"]);
        let wt = dir.join("wt");
        run_git(&repo, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
        assert!(same_repo(repo.to_str().unwrap(), wt.to_str().unwrap()));
    }

    #[test]
    fn same_repo_is_false_for_two_unrelated_checkouts() {
        let dir = testkit::TempDir::new("spira-config-repos-different-repos");
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        run_git(&a, &["init", "-q"]);
        run_git(&b, &["init", "-q"]);
        assert!(!same_repo(a.to_str().unwrap(), b.to_str().unwrap()));
    }

    #[test]
    fn same_repo_is_false_when_either_path_is_not_a_git_checkout() {
        let dir = testkit::TempDir::new("spira-config-repos-not-a-repo");
        assert!(!same_repo(dir.path().to_str().unwrap(), dir.path().to_str().unwrap()));
    }

    // ---- base refs (family W, sp-o88bx) ---------------------------------------------------

    fn init_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "-q", "-b", branch]);
        std::fs::write(dir.join("f"), "x").unwrap();
        run_git(dir, &["add", "f"]);
        run_git(dir, &["commit", "-q", "-m", "x"]);
    }

    fn registry_for(map: &str, home_name: &str) -> Registry {
        Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", home_name)]), Path::new("/nonexistent"))
    }

    #[test]
    fn ref_branch_strips_up_to_the_first_slash_or_passes_through() {
        assert_eq!(ref_branch("origin/master"), "master");
        assert_eq!(ref_branch("local/main"), "main");
        assert_eq!(ref_branch("main"), "main");
    }

    #[test]
    fn ref_remote_reads_the_prefix_and_a_repo_narrows_it_to_a_real_remote() {
        assert_eq!(ref_remote("main", None), None, "no slash at all");
        assert_eq!(ref_remote("origin/main", None), Some("origin".to_string()), "no repo: unconditional split");
        let dir = testkit::TempDir::new("spira-config-repos-ref-remote");
        let repo = dir.join("repo");
        init_repo(&repo, "main");
        let repo = repo.to_str().unwrap();
        assert_eq!(ref_remote("origin/main", Some(repo)), None, "no remote named origin here");
        assert_eq!(
            ref_remote("local/main", Some(repo)), None,
            "a queue.local ref: the 'local' prefix is not a real remote, so it reads as local, not remote 'local'"
        );
    }

    #[test]
    fn qualify_base_ref_qualifies_only_a_real_remote_tracking_ref() {
        let dir = testkit::TempDir::new("spira-config-repos-qualify");
        let upstream = dir.join("upstream");
        init_repo(&upstream, "main");
        let local = dir.join("local");
        run_git(dir.path(), &["clone", "-q", upstream.to_str().unwrap(), local.to_str().unwrap()]);
        let local = local.to_str().unwrap();
        assert_eq!(qualify_base_ref("origin/main", local), "refs/remotes/origin/main");
        // local/main is not a remote-tracking ref here at all (no remote named "local").
        assert_eq!(qualify_base_ref("local/main", local), "local/main");
        assert_eq!(qualify_base_ref("main", local), "main", "no slash: unchanged");
    }

    #[test]
    fn landref_prefers_the_declared_base_when_it_verifies() {
        let dir = testkit::TempDir::new("spira-config-repos-landref-declared");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        run_git(&repo, &["branch", "-q", "release"]);
        let map = format!("r | {} | push | release |  | \n", repo.display());
        let reg = registry_for(&map, "r");
        assert_eq!(landref(&reg, "r"), Some("release".to_string()));
        // by path, not by name, reads the same declared base once the name is recovered via name_at.
        assert_eq!(landref(&reg, repo.to_str().unwrap()), Some("release".to_string()));
    }

    #[test]
    fn landref_refuses_a_declared_base_that_does_not_exist() {
        let dir = testkit::TempDir::new("spira-config-repos-landref-bad-declared");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        let map = format!("r | {} | push | no-such-branch |  | \n", repo.display());
        let reg = registry_for(&map, "r");
        // rung 1 fails outright rather than falling through — a declared base naming a ref
        // this checkout does not have is the defect, not a cue to guess further.
        assert_eq!(landref(&reg, "r"), None);
    }

    #[test]
    fn landref_falls_back_to_head_when_there_is_no_remote_at_all() {
        let dir = testkit::TempDir::new("spira-config-repos-landref-head");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        let map = format!("r | {}\n", repo.display());
        let reg = registry_for(&map, "r");
        assert_eq!(landref(&reg, "r"), Some("trunk".to_string()));
    }

    #[test]
    fn landref_asks_the_remote_once_when_origin_head_is_not_yet_cached() {
        let dir = testkit::TempDir::new("spira-config-repos-landref-remote");
        let upstream = dir.join("upstream");
        init_repo(&upstream, "main");
        let local = dir.join("local");
        run_git(dir.path(), &["clone", "-q", upstream.to_str().unwrap(), local.to_str().unwrap()]);
        // A clone caches refs/remotes/origin/HEAD itself; drop it to exercise rung 3 ("ask
        // the remote once") rather than rung 2 reading clone's own cache.
        let _ = std::fs::remove_file(local.join(".git/refs/remotes/origin/HEAD"));
        let map = format!("r | {}\n", local.display());
        let reg = registry_for(&map, "r");
        assert_eq!(landref(&reg, "r"), Some("origin/main".to_string()));
    }

    #[test]
    fn landref_refuses_an_unmapped_name_and_a_missing_git_dir() {
        let reg = registry_for("r | /nope\n", "r");
        assert_eq!(landref(&reg, "ghost"), None, "unmapped name");
        assert_eq!(landref(&reg, "r"), None, "mapped but not a real checkout");
    }

    #[test]
    fn landrefs_pairs_the_base_with_its_verified_local_counterpart() {
        let dir = testkit::TempDir::new("spira-config-repos-landrefs");
        let upstream = dir.join("upstream");
        init_repo(&upstream, "main");
        let local = dir.join("local");
        run_git(dir.path(), &["clone", "-q", upstream.to_str().unwrap(), local.to_str().unwrap()]);
        let local = local.to_str().unwrap();
        let map = format!("r | {local}\n");
        let reg = registry_for(&map, "r");
        // declared base is origin/main (rung 2, clone's own cache); the local "main" branch
        // the clone checked out also exists, so both are reported.
        assert_eq!(landrefs(&reg, local), Some(("origin/main".to_string(), Some("main".to_string()))));
    }

    #[test]
    fn landrefs_omits_the_local_counterpart_when_it_does_not_exist() {
        let dir = testkit::TempDir::new("spira-config-repos-landrefs-no-local");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        run_git(&repo, &["branch", "-q", "release"]);
        let map = format!("r | {} | push | release |  | \n", repo.display());
        let reg = registry_for(&map, "r");
        // base is "release" (no slash at all), so ref_branch(base) == base: never a distinct
        // "local counterpart" to go looking for.
        assert_eq!(landrefs(&reg, repo.to_str().unwrap()), Some(("release".to_string(), None)));
    }

    #[test]
    fn publish_forge_defaults_to_origin_and_the_land_ref_stripped_of_local() {
        let dir = testkit::TempDir::new("spira-config-repos-publish-forge-default");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        run_git(&repo, &["branch", "-q", "local/trunk"]);
        let map = format!("q | {} | push | local/trunk |  | \n", repo.display());
        let reg = registry_for(&map, "q");
        assert_eq!(publish_forge(&reg, "q", &env(&[])), Some(("origin".to_string(), "trunk".to_string())));
    }

    #[test]
    fn publish_forge_takes_a_per_name_override_over_the_default() {
        let dir = testkit::TempDir::new("spira-config-repos-publish-forge-override");
        let repo = dir.join("repo");
        init_repo(&repo, "trunk");
        run_git(&repo, &["branch", "-q", "local/trunk"]);
        let map = format!("my-queue | {} | push | local/trunk |  | \n", repo.display());
        let reg = registry_for(&map, "my-queue");
        let overrides = env(&[("SPIRA_PUBLISH_REMOTE_MY_QUEUE", "upstream"), ("SPIRA_PUBLISH_BRANCH_MY_QUEUE", "release")]);
        assert_eq!(publish_forge(&reg, "my-queue", &overrides), Some(("upstream".to_string(), "release".to_string())));
        // the blanket SPIRA_PUBLISH_REMOTE only applies when no per-name override exists.
        let blanket = env(&[("SPIRA_PUBLISH_REMOTE", "upstream")]);
        assert_eq!(publish_forge(&reg, "my-queue", &blanket), Some(("upstream".to_string(), "trunk".to_string())));
    }

    #[test]
    fn publish_forge_refuses_when_the_land_ref_itself_does_not_resolve() {
        let reg = registry_for("q | /nope\n", "q");
        assert_eq!(publish_forge(&reg, "q", &env(&[])), None);
    }

    #[test]
    fn landref_resolves_a_declared_local_main_from_a_linked_worktree() {
        let t = testkit::TempDir::new("landref-linked-worktree");
        let main = t.path().join("main");
        let wt = t.path().join("wt");
        let git = |dir: &Path, args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "local/main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "c"]);
        git(&main, &["worktree", "add", "-q", "-b", "spira/x", wt.to_str().unwrap()]);
        assert!(wt.join(".git").is_file(), "fixture must be a linked worktree");
        let map = format!("q | {} | local | local/main | | true\n", wt.display());
        let reg = registry_for(&map, "q");
        assert_eq!(landref(&reg, wt.to_str().unwrap()), Some("local/main".to_string()));
        assert_eq!(landref(&reg, "q"), Some("local/main".to_string()));
    }
}
