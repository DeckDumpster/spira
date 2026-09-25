//! test-plan — the typed schema behind the use-case catalogue and its derived coverage
//! matrix.
//!
//! `docs/test-plan/<area>.toml` declares each area's use cases (id, tier, statement, and
//! optionally why it is uncovered). `deny_unknown_fields` on every struct here makes a typo
//! a hard error instead of a field nobody validates — the same reason `spira-config` carries
//! it. A suite's own coverage (its `# tier:`/`# covers:` header) is read into the same typed
//! model via [`SuiteCoverage`], built from JSON a shell script emits (`suite_tier_of`/
//! `suite_covers_of` stay the ONE parser for that header; this crate never reparses it).
//!
//! Three checks live here, not in prose: a suite naming an id no catalogue declares
//! ([`unknown_uc_violations`]), a use case a suite set used to cover and now nothing does
//! ([`orphan_violations`] — "deletion writes the plan"), and a suite whose measured runtime
//! exceeds its declared tier's budget ([`tier_budget_flags`]). [`build_matrix`] joins all of
//! it into one derived, deterministic document; [`render_markdown`] is the only thing allowed
//! to turn that into prose.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CATALOGUE_API_VERSION: &str = "test-plan/v1";
pub const MATRIX_API_VERSION: &str = "test-plan-matrix/v1";

/// The cheapest tier that would catch a regression in a use case's behaviour —
/// `docs/test-plan/README.md`'s own table. Spelled as the exact uppercase token every
/// suite header and catalogue line already uses, so no rename table can drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub enum Tier {
    T0,
    T1,
    T2,
    T3,
    T4,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::T0 => "T0",
            Tier::T1 => "T1",
            Tier::T2 => "T2",
            Tier::T3 => "T3",
            Tier::T4 => "T4",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "T0" => Some(Tier::T0),
            "T1" => Some(Tier::T1),
            "T2" => Some(Tier::T2),
            "T3" => Some(Tier::T3),
            "T4" => Some(Tier::T4),
            _ => None,
        }
    }

    /// A provisional, generous per-tier wall-time budget in milliseconds, so the matrix can
    /// flag an obvious mismeasurement of its own tier. This is independent of sp-5m133's own
    /// gate-enforced budget (which ratchets against today's violators); this one never fails
    /// anything, it only flags a row in the derived matrix.
    pub fn provisional_budget_ms(&self) -> f64 {
        match self {
            Tier::T0 => 2_000.0,
            Tier::T1 => 5_000.0,
            Tier::T2 => 30_000.0,
            Tier::T3 => 120_000.0,
            Tier::T4 => 600_000.0,
        }
    }
}

/// Why a T0–T3 use case has no covering suite right now. Every field is required: an
/// uncovered marker with no reason, date or bead is not a record, it is a suppression.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Uncovered {
    pub reason: String,
    pub date: String,
    pub bead: String,
}

/// One row of a catalogue: `UC-<area>-NN`, its tier, the behaviour it states, and — only
/// while nothing covers it — why.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseCase {
    pub id: String,
    pub tier: Tier,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncovered: Option<Uncovered>,
}

/// `docs/test-plan/<area>.toml` in full: one area's use cases, versioned so a future
/// breaking change to this shape is a version bump, not a silent reinterpretation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub api_version: String,
    pub area: String,
    #[serde(default)]
    pub use_case: Vec<UseCase>,
}

/// Parses TOML catalogue `text`, reporting the first error at its TOML path
/// (`use_case[3].tier`, ...) rather than a bare line/column — the same convention
/// `spira-config::validate` uses, for the same reason: a hard error should name the thing to
/// fix.
pub fn parse_catalogue(text: &str) -> Result<Catalogue, String> {
    let de = toml::Deserializer::new(text);
    serde_path_to_error::deserialize(de).map_err(|e| {
        let path = e.path().to_string();
        if path.is_empty() {
            e.inner().to_string()
        } else {
            format!("{path}: {}", e.inner())
        }
    })
}

/// One parsed catalogue file, kept beside the path it came from so every error below can
/// name it.
#[derive(Debug, Clone)]
pub struct LoadedCatalogue {
    pub path: String,
    pub catalogue: Catalogue,
}

