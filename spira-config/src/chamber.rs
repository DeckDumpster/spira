//! The chamber / fayth registry — spira/lib.sh family D (wave 4 decomposition, row D;
//! sp-r5zd2). lib.sh's own `fayth_names`,
//! `spira_fayths` (and its `spira_task_fayths`/`spira_lane_fayths` splits), `fayth_get`,
//! `fayth_partitions`, `fayths_for_labels` and `persona_model` all move here; lib.sh keeps
//! one-line shims onto the `spira-config fayth ...` CLI this module backs (see `main.rs`),
//! so the 50-odd bash sourcers keep working unchanged.
//!
//! `fayth_get` is the one function here that is NOT a plain lookup: a `.fayth` file is an
//! arbitrary shell fragment (fixtures use `${SPIRA_SCOPE_LABEL:+...}` parameter expansion),
//! sourced in a subshell by the original bash. This bridges to the exact same mechanism —
//! `bash -c '. "$f"; eval ...'` — the `bead` crate's own copy already uses (its own
//! comment: "rather than writing a second, partial shell-fragment parser"), instead of
//! reimplementing a shell-fragment evaluator in Rust.
//!
//! `fayth_get`'s bash subshell needs every config key a `.fayth` file's `FAYTH_LABELS`/
//! `FAYTH_EXCLUDE_LABELS` references by parameter expansion — `SPIRA_CZAR_LABEL`,
//! `SPIRA_INCIDENT_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL`, and so on — in its OWN
//! environment, because an unset expansion is silently empty, not an error, and this
//! process's own ambient environment (a bare `spira-sentinel.service`, never conf.sh)
//! carries none of them. Wave 4.9 (sp-k80sa) retired lib.sh's own `export` of a five-key
//! slice of these (its one-line shim onto this module's `fayth_get`) on the strength of
//! [`fayth_label_overlay`] resolving THAT SLICE in-process instead — which is exactly the
//! defect sp-xsnid found: a hand-curated list drifts the moment a persona's predicate
//! grows a new variable, and it had, by five (`SPIRA_INCIDENT_LABEL`, `SPIRA_SPIKE_LABEL`,
//! `SPIRA_PLAN_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL`, `SPIRA_CI_LABEL` — every
//! real fayth's own label variable and both of `FAYTH_EXCLUDE_LABELS`'s, none of them in
//! the five-key list). [`fayth_label_overlay`] now hands the subshell EVERY key
//! [`crate::resolve::resolve_for_process`] resolved, not a slice of it, so no future fayth
//! variable can repeat that drift. [`fayth_predicate`] is the fail-closed half on top: a
//! `.fayth`'s own text may still reference a variable that is empty for a legitimate
//! reason (`SPIRA_SCOPE_LABEL`'s `${VAR:+...}` guard, present in every real fayth, is
//! exactly that — "no scope restriction configured" is this harness's own common case) or
//! for a genuine config gap, and only the fail-closed check below can tell those apart.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{discover, load, SpiraToml};

/// The pure half of [`fayth_label_overlay`]: every key an
/// already-[`crate::resolve::resolve_for_process`]d [`crate::resolve::Resolved`] carries,
/// as the subshell's overlay — sp-xsnid widened this from a hand-curated five-key slice
/// (`SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/`SPIRA_MAECHEN_LABEL`/
/// `SPIRA_BATCH_JUDGEMENT_LABEL`/`SPIRA_HOME_REPO`) to the FULL resolved map, because the
/// slice had already drifted: it never carried `SPIRA_INCIDENT_LABEL`, `SPIRA_SPIKE_LABEL`,
/// `SPIRA_PLAN_LABEL`, `SPIRA_SCOPE_LABEL`, `SPIRA_ASK_LABEL` or `SPIRA_CI_LABEL` — every
/// other real fayth's own label variable, and both of `FAYTH_EXCLUDE_LABELS`'s. Split out
/// so it is unit-testable without a `discover()` call touching this process's real
/// environment — the same reason `persona_model_from_doc` exists alongside `persona_model`
/// below, and the same hazard: a test that called the `discover`-touching half directly
/// would need `crate::ENV_LOCK` (test-only, sp-dh4fv/sp-mz7dn's crate-wide serialization
/// against another test's `std::env::set_var`), and holding that lock here while a caller
/// above is ALSO holding it to drive its own env mutation would deadlock (`std::sync::Mutex`
/// is not reentrant) — so this half takes no lock and touches no env at all.
fn extract_label_overlay(resolved: &crate::resolve::Resolved) -> BTreeMap<String, String> {
    resolved.values.clone()
}

