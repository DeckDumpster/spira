//! The runes / rune registry — spira/lib.sh family D (wave 4 decomposition, row D;
//! sp-r5zd2). lib.sh's own `rune_names`,
//! `spira_runes` (and its `spira_task_runes`/`spira_lane_runes` splits), `rune_get`,
//! `rune_partitions`, `runes_for_labels` and `persona_model` all move here; lib.sh keeps
//! one-line shims onto the `spira-config rune ...` CLI this module backs (see `main.rs`),
//! so the 50-odd bash sourcers keep working unchanged.
//!
//! `rune_get` is the one function here that is NOT a plain lookup: a `.rune` file is an
//! arbitrary shell fragment (fixtures use `${SPIRA_SCOPE_LABEL:+...}` parameter expansion),
//! sourced in a subshell by the original bash. This bridges to the exact same mechanism —
//! `bash -c '. "$f"; eval ...'` — the `bead` crate's own copy already uses (its own
//! comment: "rather than writing a second, partial shell-fragment parser"), instead of
//! reimplementing a shell-fragment evaluator in Rust.
//!
//! `rune_get`'s bash subshell needs every config key a `.rune` file's `RUNE_LABELS`/
//! `RUNE_EXCLUDE_LABELS` references by parameter expansion — `SPIRA_CZAR_LABEL`,
//! `SPIRA_INCIDENT_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL`, and so on — in its OWN
//! environment, because an unset expansion is silently empty, not an error, and this
//! process's own ambient environment (a bare `spira-sentinel.service`, never conf.sh)
//! carries none of them. Wave 4.9 (sp-k80sa) retired lib.sh's own `export` of a five-key
//! slice of these (its one-line shim onto this module's `rune_get`) on the strength of
//! [`rune_label_overlay`] resolving THAT SLICE in-process instead — which is exactly the
//! defect sp-xsnid found: a hand-curated list drifts the moment a persona's predicate
//! grows a new variable, and it had, by five (`SPIRA_INCIDENT_LABEL`, `SPIRA_SPIKE_LABEL`,
//! `SPIRA_PLAN_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL`, `SPIRA_CI_LABEL` — every
//! real rune's own label variable and both of `RUNE_EXCLUDE_LABELS`'s, none of them in
//! the five-key list). [`rune_label_overlay`] now hands the subshell EVERY key
//! [`crate::resolve::resolve_for_process`] resolved, not a slice of it, so no future rune
//! variable can repeat that drift. [`rune_predicate`] is the fail-closed half on top: a
//! `.rune`'s own text may still reference a variable that is empty for a legitimate
//! reason (`SPIRA_SCOPE_LABEL`'s `${VAR:+...}` guard, present in every real rune, is
//! exactly that — "no scope restriction configured" is this harness's own common case) or
//! for a genuine config gap, and only the fail-closed check below can tell those apart.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::{discover, load, SpiraToml};

/// The pure half of [`rune_label_overlay`]: every key an
/// already-[`crate::resolve::resolve_for_process`]d [`crate::resolve::Resolved`] carries,
/// as the subshell's overlay — sp-xsnid widened this from a hand-curated five-key slice
/// (`SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/`SPIRA_MIMIR_LABEL`/
/// `SPIRA_BATCH_JUDGEMENT_LABEL`/`SPIRA_HOME_REPO`) to the FULL resolved map, because the
/// slice had already drifted: it never carried `SPIRA_INCIDENT_LABEL`, `SPIRA_SPIKE_LABEL`,
/// `SPIRA_PLAN_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL` or `SPIRA_CI_LABEL` — every
/// other real rune's own label variable, and both of `RUNE_EXCLUDE_LABELS`'s. Split out
/// so it is unit-testable without a `discover()` call touching this process's real
/// environment — the same reason `persona_model_from_doc` exists alongside `persona_model`
/// below, and the same hazard: a test that called the `discover`-touching half directly
/// would need `testkit::env` (the workspace-wide env serialization
/// against another test's `std::env::set_var`), and holding that lock here while a caller
/// above is ALSO holding it to drive its own env mutation would deadlock (`std::sync::Mutex`
/// is not reentrant) — so this half takes no lock and touches no env at all.
fn process_env() -> BTreeMap<String, String> {
    #[cfg(test)]
    let _held = testkit::env_read();
    #[cfg(test)]
    return std::env::vars().filter(|(k, _)| k != "SPIRA_TOML").collect();
    #[cfg(not(test))]
    std::env::vars().collect()
}

fn extract_label_overlay(resolved: &crate::resolve::Resolved) -> BTreeMap<String, String> {
    resolved.values.clone()
}