/// Loads every `*.toml` in `dir`, refusing (a) a file that fails to parse, (b) a file whose
/// `area` does not match its own filename stem, and (c) a use-case id declared more than
/// once across the whole set — each error names its path. Order is deterministic (sorted by
/// filename) so a diff between two runs is never just directory-listing noise.
pub fn load_catalogues(dir: &Path) -> Result<Vec<LoadedCatalogue>, Vec<String>> {
    let mut errors = Vec::new();
    let mut paths: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "toml").unwrap_or(false))
            .collect(),
        Err(e) => {
            return Err(vec![format!("{}: {e}", dir.display())]);
        }
    };
    paths.sort();

    let mut loaded = Vec::new();
    let mut seen_ids: BTreeMap<String, String> = BTreeMap::new();
    for path in paths.drain(..) {
        let rel = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let catalogue = match parse_catalogue(&text) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if catalogue.area != stem {
            errors.push(format!(
                "{rel}: area {:?} does not match its filename ({stem:?}.toml)",
                catalogue.area
            ));
        }
        for uc in &catalogue.use_case {
            if let Some(prior) = seen_ids.insert(uc.id.clone(), rel.clone()) {
                errors.push(format!(
                    "{rel}: duplicate use-case id {} (also declared in {prior})",
                    uc.id
                ));
            }
        }
        loaded.push(LoadedCatalogue {
            path: rel,
            catalogue,
        });
    }

    if errors.is_empty() {
        Ok(loaded)
    } else {
        Err(errors)
    }
}

/// A suite's own declared coverage, as read off its `# tier:`/`# covers:` header by
/// `spira/suite-covers.sh` (the one parser) and handed to this crate as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteCoverage {
    pub path: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub covers: Vec<String>,
}

impl SuiteCoverage {
    /// The `UC-*` tokens on this suite's `# covers:` line — the same filter
    /// `suite_uc_of` applies in bash, kept in step because both read the prefix, not a
    /// second declaration.
    pub fn uc_ids(&self) -> impl Iterator<Item = &str> {
        self.covers.iter().filter_map(|c| {
            if c.starts_with("UC-") {
                Some(c.as_str())
            } else {
                None
            }
        })
    }
}

fn all_uc_ids(catalogues: &[LoadedCatalogue]) -> BTreeSet<&str> {
    catalogues
        .iter()
        .flat_map(|lc| lc.catalogue.use_case.iter().map(|uc| uc.id.as_str()))
        .collect()
}

/// A suite's `# covers:` naming a UC id no catalogue declares — the id was mistyped, or the
/// catalogue entry was deleted out from under a live suite.
pub fn unknown_uc_violations(catalogues: &[LoadedCatalogue], suites: &[SuiteCoverage]) -> Vec<String> {
    let known = all_uc_ids(catalogues);
    let mut out = Vec::new();
    for s in suites {
        for id in s.uc_ids() {
            if !known.contains(id) {
                out.push(format!("{}: unknown UC id on # covers: {id}", s.path));
            }
        }
    }
    out
}

/// "Deletion writes the plan": a use case some suite covered in `prev` (the base ref) and no
/// suite covers in `cur` (the branch's tip) is orphaned, unless the current catalogue marks
/// it `uncovered`. A suite that adds a NEW cover for the id in the same commit is not
/// orphaned at all — it never leaves `cur`'s covered set, so it is never a candidate here.
pub fn orphan_violations(
    catalogues: &[LoadedCatalogue],
    prev: &[SuiteCoverage],
    cur: &[SuiteCoverage],
) -> Vec<String> {
    let covered_prev: BTreeSet<&str> = prev.iter().flat_map(|s| s.uc_ids()).collect();
    let covered_cur: BTreeSet<&str> = cur.iter().flat_map(|s| s.uc_ids()).collect();
    let uncovered_now: BTreeMap<&str, &Uncovered> = catalogues
        .iter()
        .flat_map(|lc| lc.catalogue.use_case.iter())
        .filter_map(|uc| uc.uncovered.as_ref().map(|u| (uc.id.as_str(), u)))
        .collect();

    let mut out = Vec::new();
    for id in covered_prev.difference(&covered_cur) {
        if !uncovered_now.contains_key(id) {
            out.push(format!(
                "{id}: lost its last covering suite with no [use_case.uncovered] marker and no new cover in this commit"
            ));
        }
    }
    out
}