/// Resolves the FULL config via [`crate::resolve::resolve_for_process`] (env > toml >
/// derived default — the same precedence `conf.sh` always applied), so [`fayth_get`] can
/// hand it to its bash subshell explicitly rather than relying on this process's own
/// ambient environment, which a bare unit (`spira-sentinel.service`: `SPIRA_RELEASE`/
/// `PATH` only) never carries any of it in. A resolution failure (no config document
/// resolves, or a parse error) yields an empty overlay: the bash subshell then falls back
/// to whatever it would have seen anyway, never a hard failure over a label — the EMPTY
/// half of that fallback is exactly what [`fayth_predicate`]'s own fail-closed check
/// exists to catch instead of letting it widen a partition silently.
fn fayth_label_overlay(home: &Path) -> BTreeMap<String, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let repo = crate::resolve::derive_home_repo(home, &env);
    crate::resolve::resolve_for_process(home, &repo, &env)
        .map(|r| extract_label_overlay(&r))
        .unwrap_or_default()
}

/// `<home>/chamber` — the one directory every function here resolves a fayth against.
pub fn chamber_dir(home: &Path) -> PathBuf {
    home.join("chamber")
}

/// `fayth_names` — every persona defined in the chamber, sorted (bash's own glob
/// expansion sorts lexicographically; matched here with a plain `Vec::sort`, same as the
/// `bead` crate's existing copy). Empty when the chamber directory is absent or holds no
/// `*.fayth` file — never an error, the same "nothing found" an unmatched bash glob
/// collapses to.
pub fn fayth_names(home: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(chamber_dir(home)) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("fayth") {
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

/// `fayth_get <fayth> <var> [default]` — one field of a fayth: `<home>/chamber/<fayth>.fayth`
/// sourced as bash in a subshell, then `$var` evaluated (falling back to `default`). A
/// missing fayth file, or any failure of the bridge itself, returns `default` — exactly
/// what the bash form prints (its own `return 1` on a missing file is a status nothing here
/// reads: every caller, bash and Rust alike, only ever consumes the value on stdout).
pub fn fayth_get(home: &Path, fayth: &str, var: &str, default: &str) -> String {
    let f = chamber_dir(home).join(format!("{fayth}.fayth"));
    if !f.is_file() {
        return default.to_string();
    }
    let script =
        r#"f="$1"; var="$2"; def="$3"; . "$f" 2>/dev/null; eval "printf '%s' \"\${$var:-\$def}\"""#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg("fayth_get")
        .arg(&f)
        .arg(var)
        .arg(default)
        .envs(fayth_label_overlay(home))
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => default.to_string(),
    }
}

/// A `.fayth`'s partition predicate, fully evaluated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Predicate {
    pub labels: String,
    pub exclude_labels: String,
}

/// Why [`fayth_predicate`] refused rather than handing back a predicate that would widen
/// a partition to the whole ready queue (sp-xsnid, law-a-control-that-cannot-check-must-
/// refuse). `var` is `FAYTH_LABELS` or `FAYTH_EXCLUDE_LABELS`; `reference` is the ONE bare
/// (unguarded) `$NAME` the fayth's own text uses that resolved empty — never every name in
/// the expression, so the message points at the exact config key to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub fayth: String,
    pub var: String,
    pub reference: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.fayth: {} references ${} unguarded, and it resolved empty — refusing rather than widening this partition to the whole ready queue",
            self.fayth, self.var, self.reference,
        )
    }
}

