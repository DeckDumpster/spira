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
//! `fayth_get`'s bash subshell needs `SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/
//! `SPIRA_MAECHEN_LABEL`/`SPIRA_BATCH_JUDGEMENT_LABEL`/`SPIRA_HOME_REPO` in its OWN
//! environment: `czar.fayth`/`groomer.fayth`/`maechen.fayth` each write their
//! `FAYTH_LABELS` as `${SPIRA_SCOPE_LABEL:+...}$SPIRA_CZAR_LABEL` (etc.) parameter
//! expansion, and an unset expansion is silently empty, not an error — a persona's
//! partition label reads as empty rather than failing loudly. Wave 4.9 (sp-k80sa) retired
//! lib.sh's own `export` of these (its one-line shim onto this module's `fayth_get`), on
//! the strength of [`fayth_label_overlay`] resolving them in-process and setting them
//! explicitly on the subshell's `Command`, rather than depending on whatever this
//! process's OWN environment happened to inherit — the scar this closes (round 151): a
//! caller that did not itself export these left the roster silently empty.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{discover, load, SpiraToml};

/// The fayth-label keys a `.fayth` file's `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS` may
/// reference by parameter expansion — exactly the set `spira/lib.sh` used to `export`
/// before wave 4.9 (sp-k80sa) retired that line in favour of this in-process resolution.
const FAYTH_LABEL_KEYS: [&str; 5] = [
    "SPIRA_CZAR_LABEL",
    "SPIRA_GROOMER_LABEL",
    "SPIRA_MAECHEN_LABEL",
    "SPIRA_BATCH_JUDGEMENT_LABEL",
    "SPIRA_HOME_REPO",
];

/// The pure half of [`fayth_label_overlay`]: pick [`FAYTH_LABEL_KEYS`] out of an
/// already-[`crate::resolve::resolve_for_process`]d [`crate::resolve::Resolved`]. Split out
/// so it is unit-testable without a `discover()` call touching this process's real
/// environment — the same reason `persona_model_from_doc` exists alongside `persona_model`
/// below, and the same hazard: a test that called the `discover`-touching half directly
/// would need `crate::ENV_LOCK` (test-only, sp-dh4fv/sp-mz7dn's crate-wide serialization
/// against another test's `std::env::set_var`), and holding that lock here while a caller
/// above is ALSO holding it to drive its own env mutation would deadlock (`std::sync::Mutex`
/// is not reentrant) — so this half takes no lock and touches no env at all.
fn extract_label_overlay(resolved: &crate::resolve::Resolved) -> BTreeMap<String, String> {
    FAYTH_LABEL_KEYS
        .iter()
        .filter_map(|k| resolved.values.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect()
}

/// Resolves [`FAYTH_LABEL_KEYS`] via [`crate::resolve::resolve_for_process`] (env > toml >
/// derived default — the same precedence `conf.sh` always applied), so [`fayth_get`] can
/// hand them to its bash subshell explicitly rather than relying on this process's own
/// ambient environment. A resolution failure (no config document resolves, or a parse
/// error) yields an empty overlay: the bash subshell then falls back to whatever it would
/// have seen anyway, never a hard failure over a label.
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
        assert!(!overlay.contains_key("SPIRA_MAECHEN_LABEL"), "a key absent from Resolved must stay absent, not default to empty");
        assert!(!overlay.contains_key("SPIRA_BATCH_JUDGEMENT_LABEL"));
        assert!(!overlay.contains_key("SPIRA_SOME_OTHER_KEY"), "only FAYTH_LABEL_KEYS may pass through");
    }
}