/// A suite whose measured p50 (`timings`, keyed by suite path, milliseconds) exceeds its
/// declared tier's provisional budget. Reported, never failed — TIER HONESTY in the matrix.
pub fn tier_budget_flags(suites: &[SuiteCoverage], timings: &BTreeMap<String, f64>) -> Vec<String> {
    let mut out = Vec::new();
    for s in suites {
        let Some(tier) = s.tier.as_deref().and_then(Tier::parse) else {
            continue;
        };
        let Some(&p50) = timings.get(&s.path) else {
            continue;
        };
        let budget = tier.provisional_budget_ms();
        if p50 > budget {
            out.push(format!(
                "{}: measured p50 {p50:.0}ms exceeds {} provisional budget {budget:.0}ms",
                s.path,
                tier.as_str()
            ));
        }
    }
    out
}

/// One covering suite's row in the derived matrix.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SuiteRef {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<f64>,
    pub over_budget: bool,
}

/// One use case's row in the derived matrix: its statement and tier from the catalogue,
/// every suite that covers it, and its uncovered marker if it has one.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UseCaseMatrix {
    pub id: String,
    pub tier: Tier,
    pub statement: String,
    pub covering_suites: Vec<SuiteRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncovered: Option<Uncovered>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AreaMatrix {
    pub area: String,
    pub use_cases: Vec<UseCaseMatrix>,
}

/// The whole derived coverage matrix — `docs/test-plan/coverage.json` in full. Regenerated
/// whole from the catalogues, the suite headers and the timings; never hand-edited.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatrixDoc {
    pub api_version: String,
    pub areas: Vec<AreaMatrix>,
}

/// Builds the matrix deterministically: areas sorted by name, use cases by id, covering
/// suites by path — so two runs over the same inputs produce byte-identical JSON and a
/// `--check` diff only ever fires on a real change.
pub fn build_matrix(
    catalogues: &[LoadedCatalogue],
    suites: &[SuiteCoverage],
    timings: &BTreeMap<String, f64>,
) -> MatrixDoc {
    let mut by_uc: BTreeMap<&str, Vec<&SuiteCoverage>> = BTreeMap::new();
    for s in suites {
        for id in s.uc_ids() {
            by_uc.entry(id).or_default().push(s);
        }
    }

    let mut sorted: Vec<&LoadedCatalogue> = catalogues.iter().collect();
    sorted.sort_by(|a, b| a.catalogue.area.cmp(&b.catalogue.area));

    let mut areas = Vec::new();
    for lc in sorted {
        let mut ucs: Vec<&UseCase> = lc.catalogue.use_case.iter().collect();
        ucs.sort_by(|a, b| a.id.cmp(&b.id));
        let mut use_cases = Vec::new();
        for uc in ucs {
            let mut covering: Vec<&SuiteCoverage> =
                by_uc.get(uc.id.as_str()).cloned().unwrap_or_default();
            covering.sort_by(|a, b| a.path.cmp(&b.path));
            let covering_suites = covering
                .into_iter()
                .map(|s| {
                    let p50_ms = timings.get(&s.path).copied();
                    let over_budget = match (s.tier.as_deref().and_then(Tier::parse), p50_ms) {
                        (Some(t), Some(p)) => p > t.provisional_budget_ms(),
                        _ => false,
                    };
                    SuiteRef {
                        path: s.path.clone(),
                        tier: s.tier.clone(),
                        p50_ms,
                        over_budget,
                    }
                })
                .collect();
            use_cases.push(UseCaseMatrix {
                id: uc.id.clone(),
                tier: uc.tier,
                statement: uc.statement.clone(),
                covering_suites,
                uncovered: uc.uncovered.clone(),
            });
        }
        areas.push(AreaMatrix {
            area: lc.catalogue.area.clone(),
            use_cases,
        });
    }

    MatrixDoc {
        api_version: MATRIX_API_VERSION.to_string(),
        areas,
    }
}