/// Every `NAME` `text` references via `${NAME:+...}` or `${NAME:-...}` — guarded forms
/// exist precisely so an unset/empty `NAME` is fine (`SPIRA_SCOPE_LABEL` is guarded in
/// every real `FAYTH_LABELS`: "no scope restriction configured" is this harness's own
/// common case, not a defect). A name guarded ANYWHERE in `text` is treated as optional
/// EVERYWHERE in it — simpler than tracking which brace a later bare occurrence sits
/// inside, and sufficient for every real `.fayth` file, which never mixes a guarded and a
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
/// later line wins — every real `.fayth` only assigns each var once, but a fixture or a
/// future file might not), surrounding double quotes stripped when the whole RHS is one
/// quoted string (every real `.fayth` writes it that way). `None` when `text` never
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

/// `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS`, fully resolved and fail-closed — THE shared
/// evaluator every ready-counting/claiming caller (`spira-claim`, sentinel's own
/// `express_ready_in_task_pool`, `bead --for`) goes through, replacing each one's own copy
/// (sp-xsnid: `bead`'s private `fayth_get` carried a THIRD, independently drifted
/// three-key overlay — `SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/`SPIRA_MAECHEN_LABEL`
/// only — on top of this module's own five-key one; two hand-curated lists, two different
/// sets of gaps).
///
/// Evaluates both vars via [`fayth_get`] (now handed the FULL resolved config — see
/// [`fayth_label_overlay`]'s own doc), then checks the fayth's OWN raw text: any name it
/// references UNGUARDED ([`bare_references`]) that resolved to `""` is a refusal, never a
/// silent "match everything". A fayth file that does not exist, or that writes a var as a
/// plain literal with no `$` at all (`concierge.fayth`'s own `FAYTH_LABELS=""` — a
/// deliberate "no restriction", not a config gap), never refuses: only a REFERENCE that
/// resolves empty does. Callers that need "no partition at all" to mean something
/// (`fayth_partitions`) still read that off the plain [`fayth_get`], unaffected by this
/// function.
pub fn fayth_predicate(home: &Path, fayth: &str) -> Result<Predicate, Refusal> {
    let path = chamber_dir(home).join(format!("{fayth}.fayth"));
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let overlay = fayth_label_overlay(home);
    for var in ["FAYTH_LABELS", "FAYTH_EXCLUDE_LABELS"] {
        let Some(raw) = assigned_value(&text, var) else { continue };
        for name in bare_references(&raw) {
            if effective_value(&overlay, &name).is_empty() {
                return Err(Refusal { fayth: fayth.to_string(), var: var.to_string(), reference: name });
            }
        }
    }
    Ok(Predicate {
        labels: fayth_get(home, fayth, "FAYTH_LABELS", ""),
        exclude_labels: fayth_get(home, fayth, "FAYTH_EXCLUDE_LABELS", ""),
    })
}

/// The value `fayth_get`'s own bash subshell actually sees for `name`: the overlay entry
/// if [`fayth_label_overlay`] resolved one — even an explicit empty string shadows ambient,
/// exactly as `Command::envs` behaves, since `fayth_get` sets the overlay on a `Command`
/// that otherwise inherits this process's full environment — else that AMBIENT
/// environment directly. In-process resolution is not the only source: a caller that
/// sourced the real `conf.sh` already has the right value genuinely exported (every real
/// suite; `test-spike.sh` pins `SPIRA_SPIKE_LABEL=research` by a bare `export`, under a
/// fixture `SPIRA_HOME` with no `conf.d` at all, so [`fayth_label_overlay`]'s own
/// `resolve_for_process` call fails outright there — that failure must not be read as "the
/// variable itself is unresolved" when the ambient shell plainly has it).
fn effective_value(overlay: &BTreeMap<String, String>, name: &str) -> String {
    overlay.get(name).cloned().unwrap_or_else(|| std::env::var(name).unwrap_or_default())
}

