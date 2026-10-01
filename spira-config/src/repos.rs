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
    /// `map_text` is `None` when `SPIRA_REPO_MAP` does not exist — "no map" is not a parse
    /// error (lib.sh: `[ -f "$SPIRA_REPO_MAP" ] || return 0/1`, depending on the caller; see
    /// [`Registry::map_present`]).
    pub fn new(map_text: Option<&str>, env: &BTreeMap<String, String>, home: &Path) -> Registry {
        Registry {
            rows: map_text.map(parse).unwrap_or_default(),
            home: home_repo(env, home),
            root_override: repo_override(env),
            map_present: map_text.is_some(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