/// Renders the matrix as markdown — `docs/test-plan/COVERAGE.md` in full. The ONLY function
/// allowed to turn the derived JSON into prose; nothing else may hand-edit that file.
pub fn render_markdown(doc: &MatrixDoc) -> String {
    let mut out = String::new();
    out.push_str("# Coverage matrix\n\n");
    out.push_str(
        "Generated from docs/test-plan/*.toml, every suite's # tier:/# covers: header, and \
         run/tsd/ suite timings. Regenerate with `spira/plan-matrix.sh`; never hand-edit — a \
         stale copy fails the gate.\n\n",
    );
    for area in &doc.areas {
        out.push_str(&format!("## {}\n\n", area.area));
        out.push_str("| UC | tier | statement | covering suites | status |\n");
        out.push_str("|---|---|---|---|---|\n");
        for uc in &area.use_cases {
            let suites = if uc.covering_suites.is_empty() {
                String::from("—")
            } else {
                uc.covering_suites
                    .iter()
                    .map(|s| {
                        let p50 = match s.p50_ms {
                            Some(p) if s.over_budget => format!(" ({p:.0}ms, OVER BUDGET)"),
                            Some(p) => format!(" ({p:.0}ms)"),
                            None => String::new(),
                        };
                        format!("{}{p50}", s.path)
                    })
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            let status = match &uc.uncovered {
                Some(u) => format!("uncovered: {} ({}, {})", u.reason, u.date, u.bead),
                None if uc.covering_suites.is_empty() => String::from("**GAP**"),
                None => String::from("covered"),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                uc.id,
                uc.tier.as_str(),
                uc.statement.replace('|', "\\|"),
                suites,
                status
            ));
        }
        out.push('\n');
    }
    out
}

pub fn catalogue_json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(Catalogue)
}