/// `spira_fayths` — the personas this harness runs, space separated, IN PRIORITY ORDER.
/// `env_override` is `$SPIRA_FAYTHS`, which still wins outright when non-empty (which
/// personas a HOST runs is deployment configuration). Otherwise: every fayth in the
/// chamber, non-elastic ones first in `fayth_names` order, elastic ones (`FAYTH_ELASTIC=1`)
/// last — the elastic persona that scales to fill the box must sort behind the on-call
/// ones, not ahead of them alphabetically.
///
/// Replicates the bash original byte for byte, INCLUDING a latent quirk: when there are no
/// fixed fayths but at least one elastic one, the result carries a leading space (the bash
/// `${elastic:+ }` is an unconditional literal, not derived from whether `fixed` is itself
/// empty). Harmless for every real caller — all of them `for f in $(spira_fayths)`, which
/// word-splits it away — but kept exactly so a byte-for-byte suite comparison still passes.
pub fn spira_fayths(home: &Path, env_override: Option<&str>) -> String {
    if let Some(v) = env_override {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let mut fixed = String::new();
    let mut elastic = String::new();
    for f in fayth_names(home) {
        if fayth_get(home, &f, "FAYTH_ELASTIC", "0") == "1" {
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

/// `spira_task_fayths` — the personas the sentinel's pool summons: everything from
/// [`spira_fayths`] that is not a party member (`FAYTH_ROLE=party`) and not a lane fayth
/// (`FAYTH_LANE` set), and that is summonable at all (`FAYTH_SUMMON=auto`, the default).
pub fn spira_task_fayths(home: &Path, env_override: Option<&str>) -> String {
    let roster = spira_fayths(home, env_override);
    let mut out = String::new();
    for f in roster.split_whitespace() {
        if fayth_get(home, f, "FAYTH_SUMMON", "auto") != "auto" {
            continue;
        }
        if fayth_get(home, f, "FAYTH_ROLE", "task") == "party" {
            continue;
        }
        if !fayth_get(home, f, "FAYTH_LANE", "").is_empty() {
            continue;
        }
        out.push(' ');
        out.push_str(f);
    }
    out.strip_prefix(' ').unwrap_or(&out).to_string()
}

/// `spira_lane_fayths` — the personas that belong to a declared lane (`FAYTH_LANE` set),
/// drawn from [`spira_fayths`] under the same `FAYTH_SUMMON=auto` gate `spira_task_fayths`
/// applies (the sentinel summons from both lists, so a persona excluded from one by
/// `FAYTH_SUMMON` must be excluded from the other too).
pub fn spira_lane_fayths(home: &Path, env_override: Option<&str>) -> String {
    let roster = spira_fayths(home, env_override);
    let mut out = String::new();
    for f in roster.split_whitespace() {
        if fayth_get(home, f, "FAYTH_SUMMON", "auto") != "auto" {
            continue;
        }
        if !fayth_get(home, f, "FAYTH_LANE", "").is_empty() {
            out.push(' ');
            out.push_str(f);
        }
    }
    out.strip_prefix(' ').unwrap_or(&out).to_string()
}

/// `fayth_partitions` — every partition this host watches: one `(labels, exclude_labels)`
/// pair per distinct non-empty `FAYTH_LABELS` across [`spira_fayths`], deduplicated (two
/// personas may legitimately share a partition) and in roster order. Empty when the
/// chamber is empty — callers must treat that as "nothing to watch", never fall back to a
/// hardcoded partition (law-absence-needs-a-positive-control).
pub fn fayth_partitions(home: &Path, env_override: Option<&str>) -> Vec<(String, String)> {
    let roster = spira_fayths(home, env_override);
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for f in roster.split_whitespace() {
        let labels = fayth_get(home, f, "FAYTH_LABELS", "");
        if labels.is_empty() || seen.iter().any(|s| s == &labels) {
            continue;
        }
        seen.push(labels.clone());
        let exclude = fayth_get(home, f, "FAYTH_EXCLUDE_LABELS", "");
        out.push((labels, exclude));
    }
    out
}

/// `fayths_for_labels <labels>` — personas whose `FAYTH_LABELS` IS exactly `labels`, drawn
/// from [`fayth_names`] (every fayth in the chamber, unlike [`spira_fayths`]'s callers:
/// the question here is "is anything working these beads at all", and `SPIRA_FAYTHS`
/// narrowing the active roster must not hide a fayth that could answer it).
pub fn fayths_for_labels(home: &Path, want: &str) -> Vec<String> {
    fayth_names(home)
        .into_iter()
        .filter(|f| fayth_get(home, f, "FAYTH_LABELS", "") == want)
        .collect()
}

/// `persona_model <fayth> [default]` — the model this persona launches under:
/// `persona.<fayth>.model` from the spira.toml this process's own [`discover`]/[`load`]
/// resolve, falling back to `default` (or `claude-opus-5`, lib.sh's own built-in) when no
/// document resolves, the document has no `[persona.<fayth>]` table, or its `model` is
/// empty. ALWAYS LOCATES FRESH, never a cached path — a long-lived process (cockpit, a
/// sweep) must see a fayth edited after it started, the same requirement lib.sh's own
/// comment carries. Mirrors the aeon crate's own `conf::persona_model` (sp-z134z,
/// sp-zs04v.4), which reads an already-resolved toml path instead of locating one itself.
pub fn persona_model(fayth: &str, default: Option<&str>) -> String {
    const BUILTIN_DEFAULT: &str = "claude-opus-5";
    let def = default.filter(|d| !d.is_empty()).unwrap_or(BUILTIN_DEFAULT);
    let doc = discover(None).and_then(|p| load(&p).ok());
    persona_model_from_doc(doc.as_ref(), fayth, def)
}

/// The pure lookup [`persona_model`] runs once it has a document (or none) in hand —
/// split out so it can be unit-tested without touching the process environment, which
/// [`discover`] reads and which every test in this crate's binary shares (see
/// `locate::tests::ENV_LOCK`'s own comment on why that hazard is real).
fn persona_model_from_doc(doc: Option<&SpiraToml>, fayth: &str, default: &str) -> String {
    doc.and_then(|d| d.persona.get(fayth))
        .map(|p| p.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_fayth(home: &Path, name: &str, body: &str) {
        let dir = chamber_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.fayth")), body).unwrap();
    }

    #[test]
    fn fayth_names_sorts_and_strips_extension() {
        let ws = testkit::TempDir::new("spira-config-chamber-names");
        write_fayth(&ws, "ops", "FAYTH_LABELS=ops\n");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        assert_eq!(fayth_names(&ws), vec!["builder".to_string(), "ops".to_string()]);
    }

    #[test]
    fn fayth_names_empty_when_chamber_is_absent() {
        let ws = testkit::TempDir::new("spira-config-chamber-names-absent");
        assert!(fayth_names(&ws).is_empty());
    }

    #[test]
    fn fayth_get_reads_a_var_and_falls_back_to_default() {
        let ws = testkit::TempDir::new("spira-config-chamber-get");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        assert_eq!(fayth_get(&ws, "builder", "FAYTH_LABELS", ""), "spira,plan");
        assert_eq!(fayth_get(&ws, "builder", "FAYTH_MISSING", "fallback"), "fallback");
        assert_eq!(fayth_get(&ws, "ghost", "FAYTH_LABELS", "fallback"), "fallback");
    }

    #[test]
    fn fayth_get_evaluates_shell_parameter_expansion() {
        // A fayth file is a shell fragment, not a key=value format — this is exactly the
        // shape test-fayth.sh exercises (SPIRA_SCOPE_LABEL referenced from the fayth).
        let ws = testkit::TempDir::new("spira-config-chamber-get-expansion");
        write_fayth(&ws, "scoped", "FAYTH_LABELS=\"${SPIRA_SCOPE_LABEL:-spira},plan\"\n");
        assert_eq!(fayth_get(&ws, "scoped", "FAYTH_LABELS", ""), "spira,plan");
    }

    #[test]
    fn spira_fayths_env_override_wins_outright() {
        let ws = testkit::TempDir::new("spira-config-chamber-roster-override");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        assert_eq!(spira_fayths(&ws, Some("only-this")), "only-this");
    }

    #[test]
    fn spira_fayths_sorts_elastic_last() {
        let ws = testkit::TempDir::new("spira-config-chamber-roster-elastic");
        write_fayth(&ws, "batcher", "FAYTH_ELASTIC=1\n");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "ops", "FAYTH_LABELS=ops\n");
        assert_eq!(spira_fayths(&ws, None), "builder ops batcher");
    }

    #[test]
    fn spira_task_fayths_excludes_party_and_lane_and_unsummonable() {
        let ws = testkit::TempDir::new("spira-config-chamber-task");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "human", "FAYTH_SUMMON=manual\n");
        write_fayth(&ws, "guardian", "FAYTH_LANE=groom\n");
        write_fayth(&ws, "worker", "FAYTH_ROLE=party\n");
        assert_eq!(spira_task_fayths(&ws, None), "builder");
    }

    #[test]
    fn spira_lane_fayths_is_the_declared_lane_fayth_only() {
        let ws = testkit::TempDir::new("spira-config-chamber-lane");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "guardian", "FAYTH_LANE=groom\n");
        write_fayth(&ws, "human", "FAYTH_SUMMON=manual\nFAYTH_LANE=groom\n");
        assert_eq!(spira_lane_fayths(&ws, None), "guardian");
    }

    #[test]
    fn fayth_partitions_dedupes_and_skips_unlabelled() {
        let ws = testkit::TempDir::new("spira-config-chamber-partitions");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "czar", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "ops", "FAYTH_LABELS=ops\nFAYTH_EXCLUDE_LABELS=no-ops\n");
        write_fayth(&ws, "human", "FAYTH_SUMMON=manual\n");
        let got = fayth_partitions(&ws, None);
        assert_eq!(
            got,
            vec![
                ("spira,plan".to_string(), "".to_string()),
                ("ops".to_string(), "no-ops".to_string()),
            ]
        );
    }

    #[test]
    fn fayths_for_labels_reads_every_chamber_fayth_not_just_the_active_roster() {
        let ws = testkit::TempDir::new("spira-config-chamber-for-labels");
        write_fayth(&ws, "builder", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "shadow", "FAYTH_LABELS=spira,plan\n");
        write_fayth(&ws, "ops", "FAYTH_LABELS=ops\n");
        assert_eq!(fayths_for_labels(&ws, "spira,plan"), vec!["builder".to_string(), "shadow".to_string()]);
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
        });
        assert_eq!(persona_model_from_doc(Some(&doc), "builder", "claude-opus-5"), "claude-sonnet-5");
    }

    /// Wave 4.9 (sp-k80sa): `extract_label_overlay` must pick every one of
    /// `FAYTH_LABEL_KEYS` out of a `Resolved` that carries it, and quietly drop any that
    /// is missing — mirroring a `conf.d` registry where not every label has a file (never
    /// an error: a `.fayth` referencing an unresolved key just sees it absent). Pure: no
    /// process environment touched, so unlike `fayth_label_overlay` itself (which calls
    /// `discover()` and so needs the crate-wide `ENV_LOCK` serialization any REAL test of
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
        // must pass through too, or the next fayth variable repeats this bug.
        assert_eq!(overlay.get("SPIRA_SOME_OTHER_KEY").map(String::as_str), Some("irrelevant"));
    }

    #[test]
    fn bare_references_skips_guarded_names_but_keeps_required_ones() {
        // SPIRA_SCOPE_LABEL appears TWICE — once as the `:+` condition, once bare INSIDE
        // its own body — and must be exempt both times; SPIRA_INCIDENT_LABEL is bare and
        // required. This is ops.fayth's own real shape.
        let refs = bare_references(r#"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL"#);
        assert_eq!(refs, vec!["SPIRA_INCIDENT_LABEL".to_string()]);
    }

    #[test]
    fn bare_references_is_empty_for_a_declared_literal() {
        // concierge.fayth's own FAYTH_LABELS="" — no `$` at all, nothing to require.
        assert_eq!(bare_references(""), Vec::<String>::new());
    }

    #[test]
    fn assigned_value_strips_quotes_and_takes_the_last_assignment() {
        let text = "FAYTH_LABELS=\"first\"\nFAYTH_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_X_LABEL\"\n";
        assert_eq!(assigned_value(text, "FAYTH_LABELS").unwrap(), "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_X_LABEL");
        assert_eq!(assigned_value(text, "FAYTH_EXCLUDE_LABELS"), None);
    }

    /// The sp-xsnid repro, pinned at the `fayth_predicate` level rather than through a
    /// real resolved config (which would need `ENV_LOCK`/`discover()` — see
    /// `fayth_label_overlay`'s own split): a fayth whose `FAYTH_LABELS` references a
    /// variable this overlay never supplies refuses, naming the exact reference, rather
    /// than handing back an empty string a caller would read as "match everything".
    #[test]
    fn fayth_predicate_refuses_rather_than_widen_on_an_unresolved_bare_reference() {
        let dir = testkit::TempDir::new("fayth-predicate-refuse");
        write_fayth(&dir, "ops", "FAYTH_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL\"\nFAYTH_EXCLUDE_LABELS=\"spira-poison,$SPIRA_ASK_LABEL\"\n");
        // No spira.toml under this fixture, and no real conf.d registry beside it either
        // — `fayth_label_overlay` resolves to an EMPTY map (its own documented
        // "resolution failure" fallback), so every bare reference reads as unresolved.
        let err = fayth_predicate(&dir, "ops").unwrap_err();
        assert_eq!(err.fayth, "ops");
        assert_eq!(err.var, "FAYTH_LABELS");
        assert_eq!(err.reference, "SPIRA_INCIDENT_LABEL");
        assert!(err.to_string().contains("SPIRA_INCIDENT_LABEL"));
    }

    /// test-spike.sh's own real shape: `SPIRA_HOME` is a scratch fixture with NO `conf.d`
    /// at all (only a `chamber/` and a stub `lib.sh`), so [`fayth_label_overlay`]'s own
    /// `resolve_for_process` call fails OUTRIGHT there — but the suite pins
    /// `SPIRA_SPIKE_LABEL=research` by a bare `export`, genuinely present in this
    /// process's own ambient environment. That failure must not be read as "the variable
    /// itself is unresolved": [`fayth_predicate`] must fall back to the ambient value
    /// `fayth_get`'s own bash subshell would actually inherit, not refuse.
    #[test]
    fn fayth_predicate_falls_back_to_the_ambient_environment_when_in_process_resolution_fails() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = testkit::TempDir::new("fayth-predicate-ambient-fallback");
        write_fayth(&dir, "spike", "FAYTH_LABELS=\"${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_SPIKE_LABEL\"\nFAYTH_EXCLUDE_LABELS=\"spira-poison\"\n");
        let saved = std::env::var("SPIRA_SPIKE_LABEL").ok();
        std::env::set_var("SPIRA_SPIKE_LABEL", "research");
        let got = fayth_predicate(&dir, "spike");
        match saved {
            Some(v) => std::env::set_var("SPIRA_SPIKE_LABEL", v),
            None => std::env::remove_var("SPIRA_SPIKE_LABEL"),
        }
        assert_eq!(got.unwrap().labels, "research");
    }

    #[test]
    fn fayth_predicate_accepts_a_declared_literal_empty_with_no_reference() {
        let dir = testkit::TempDir::new("fayth-predicate-literal");
        write_fayth(&dir, "concierge", "FAYTH_LABELS=\"\"\nFAYTH_EXCLUDE_LABELS=\"\"\n");
        let p = fayth_predicate(&dir, "concierge").unwrap();
        assert_eq!(p, Predicate { labels: String::new(), exclude_labels: String::new() });
    }

    #[test]
    fn fayth_predicate_missing_file_is_empty_not_a_refusal() {
        let dir = testkit::TempDir::new("fayth-predicate-missing");
        std::fs::create_dir_all(dir.join("chamber")).unwrap();
        let p = fayth_predicate(&dir, "ghost").unwrap();
        assert_eq!(p, Predicate::default());
    }
}