/// Resolves the FULL config via [`crate::resolve::resolve_for_process`] (env > toml >
/// derived default — the same precedence `conf.sh` always applied), so [`rune_get`] can
/// hand it to its bash subshell explicitly rather than relying on this process's own
/// ambient environment, which a bare unit (`spira-sentinel.service`: `SPIRA_RELEASE`/
/// `PATH` only) never carries any of it in. A resolution failure (no config document
/// resolves, or a parse error) yields an empty overlay: the bash subshell then falls back
/// to whatever it would have seen anyway, never a hard failure over a label — the EMPTY
/// half of that fallback is exactly what [`rune_predicate`]'s own fail-closed check
/// exists to catch instead of letting it widen a partition silently.
///
/// RESOLVED ONCE PER PROCESS per (home, environment): every [`rune_get`] asked for it, and one
/// `sentinel --call` makes ~200 such lookups — each re-deriving the repo through git and
/// re-resolving the whole config (~100 ms), 19 s of a pass. A process pins the config it
/// started with (law-long-lived-processes-pin-their-config).
fn rune_label_overlay(home: &Path) -> BTreeMap<String, String> {
    let env = process_env();
    memo(&OVERLAY, home, &env, || rune_label_overlay_uncached(home, &env))
}

type GotMap = std::sync::Mutex<std::collections::HashMap<(PathBuf, Option<std::time::SystemTime>, String, String, BTreeMap<String, String>), String>>;
type MemoMap<V> = std::sync::Mutex<std::collections::HashMap<(PathBuf, BTreeMap<String, String>), V>>;
static OVERLAY: std::sync::OnceLock<MemoMap<BTreeMap<String, String>>> = std::sync::OnceLock::new();
static RUNES: std::sync::OnceLock<MemoMap<PathBuf>> = std::sync::OnceLock::new();

fn memo<V: Clone>(cell: &std::sync::OnceLock<MemoMap<V>>, home: &Path, env: &BTreeMap<String, String>, f: impl FnOnce() -> V) -> V {
    let m = cell.get_or_init(Default::default);
    let key = (home.to_path_buf(), env.clone());
    if let Some(v) = m.lock().ok().and_then(|g| g.get(&key).cloned()) {
        return v;
    }
    let v = f();
    if let Ok(mut g) = m.lock() {
        g.insert(key, v.clone());
    }
    v
}

fn rune_label_overlay_uncached(home: &Path, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let repo = crate::resolve::derive_home_repo(home, env);
    crate::resolve::resolve_for_process(home, &repo, env)
        .map(|r| extract_label_overlay(&r))
        .unwrap_or_default()
}

/// The configured `SPIRA_RUNES_DIR` (env, then config document, then `<home>/runes`) — the
/// one directory every function here resolves a rune against. `SPIRA_RUNES_DIR` is never
/// exported, so a bare caller only sees it by resolving the config in-process.
pub fn runes_dir(home: &Path) -> PathBuf {
    let env = process_env();
    memo(&RUNES, home, &env, || runes_dir_with(home, &env))
}

fn runes_dir_with(home: &Path, env: &BTreeMap<String, String>) -> PathBuf {
    if let Some(v) = env.get("SPIRA_RUNES_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let repo = crate::resolve::derive_home_repo(home, env);
    crate::resolve::resolve_for_process(home, &repo, env)
        .ok()
        .and_then(|r| r.values.get("SPIRA_RUNES_DIR").cloned())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("runes"))
}

/// `rune_names` — every persona defined in the runes, sorted (bash's own glob
/// expansion sorts lexicographically; matched here with a plain `Vec::sort`, same as the
/// `bead` crate's existing copy). Empty when the runes directory is absent or holds no
/// `*.rune` file — never an error, the same "nothing found" an unmatched bash glob
/// collapses to.
pub fn rune_names(home: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(runes_dir(home)) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("rune") {
                    p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// `rune_get <rune> <var> [default]` — one field of a rune: `<home>/runes/<rune>.rune`
/// read in-process (see [`crate::rune_file`]), `default` when the field is unset or empty.
/// A missing file returns `default`; a file with a construct outside the supported subset is
/// refused on stderr, naming file and line, and also returns `default`.
pub fn rune_get(home: &Path, rune: &str, var: &str, default: &str) -> String {
    let f = runes_dir(home).join(format!("{rune}.rune"));
    if !f.is_file() {
        return default.to_string();
    }
    // ONE read per (file as it stands, var, default, overlay) per process: a call asked the
    // same few fields of the same six runes ~200 times. The file's mtime is in the key, so an
    // edited rune is read again.
    static GOT: std::sync::OnceLock<GotMap> = std::sync::OnceLock::new();
    let overlay = rune_label_overlay(home);
    let mtime = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
    let key = (f.clone(), mtime, var.to_string(), default.to_string(), overlay.clone());
    let got = GOT.get_or_init(Default::default);
    if let Some(v) = got.lock().ok().and_then(|g| g.get(&key).cloned()) {
        return v;
    }
    let ambient = process_env();
    let lookup = |n: &str| overlay.get(n).or_else(|| ambient.get(n)).cloned();
    let v = match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| crate::rune_file::parse(&f.to_string_lossy(), &t, &lookup)) {
        Ok(vars) => vars
            .get(var)
            .cloned()
            .or_else(|| lookup(var))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string()),
        Err(e) => {
            eprintln!("rune_get: refusing {e}");
            default.to_string()
        }
    };
    if let Ok(mut g) = got.lock() {
        g.insert(key, v.clone());
    }
    v
}