pub fn matrix_json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(MatrixDoc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lc(area: &str, toml_text: &str) -> LoadedCatalogue {
        LoadedCatalogue {
            path: format!("{area}.toml"),
            catalogue: parse_catalogue(toml_text).expect("valid fixture"),
        }
    }

    #[test]
    fn valid_minimal_catalogue() {
        let c = parse_catalogue(
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )
        .expect("valid");
        assert_eq!(c.use_case.len(), 1);
        assert_eq!(c.use_case[0].tier, Tier::T1);
    }

    #[test]
    fn unknown_field_names_its_path() {
        let err = parse_catalogue(
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\nbogus = 1\n",
        )
        .unwrap_err();
        assert!(err.starts_with("bogus"), "{err}");
    }

    #[test]
    fn bad_tier_names_its_path() {
        let err = parse_catalogue(
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T9\"\nstatement = \"x\"\n",
        )
        .unwrap_err();
        assert!(err.starts_with("use_case"), "{err}");
    }

    #[test]
    fn duplicate_id_across_catalogues_is_refused() {
        let a = lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        );
        let b = LoadedCatalogue {
            path: "other.toml".into(),
            catalogue: parse_catalogue(
                "api_version = \"test-plan/v1\"\narea = \"other\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"y\"\n",
            )
            .unwrap(),
        };
        // load_catalogues itself is exercised in the CLI-facing tests (it walks a
        // directory); the duplicate check's logic is unit-tested directly here via the
        // same loop load_catalogues runs.
        let mut seen = std::collections::BTreeMap::new();
        let mut dups = Vec::new();
        for c in [&a, &b] {
            for uc in &c.catalogue.use_case {
                if seen.insert(uc.id.clone(), c.path.clone()).is_some() {
                    dups.push(uc.id.clone());
                }
            }
        }
        assert_eq!(dups, vec!["UC-dispatch-01".to_string()]);
    }

    #[test]
    fn unknown_uc_on_suite_is_a_violation() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )];
        let suites = vec![SuiteCoverage {
            path: "test-foo.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-99".into()],
        }];
        let v = unknown_uc_violations(&cats, &suites);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("UC-dispatch-99"), "{}", v[0]);
    }

    #[test]
    fn known_uc_on_suite_is_clean() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )];
        let suites = vec![SuiteCoverage {
            path: "test-foo.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        assert!(unknown_uc_violations(&cats, &suites).is_empty());
    }

    #[test]
    fn deleting_the_last_cover_orphans_the_uc() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )];
        let prev = vec![SuiteCoverage {
            path: "test-foo.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        let cur: Vec<SuiteCoverage> = vec![];
        let v = orphan_violations(&cats, &prev, &cur);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("UC-dispatch-01"), "{}", v[0]);
    }

    #[test]
    fn marking_uncovered_clears_the_orphan() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n\n[use_case.uncovered]\nreason = \"suite retired\"\ndate = \"2026-09-25\"\nbead = \"sp-xxxxx\"\n",
        )];
        let prev = vec![SuiteCoverage {
            path: "test-foo.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        let cur: Vec<SuiteCoverage> = vec![];
        assert!(orphan_violations(&cats, &prev, &cur).is_empty());
    }

    #[test]
    fn a_new_cover_in_the_same_commit_clears_the_orphan() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )];
        let prev = vec![SuiteCoverage {
            path: "test-old.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        let cur = vec![SuiteCoverage {
            path: "test-new.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        assert!(orphan_violations(&cats, &prev, &cur).is_empty());
    }

    #[test]
    fn tier_budget_flags_only_over_budget_suites() {
        let suites = vec![
            SuiteCoverage {
                path: "test-fast.sh".into(),
                tier: Some("T1".into()),
                covers: vec![],
            },
            SuiteCoverage {
                path: "test-slow.sh".into(),
                tier: Some("T1".into()),
                covers: vec![],
            },
        ];
        let mut timings = std::collections::BTreeMap::new();
        timings.insert("test-fast.sh".to_string(), 100.0);
        timings.insert("test-slow.sh".to_string(), 999_999.0);
        let flags = tier_budget_flags(&suites, &timings);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].contains("test-slow.sh"), "{}", flags[0]);
    }

    #[test]
    fn matrix_is_deterministic_across_reorderings() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-02\"\ntier = \"T1\"\nstatement = \"b\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"a\"\n",
        )];
        let suites_a = vec![
            SuiteCoverage {
                path: "test-b.sh".into(),
                tier: Some("T1".into()),
                covers: vec!["UC-dispatch-01".into()],
            },
            SuiteCoverage {
                path: "test-a.sh".into(),
                tier: Some("T1".into()),
                covers: vec!["UC-dispatch-01".into()],
            },
        ];
        let mut suites_b = suites_a.clone();
        suites_b.reverse();
        let timings = std::collections::BTreeMap::new();
        let m_a = build_matrix(&cats, &suites_a, &timings);
        let m_b = build_matrix(&cats, &suites_b, &timings);
        assert_eq!(
            serde_json::to_string(&m_a).unwrap(),
            serde_json::to_string(&m_b).unwrap()
        );
        // and use cases come out sorted by id despite the source order being b, a
        assert_eq!(m_a.areas[0].use_cases[0].id, "UC-dispatch-01");
        assert_eq!(m_a.areas[0].use_cases[1].id, "UC-dispatch-02");
    }

    #[test]
    fn render_markdown_flags_gaps_and_over_budget() {
        let cats = vec![lc(
            "dispatch",
            "api_version = \"test-plan/v1\"\narea = \"dispatch\"\n\n[[use_case]]\nid = \"UC-dispatch-01\"\ntier = \"T1\"\nstatement = \"x\"\n",
        )];
        let suites = vec![SuiteCoverage {
            path: "test-slow.sh".into(),
            tier: Some("T1".into()),
            covers: vec!["UC-dispatch-01".into()],
        }];
        let mut timings = std::collections::BTreeMap::new();
        timings.insert("test-slow.sh".to_string(), 999_999.0);
        let doc = build_matrix(&cats, &suites, &timings);
        let md = render_markdown(&doc);
        assert!(md.contains("OVER BUDGET"), "{md}");

        let empty_suites: Vec<SuiteCoverage> = vec![];
        let doc2 = build_matrix(&cats, &empty_suites, &BTreeMap::new());
        let md2 = render_markdown(&doc2);
        assert!(md2.contains("**GAP**"), "{md2}");
    }
}