/// A `.rune`'s partition predicate, fully evaluated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Predicate {
    pub labels: String,
    pub exclude_labels: String,
}

/// Why [`rune_predicate`] refused rather than handing back a predicate that would widen
/// a partition to the whole ready queue (sp-xsnid, law-a-control-that-cannot-check-must-
/// refuse). `var` is `RUNE_LABELS` or `RUNE_EXCLUDE_LABELS`; `reference` is the ONE bare
/// (unguarded) `$NAME` the rune's own text uses that resolved empty — never every name in
/// the expression, so the message points at the exact config key to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub rune: String,
    pub var: String,
    pub reference: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.rune: {} references ${} unguarded, and it resolved empty — refusing rather than widening this partition to the whole ready queue",
            self.rune, self.var, self.reference,
        )
    }
}

/// Every `NAME` `text` references via `${NAME:+...}` or `${NAME:-...}` — guarded forms
/// exist precisely so an unset/empty `NAME` is fine (`SPIRA_SCOPE_LABEL` is guarded in
/// every real `RUNE_LABELS`: "no scope restriction configured" is this harness's own
/// common case, not a defect). A name guarded ANYWHERE in `text` is treated as optional
/// EVERYWHERE in it — simpler than tracking which brace a later bare occurrence sits
/// inside, and sufficient for every real `.rune` file, which never mixes a guarded and a
/// required use of the same name.
fn guarded_names(text: &str) -> HashSet<String> {
    let b = text.as_bytes();
    let mut out = HashSet::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'$' && b[i + 1] == b'{' {
            let start = i + 2;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start && j + 1 < b.len() && b[j] == b':' && matches!(b[j + 1], b'+' | b'-') {
                out.insert(text[start..j].to_string());
            }
        }
        i += 1;
    }
    out
}

/// Every DISTINCT `NAME` `text` references via `$NAME`, `${NAME}`, `${NAME:+...}` or
/// `${NAME:-...}` — every shell parameter expansion at all, in first-appearance order.
fn referenced_names(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            let braced = i + 1 < b.len() && b[i + 1] == b'{';
            let start = if braced { i + 2 } else { i + 1 };
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start {
                let name = text[start..j].to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The names `text` references that it never guards with `${NAME:+...}`/`${NAME:-...}`
/// anywhere — [`referenced_names`] minus [`guarded_names`] — the ones a predicate NEEDS
/// resolved non-empty, in doc order.
fn bare_references(text: &str) -> Vec<String> {
    let guarded = guarded_names(text);
    referenced_names(text).into_iter().filter(|n| !guarded.contains(n)).collect()
}

/// The RHS of the LAST `VAR=...` assignment in `text` (bash reassignment semantics: a
/// later line wins — every real `.rune` only assigns each var once, but a fixture or a
/// future file might not), surrounding double quotes stripped when the whole RHS is one
/// quoted string (every real `.rune` writes it that way). `None` when `text` never
/// assigns `var` at all.
fn assigned_value(text: &str, var: &str) -> Option<String> {
    let prefix = format!("{var}=");
    let mut found = None;
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(&prefix) {
            found = Some(rest.to_string());
        }
    }
    found.map(|v| v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).map(str::to_string).unwrap_or(v))
}

pub const WORKING_EXITS_KEY: &str = "RUNE_WORKING_EXITS";

/// What a rune may declare as a way out of WORKING; `ask`, `park` and `hold` are the
/// non-terminal exits, `SUBMITTED` and `DONE` the deliveries.
pub const WORKING_EXIT_VOCABULARY: [&str; 5] = ["SUBMITTED", "DONE", "ask", "park", "hold"];

/// The states a bead claimed by `rune` may leave WORKING for, read from the rune file in
/// `runes` itself. There is no default: a file that is absent, or does not declare the key,
/// or declares a word outside [`WORKING_EXIT_VOCABULARY`], is an error naming what is wrong.
pub fn working_exits_in(runes: &Path, rune: &str) -> Result<Vec<String>, String> {
    let path = runes.join(format!("{rune}.rune"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw = assigned_value(&text, WORKING_EXITS_KEY).unwrap_or_default();
    let exits: Vec<String> = raw.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).map(str::to_string).collect();
    if exits.is_empty() {
        return Err(format!("{}: {WORKING_EXITS_KEY} is not declared", path.display()));
    }
    if let Some(bad) = exits.iter().find(|w| !WORKING_EXIT_VOCABULARY.contains(&w.as_str())) {
        return Err(format!("{}: {WORKING_EXITS_KEY} names {bad:?}; want any of {}", path.display(), WORKING_EXIT_VOCABULARY.join(" ")));
    }
    Ok(exits)
}

/// `RUNE_LABELS`/`RUNE_EXCLUDE_LABELS`, fully resolved and fail-closed — THE shared
/// evaluator every ready-counting/claiming caller (`spira-claim`, sentinel's own
/// `express_ready_in_task_pool`, `bead --for`) goes through, replacing each one's own copy
/// (sp-xsnid: `bead`'s private `rune_get` carried a THIRD, independently drifted
/// three-key overlay — `SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/`SPIRA_MIMIR_LABEL`
/// only — on top of this module's own five-key one; two hand-curated lists, two different
/// sets of gaps).
///
/// Evaluates both vars via [`rune_get`] (now handed the FULL resolved config — see
/// [`rune_label_overlay`]'s own doc), then checks the rune's OWN raw text: any name it
/// references UNGUARDED ([`bare_references`]) that resolved to `""` is a refusal, never a
/// silent "match everything". A rune file that does not exist, or that writes a var as a
/// plain literal with no `$` at all (`concierge.rune`'s own `RUNE_LABELS=""` — a
/// deliberate "no restriction", not a config gap), never refuses: only a REFERENCE that
/// resolves empty does. Callers that need "no partition at all" to mean something
/// (`rune_partitions`) still read that off the plain [`rune_get`], unaffected by this
/// function.
pub fn rune_predicate(home: &Path, rune: &str) -> Result<Predicate, Refusal> {
    let path = runes_dir(home).join(format!("{rune}.rune"));
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let overlay = rune_label_overlay(home);
    for var in ["RUNE_LABELS", "RUNE_EXCLUDE_LABELS"] {
        let Some(raw) = assigned_value(&text, var) else { continue };
        for name in bare_references(&raw) {
            if effective_value(&overlay, &name).is_empty() {
                return Err(Refusal { rune: rune.to_string(), var: var.to_string(), reference: name });
            }
        }
    }
    Ok(Predicate {
        labels: rune_get(home, rune, "RUNE_LABELS", ""),
        exclude_labels: rune_get(home, rune, "RUNE_EXCLUDE_LABELS", ""),
    })
}

/// The value `rune_get`'s own bash subshell actually sees for `name`: the overlay entry
/// if [`rune_label_overlay`] resolved one — even an explicit empty string shadows ambient,
/// exactly as `Command::envs` behaves, since `rune_get` sets the overlay on a `Command`
/// that otherwise inherits this process's full environment — else that AMBIENT
/// environment directly. In-process resolution is not the only source: a caller that
/// sourced the real `conf.sh` already has the right value genuinely exported (every real
/// suite; `test-spike.sh` pins `SPIRA_SPIKE_LABEL=research` by a bare `export`, under a
/// fixture `SPIRA_HOME` with no `conf.d` at all, so [`rune_label_overlay`]'s own
/// `resolve_for_process` call fails outright there — that failure must not be read as "the
/// variable itself is unresolved" when the ambient shell plainly has it).
fn effective_value(overlay: &BTreeMap<String, String>, name: &str) -> String {
    overlay.get(name).cloned().unwrap_or_else(|| std::env::var(name).unwrap_or_default())
}

/// `spira_runes` — the personas this harness runs, space separated, IN PRIORITY ORDER.
/// `env_override` is `$SPIRA_RUNES`, which still wins outright when non-empty (which
/// personas a HOST runs is deployment configuration). Otherwise: every rune in the
/// runes, non-elastic ones first in `rune_names` order, elastic ones (`RUNE_ELASTIC=1`)
/// last — the elastic persona that scales to fill the box must sort behind the on-call
/// ones, not ahead of them alphabetically.
///
/// Replicates the bash original byte for byte, INCLUDING a latent quirk: when there are no
/// fixed runes but at least one elastic one, the result carries a leading space (the bash
/// `${elastic:+ }` is an unconditional literal, not derived from whether `fixed` is itself
/// empty). Harmless for every real caller — all of them `for f in $(spira_runes)`, which
/// word-splits it away — but kept exactly so a byte-for-byte suite comparison still passes.
pub fn spira_runes(home: &Path, env_override: Option<&str>) -> String {
    if let Some(v) = env_override {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let mut fixed = String::new();
    let mut elastic = String::new();
    for f in rune_names(home) {
        if rune_get(home, &f, "RUNE_ELASTIC", "0") == "1" {
            elastic.push(' ');
            elastic.push_str(&f);
        } else {
            fixed.push(' ');
            fixed.push_str(&f);
        }
    }
    let fixed_trimmed = fixed.strip_prefix(' ').unwrap_or(&fixed);
    let elastic_sep = if elastic.is_empty() { "" } else { " " };
    let elastic_trimmed = elastic.strip_prefix(' ').unwrap_or(&elastic);
    format!("{fixed_trimmed}{elastic_sep}{elastic_trimmed}")
}

/// `spira_task_runes` — the personas the sentinel's pool calls: everything from
/// [`spira_runes`] that is not a party member (`RUNE_ROLE=party`) and not a lane rune
/// (`RUNE_LANE` set), and that is callable at all (`RUNE_CALL=auto`, the default).
pub fn spira_task_runes(home: &Path, env_override: Option<&str>) -> String {
    let roster = spira_runes(home, env_override);
    let mut out = String::new();
    for f in roster.split_whitespace() {
        if rune_get(home, f, "RUNE_CALL", "auto") != "auto" {
            continue;
        }
        if rune_get(home, f, "RUNE_ROLE", "task") == "party" {
            continue;
        }
        if !rune_get(home, f, "RUNE_LANE", "").is_empty() {
            continue;
        }
        out.push(' ');
        out.push_str(f);
    }
    out.strip_prefix(' ').unwrap_or(&out).to_string()
}

/// `spira_lane_runes` — the personas that belong to a declared lane (`RUNE_LANE` set),
/// drawn from [`spira_runes`] under the same `RUNE_CALL=auto` gate `spira_task_runes`
/// applies (the sentinel calls from both lists, so a persona excluded from one by
/// `RUNE_CALL` must be excluded from the other too).
pub fn spira_lane_runes(home: &Path, env_override: Option<&str>) -> String {
    let roster = spira_runes(home, env_override);
    let mut out = String::new();
    for f in roster.split_whitespace() {
        if rune_get(home, f, "RUNE_CALL", "auto") != "auto" {
            continue;
        }
        if !rune_get(home, f, "RUNE_LANE", "").is_empty() {
            out.push(' ');
            out.push_str(f);
        }
    }
    out.strip_prefix(' ').unwrap_or(&out).to_string()
}

/// `rune_partitions` — every partition this host watches: one `(labels, exclude_labels)`
/// pair per distinct non-empty `RUNE_LABELS` across [`spira_runes`], deduplicated (two
/// personas may legitimately share a partition) and in roster order. Empty when the
/// runes is empty — callers must treat that as "nothing to watch", never fall back to a
/// hardcoded partition (law-absence-needs-a-positive-control).
pub fn rune_partitions(home: &Path, env_override: Option<&str>) -> Vec<(String, String)> {
    let roster = spira_runes(home, env_override);
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for f in roster.split_whitespace() {
        let labels = rune_get(home, f, "RUNE_LABELS", "");
        if labels.is_empty() || seen.iter().any(|s| s == &labels) {
            continue;
        }
        seen.push(labels.clone());
        let exclude = rune_get(home, f, "RUNE_EXCLUDE_LABELS", "");
        out.push((labels, exclude));
    }
    out
}

/// `runes_for_labels <labels>` — personas whose `RUNE_LABELS` IS exactly `labels`, drawn
/// from [`rune_names`] (every rune in the runes, unlike [`spira_runes`]'s callers:
/// the question here is "is anything working these beads at all", and `SPIRA_RUNES`
/// narrowing the active roster must not hide a rune that could answer it).
pub fn runes_for_labels(home: &Path, want: &str) -> Vec<String> {
    rune_names(home)
        .into_iter()
        .filter(|f| rune_get(home, f, "RUNE_LABELS", "") == want)
        .collect()
}

/// `persona_model <rune> [default]` — the model this persona launches under:
/// `persona.<rune>.model` from the spira.toml this process's own [`discover`]/[`load`]
/// resolve, falling back to `default` (or `claude-opus-5`, lib.sh's own built-in) when no
/// document resolves, the document has no `[persona.<rune>]` table, or its `model` is
/// empty. ALWAYS LOCATES FRESH, never a cached path — a long-lived process (cockpit, a
/// sweep) must see a rune edited after it started, the same requirement lib.sh's own
/// comment carries. Mirrors the dwarf crate's own `conf::persona_model` (sp-z134z,
/// sp-zs04v.4), which reads an already-resolved toml path instead of locating one itself.
pub fn persona_model(rune: &str, default: Option<&str>) -> String {
    const BUILTIN_DEFAULT: &str = "claude-opus-5";
    let def = default.filter(|d| !d.is_empty()).unwrap_or(BUILTIN_DEFAULT);
    let doc = discover(None).and_then(|p| load(&p).ok());
    persona_model_from_doc(doc.as_ref(), rune, def)
}

/// The pure lookup [`persona_model`] runs once it has a document (or none) in hand —
/// split out so it can be unit-tested without touching the process environment, which
/// [`discover`] reads and which every test in this crate's binary shares (see
/// `testkit::env` for why that hazard is real).
fn persona_model_from_doc(doc: Option<&SpiraToml>, rune: &str, default: &str) -> String {
    doc.and_then(|d| d.persona.get(rune))
        .map(|p| p.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_rune(home: &Path, name: &str, body: &str) {
        let dir = runes_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.rune")), body).unwrap();
    }

    #[test]
    fn rune_names_sorts_and_strips_extension() {
        let ws = testkit::TempDir::new("spira-config-runes-names");
        write_rune(&ws, "ops", "RUNE_LABELS=ops\n");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        assert_eq!(rune_names(&ws), vec!["builder".to_string(), "ops".to_string()]);
    }

    #[test]
    fn runes_dir_follows_configured_runes_outside_home() {
        let ws = testkit::TempDir::new("spira-config-runes-configured");
        let elsewhere = ws.join("elsewhere");
        let env: BTreeMap<String, String> =
            [("SPIRA_RUNES_DIR".to_string(), elsewhere.to_string_lossy().into_owned())].into();
        assert_eq!(runes_dir_with(&ws, &env), elsewhere);
    }

    #[test]
    fn working_exits_are_declared_never_defaulted() {
        let ws = testkit::TempDir::new("spira-config-runes-exits");
        write_rune(&ws, "builder", "RUNE_WORKING_EXITS=\"SUBMITTED ask park hold\"\n");
        write_rune(&ws, "bare", "RUNE_NAME=bare\n");
        write_rune(&ws, "odd", "RUNE_WORKING_EXITS=\"SUBMITTED landed\"\n");
        let dir = runes_dir(&ws);
        assert_eq!(working_exits_in(&dir, "builder").unwrap(), ["SUBMITTED", "ask", "park", "hold"]);
        assert!(working_exits_in(&dir, "bare").unwrap_err().contains("RUNE_WORKING_EXITS is not declared"));
        assert!(working_exits_in(&dir, "odd").unwrap_err().contains("\"landed\""));
        assert!(working_exits_in(&dir, "ghost").is_err());
    }

    #[test]
    fn rune_names_empty_when_runes_is_absent() {
        let ws = testkit::TempDir::new("spira-config-runes-names-absent");
        assert!(rune_names(&ws).is_empty());
    }

    #[test]
    fn rune_get_reads_a_var_and_falls_back_to_default() {
        let ws = testkit::TempDir::new("spira-config-runes-get");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        assert_eq!(rune_get(&ws, "builder", "RUNE_LABELS", ""), "spira,plan");
        assert_eq!(rune_get(&ws, "builder", "RUNE_MISSING", "fallback"), "fallback");
        assert_eq!(rune_get(&ws, "ghost", "RUNE_LABELS", "fallback"), "fallback");
    }

    #[test]
    fn rune_get_evaluates_shell_parameter_expansion() {
        // A rune file is a shell fragment, not a key=value format — this is exactly the
        // shape test-rune.sh exercises (SPIRA_SCOPE_LABEL referenced from the rune).
        let ws = testkit::TempDir::new("spira-config-runes-get-expansion");
        write_rune(&ws, "scoped", "RUNE_LABELS=\"${SPIRA_SCOPE_LABEL:-spira},plan\"\n");
        assert_eq!(rune_get(&ws, "scoped", "RUNE_LABELS", ""), "spira,plan");
    }

    #[test]
    fn every_real_rune_reads_as_the_bash_source_did() {
        let runes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/runes");
        let env: BTreeMap<String, String> = [
            ("SPIRA_SCOPE_LABEL", "scope"),
            ("SPIRA_CI_LABEL", "ci"),
            ("SPIRA_MENDER_DEADLINE_SECS", "77"),
        ]
        .into_iter()
        .chain(["BATCH_JUDGEMENT", "PLAN", "CZAR", "GROOMER", "MIMIR", "MENDER", "PILGRIM", "SKULD", "INCIDENT", "SPIKE", "WARDEN"].map(|n| (Box::leak(format!("SPIRA_{n}_LABEL").into_boxed_str()) as &str, "lbl")))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let lookup = |n: &str| env.get(n).cloned();
        let mut checked = 0;
        for e in std::fs::read_dir(&runes).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("rune") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let vars = crate::rune_file::parse(&p.to_string_lossy(), &text, &lookup).unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(r#". "$1"; for v in $(compgen -A variable RUNE_); do printf '%s=%s\0' "$v" "${!v}"; done"#)
                .arg("x")
                .arg(&p)
                .env_clear()
                .envs(&env)
                .output()
                .unwrap();
            let mut want: Vec<(String, String)> = String::from_utf8(out.stdout)
                .unwrap()
                .split('\0')
                .filter(|r| !r.is_empty())
                .map(|r| r.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap())
                .collect();
            want.sort();
            let mut got: Vec<(String, String)> = vars.into_iter().collect();
            got.sort();
            assert_eq!(got, want, "{}", p.display());
            checked += 1;
        }
        assert!(checked >= 10, "{checked}");
    }

    #[test]
    fn rune_get_spawns_no_process_for_a_plain_read_and_refuses_a_command_substitution() {
        let ws = testkit::TempDir::new("spira-config-runes-get-refuse");
        write_rune(&ws, "bad", "RUNE_LABELS=$(echo hi)\n");
        assert_eq!(rune_get(&ws, "bad", "RUNE_LABELS", "dflt"), "dflt");
    }

    #[test]
    fn spira_runes_env_override_wins_outright() {
        let ws = testkit::TempDir::new("spira-config-runes-roster-override");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        assert_eq!(spira_runes(&ws, Some("only-this")), "only-this");
    }

    #[test]
    fn spira_runes_sorts_elastic_last() {
        let ws = testkit::TempDir::new("spira-config-runes-roster-elastic");
        write_rune(&ws, "batcher", "RUNE_ELASTIC=1\n");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "ops", "RUNE_LABELS=ops\n");
        assert_eq!(spira_runes(&ws, None), "builder ops batcher");
    }

    #[test]
    fn spira_task_runes_excludes_party_and_lane_and_uncallable() {
        let ws = testkit::TempDir::new("spira-config-runes-task");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "human", "RUNE_CALL=manual\n");
        write_rune(&ws, "warden", "RUNE_LANE=groom\n");
        write_rune(&ws, "worker", "RUNE_ROLE=party\n");
        assert_eq!(spira_task_runes(&ws, None), "builder");
    }

    #[test]
    fn spira_lane_runes_is_the_declared_lane_rune_only() {
        let ws = testkit::TempDir::new("spira-config-runes-lane");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "warden", "RUNE_LANE=groom\n");
        write_rune(&ws, "human", "RUNE_CALL=manual\nRUNE_LANE=groom\n");
        assert_eq!(spira_lane_runes(&ws, None), "warden");
    }

    #[test]
    fn rune_partitions_dedupes_and_skips_unlabelled() {
        let ws = testkit::TempDir::new("spira-config-runes-partitions");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "czar", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "ops", "RUNE_LABELS=ops\nRUNE_EXCLUDE_LABELS=no-ops\n");
        write_rune(&ws, "human", "RUNE_CALL=manual\n");
        let got = rune_partitions(&ws, None);
        assert_eq!(
            got,
            vec![
                ("spira,plan".to_string(), "".to_string()),
                ("ops".to_string(), "no-ops".to_string()),
            ]
        );
    }

    #[test]
    fn runes_for_labels_reads_every_runes_rune_not_just_the_active_roster() {
        let ws = testkit::TempDir::new("spira-config-runes-for-labels");
        write_rune(&ws, "builder", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "shadow", "RUNE_LABELS=spira,plan\n");
        write_rune(&ws, "ops", "RUNE_LABELS=ops\n");
        assert_eq!(runes_for_labels(&ws, "spira,plan"), vec!["builder".to_string(), "shadow".to_string()]);
    }

    #[test]
    fn persona_model_from_doc_falls_back_with_no_document() {
        assert_eq!(persona_model_from_doc(None, "builder", "claude-opus-5"), "claude-opus-5");
        assert_eq!(persona_model_from_doc(None, "builder", "claude-x"), "claude-x");
    }

    #[test]
    fn persona_model_from_doc_falls_back_with_no_persona_table_or_empty_model() {
        let mut doc = SpiraToml::default();
        doc.persona.insert("builder".to_string(), crate::PersonaSection {
            model: "".to_string(),
            tools: vec![],
            labels: vec![],
            lane: None,
            lease: None,
            system_prompt: None,
            escalation_model: None,
        });
        assert_eq!(persona_model_from_doc(Some(&doc), "builder", "claude-opus-5"), "claude-opus-5");
        assert_eq!(persona_model_from_doc(Some(&doc), "ops", "claude-opus-5"), "claude-opus-5");
    }

    #[test]
    fn persona_model_from_doc_reads_the_configured_model() {
        let mut doc = SpiraToml::default();
        doc.persona.insert("builder".to_string(), crate::PersonaSection {
            model: "claude-sonnet-5".to_string(),
            tools: vec![],
            labels: vec![],
            lane: None,
            lease: None,
            system_prompt: None,
            escalation_model: None,
        });
        assert_eq!(persona_model_from_doc(Some(&doc), "builder", "claude-opus-5"), "claude-sonnet-5");
    }

    /// Wave 4.9 (sp-k80sa): `extract_label_overlay` must pick every one of
    /// `RUNE_LABEL_KEYS` out of a `Resolved` that carries it, and quietly drop any that
    /// is missing — mirroring a `conf.d` registry where not every label has a file (never
    /// an error: a `.rune` referencing an unresolved key just sees it absent). Pure: no
    /// process environment touched, so unlike `rune_label_overlay` itself (which calls
    /// `discover()` and so needs the crate-wide `testkit::env` serialization any REAL test of
    /// it would require — see `persona_model`/`persona_model_from_doc`'s identical split
    /// for why that half is exercised only by the end-to-end proof, not a unit test here).
    #[test]
    fn extract_label_overlay_picks_known_keys_and_drops_the_rest() {
        let mut resolved = crate::resolve::Resolved::default();
        resolved.values.insert("SPIRA_CZAR_LABEL".to_string(), "czar-trigger".to_string());
        resolved.values.insert("SPIRA_GROOMER_LABEL".to_string(), "groom".to_string());
        resolved.values.insert("SPIRA_HOME_REPO".to_string(), "brain".to_string());
        resolved.values.insert("SPIRA_SOME_OTHER_KEY".to_string(), "irrelevant".to_string());

        let overlay = extract_label_overlay(&resolved);

        assert_eq!(overlay.get("SPIRA_CZAR_LABEL").map(String::as_str), Some("czar-trigger"));
        assert_eq!(overlay.get("SPIRA_GROOMER_LABEL").map(String::as_str), Some("groom"));
        assert_eq!(overlay.get("SPIRA_HOME_REPO").map(String::as_str), Some("brain"));
        // sp-xsnid: the overlay is now the FULL resolved map, not a five-key slice — a
        // key that slice never carried (SPIRA_SOME_OTHER_KEY, standing in for
        // SPIRA_INCIDENT_LABEL/SPIRA_SPIKE_LABEL/SPIRA_PLAN_LABEL/SPIRA_SCOPE_LABEL/
        // SPIRA_ASK_LABEL/SPIRA_CI_LABEL, every one of which this bead found missing)
        // must pass through too, or the next rune variable repeats this bug.
        assert_eq!(overlay.get("SPIRA_SOME_OTHER_KEY").map(String::as_str), Some("irrelevant"));
    }

    #[test]
    fn bare_references_skips_guarded_names_but_keeps_required_ones() {
        // SPIRA_SCOPE_LABEL appears TWICE — once as the `:+` condition, once bare INSIDE
        // its own body — and must be exempt both times; SPIRA_INCIDENT_LABEL is bare and
        // required. This is ops.rune's own real shape.
        let refs = bare_references(r#"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL"#);
        assert_eq!(refs, vec!["SPIRA_INCIDENT_LABEL".to_string()]);
    }

    #[test]
    fn bare_references_is_empty_for_a_declared_literal() {
        // concierge.rune's own RUNE_LABELS="" — no `$` at all, nothing to require.
        assert_eq!(bare_references(""), Vec::<String>::new());
    }

    #[test]
    fn assigned_value_strips_quotes_and_takes_the_last_assignment() {
        let text = "RUNE_LABELS=\"first\"\nRUNE_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_X_LABEL\"\n";
        assert_eq!(assigned_value(text, "RUNE_LABELS").unwrap(), "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_X_LABEL");
        assert_eq!(assigned_value(text, "RUNE_EXCLUDE_LABELS"), None);
    }

    /// The sp-xsnid repro, pinned at the `rune_predicate` level rather than through a
    /// real resolved config (which would need `testkit::env`/`discover()` — see
    /// `rune_label_overlay`'s own split): a rune whose `RUNE_LABELS` references a
    /// variable this overlay never supplies refuses, naming the exact reference, rather
    /// than handing back an empty string a caller would read as "match everything".
    #[test]
    fn rune_predicate_refuses_rather_than_widen_on_an_unresolved_bare_reference() {
        let dir = testkit::TempDir::new("rune-predicate-refuse");
        write_rune(&dir, "ops", "RUNE_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL\"\nRUNE_EXCLUDE_LABELS=\"spira-poison,$SPIRA_ASK_LABEL\"\n");
        // No spira.toml under this fixture, and no real conf.d registry beside it either
        // — `rune_label_overlay` resolves to an EMPTY map (its own documented
        // "resolution failure" fallback), so every bare reference reads as unresolved.
        let err = rune_predicate(&dir, "ops").unwrap_err();
        assert_eq!(err.rune, "ops");
        assert_eq!(err.var, "RUNE_LABELS");
        assert_eq!(err.reference, "SPIRA_INCIDENT_LABEL");
        assert!(err.to_string().contains("SPIRA_INCIDENT_LABEL"));
    }

    /// test-spike.sh's own real shape: `SPIRA_HOME` is a scratch fixture with NO `conf.d`
    /// at all (only a `runes/` and a stub `lib.sh`), so [`rune_label_overlay`]'s own
    /// `resolve_for_process` call fails OUTRIGHT there — but the suite pins
    /// `SPIRA_SPIKE_LABEL=research` by a bare `export`, genuinely present in this
    /// process's own ambient environment. That failure must not be read as "the variable
    /// itself is unresolved": [`rune_predicate`] must fall back to the ambient value
    /// `rune_get`'s own bash subshell would actually inherit, not refuse.
    #[test]
    fn rune_predicate_falls_back_to_the_ambient_environment_when_in_process_resolution_fails() {
        let dir = testkit::TempDir::new("rune-predicate-ambient-fallback");
        write_rune(&dir, "spike", "RUNE_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_SPIKE_LABEL\"\nRUNE_EXCLUDE_LABELS=\"spira-poison\"\n");
        let env = testkit::env(&[("SPIRA_SPIKE_LABEL", Some("research"))]);
        let got = rune_predicate(&dir, "spike");
        drop(env);
        assert_eq!(got.unwrap().labels, "research");
    }

    #[test]
    fn rune_predicate_accepts_a_declared_literal_empty_with_no_reference() {
        let dir = testkit::TempDir::new("rune-predicate-literal");
        write_rune(&dir, "concierge", "RUNE_LABELS=\"\"\nRUNE_EXCLUDE_LABELS=\"\"\n");
        let p = rune_predicate(&dir, "concierge").unwrap();
        assert_eq!(p, Predicate { labels: String::new(), exclude_labels: String::new() });
    }

    #[test]
    fn rune_predicate_missing_file_is_empty_not_a_refusal() {
        let dir = testkit::TempDir::new("rune-predicate-missing");
        std::fs::create_dir_all(dir.join("runes")).unwrap();
        let p = rune_predicate(&dir, "ghost").unwrap();
        assert_eq!(p, Predicate::default());
    }
}
