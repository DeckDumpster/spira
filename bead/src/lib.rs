//! Pure logic behind the `bead` binary. Contract: DESIGN.md. Kept separate from `main.rs`
//! (which does the `bdq`/`.fayth`/`schema.sh`/`mail` subprocess work) so the CLI's own
//! rules — the lane-admission guard, label composition, the lint judge — are unit-testable
//! with no database, no chamber and no subprocess.
//!
//! `bdq` (the module, not the `bead` binary's own bridge to it) is `bdq` the standalone
//! binary's own pure logic — see `src/bin/bdq.rs` and `bdq.rs`'s own module doc.

pub mod bdq;

use std::collections::BTreeMap;

use spira_config::convert::{repo_sections, ConvertWarnings};
use spira_config::{Lane, RepoSection};

// ---------------------------------------------------------------------------------------
// Repositories: read through `spira-config`'s own converter (never a hand-rolled parser
// here — config-fence reserves that file's format to spira-config; see DESIGN.md
// "Decisions"). `repos_by_name` is this crate's one entry point onto it.
// ---------------------------------------------------------------------------------------

/// Every `[repo.<name>]` the map's text converts to, keyed by name. A row whose `lanes`
/// column names an unrecognised mode or lane token makes the whole map unusable — every
/// `--repo` then reads as unmapped, which is the fail-closed reading (a map spira-config
/// itself refuses to trust is not a map this crate should trust a subset of either).
pub fn repos_by_name(map_text: &str) -> BTreeMap<String, RepoSection> {
    let mut warnings = ConvertWarnings::default();
    repo_sections(map_text, &mut warnings).unwrap_or_default()
}

/// A raw `FAYTH_LABELS`/lane token, read into the typed vocabulary — `None` when it names
/// no lane at all (an ordinary label like `testscope` or `alpha-work`).
pub fn label_to_lane(label: &str) -> Option<Lane> {
    serde_json::from_value(serde_json::Value::String(label.to_string())).ok()
}

/// The configured spelling of a lane (`Lane::MaechenSweep` -> `"maechen-sweep"`) — the
/// inverse of [`label_to_lane`], read from the same typed source rather than a literal
/// copied here (which `literal-lint` refuses outside `schema.sh`/`conf.sh`).
pub fn lane_label(lane: Lane) -> String {
    serde_json::to_value(lane)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The partition a persona's comma-joined `FAYTH_LABELS` carries, per `_bead_file`'s own
/// scan: the LAST label (in order) that matches the lane vocabulary wins — not the first,
/// matching the bash's un-`break`-ed loop.
pub fn fayth_partition(fayth_labels: &str) -> Option<Lane> {
    let mut partition = None;
    for raw in fayth_labels.split(',') {
        let l = raw.trim();
        if let Some(lane) = label_to_lane(l) {
            partition = Some(lane);
        }
    }
    partition
}

/// One outcome of the lane-admission guard (`law-a-lane-runs-only-where-the-repo-admits-it`).
pub enum LaneCheck {
    /// Nothing to check (no configured lane vocabulary partition in this persona's labels,
    /// or the override is set) — filing proceeds.
    Admitted,
    /// Refused: the message is `_bead_file`'s own refusal text, ready to print to stderr.
    Refused(String),
}

/// `_bead_file`'s lane-admission guard. `repos` is [`repos_by_name`]'s result; `fayth_labels`
/// is the filing persona's raw `FAYTH_LABELS`. `override_set` is `SPIRA_BEAD_LANE_OVERRIDE`
/// being non-empty, which admits everything unconditionally.
pub fn lane_check(
    fayth_labels: &str,
    repo: &str,
    repos: &BTreeMap<String, RepoSection>,
    override_set: bool,
) -> LaneCheck {
    if override_set {
        return LaneCheck::Admitted;
    }
    let partition = match fayth_partition(fayth_labels) {
        Some(p) => p,
        None => return LaneCheck::Admitted,
    };
    let admitted: &[Lane] = repos.get(repo).map(|r| r.lanes.as_slice()).unwrap_or(&[]);
    if admitted.contains(&partition) {
        return LaneCheck::Admitted;
    }
    let names: Vec<String> = admitted.iter().copied().map(lane_label).collect();
    let refuser = if names.is_empty() {
        "no admitted lanes".to_string()
    } else {
        format!("its admitted lanes ({})", names.join(","))
    };
    LaneCheck::Refused(format!(
        "bead: repo:{repo} does not admit lane {} — refused by {refuser}\nbead: override: SPIRA_BEAD_LANE_OVERRIDE=1",
        lane_label(partition)
    ))
}

// ---------------------------------------------------------------------------------------
// `file`'s label composition — the comma-joined `-l` value `bd create` gets, for both the
// `--for` (work) and `--kind` (non-work) routes.
// ---------------------------------------------------------------------------------------

/// The `-l` label list for a `work`-kind filing: the persona's own `FAYTH_LABELS`, plus
/// `repo:<repo>`, plus the express label when asked.
pub fn work_labels(fayth_labels: &str, repo: &str, express_label: &str, express: bool) -> String {
    let mut out = format!("{fayth_labels},repo:{repo}");
    if express {
        out.push(',');
        out.push_str(express_label);
    }
    out
}

/// The `-l` label list for a non-`work` kind: the scope label, `insight` for the `insight`
/// kind, `repo:<repo>` when a repo was given, and the express label when asked.
pub fn non_work_labels(
    scope_label: &str,
    insight_label: Option<&str>,
    repo: Option<&str>,
    express_label: &str,
    express: bool,
) -> String {
    let mut out = scope_label.to_string();
    if let Some(ins) = insight_label {
        out.push(',');
        out.push_str(ins);
    }
    if let Some(r) = repo {
        out.push_str(",repo:");
        out.push_str(r);
    }
    if express {
        out.push(',');
        out.push_str(express_label);
    }
    out
}

// ---------------------------------------------------------------------------------------
// `lint` — the pure judge (`_bead_lint_judge`) plus the label-set helpers the real-data
// sweep (in main.rs) uses against `bd show`/`dep list` JSON.
// ---------------------------------------------------------------------------------------

/// Work-query types: excludes `event`/`escalation`/`proposal`/`gate` by construction,
/// matching `schema.sh`'s `SCHEMA_WORK_TYPES` plus `epic` (kept for the repo: check only).
const REPO_CHECK_TYPES: &[&str] = &["task", "bug", "feature", "epic", "chore", "spike"];
const PARTITION_CHECK_TYPES: &[&str] = &["task", "bug", "feature", "chore", "spike"];

fn has_token(space_joined_labels: &str, token: &str) -> bool {
    space_joined_labels.split_whitespace().any(|l| l == token)
}

fn has_label_starting_with(space_joined_labels: &str, prefix: &str) -> bool {
    space_joined_labels
        .split_whitespace()
        .any(|l| l.starts_with(prefix))
}

/// `_bead_lint_judge <labels> <status> <type> <partitions>` — pure, no `bd`, no I/O. Each
/// defect is one line of `out`; `count` is how many were found. `partitions` is
/// space-separated, matching the bash's own calling convention.
pub fn lint_judge(
    labels: &str,
    status: &str,
    ty: &str,
    partitions: &str,
    no_loop_label: &str,
) -> (u32, Vec<String>) {
    let mut out = Vec::new();

    if REPO_CHECK_TYPES.contains(&ty) && !has_label_starting_with(labels, "repo:") {
        out.push("no repo: label".to_string());
    }

    if status == "open" && PARTITION_CHECK_TYPES.contains(&ty) {
        let has_no_loop = !no_loop_label.is_empty() && has_token(labels, no_loop_label);
        if !has_no_loop {
            let has_partition = partitions.split_whitespace().any(|p| has_token(labels, p));
            if !has_partition {
                if no_loop_label.is_empty() {
                    out.push("no partition label".to_string());
                } else {
                    out.push(format!(
                        "no partition label; add one or mark {no_loop_label}"
                    ));
                }
            }
        }
    }

    (out.len() as u32, out)
}

/// The partition vocabulary `_bead_lint` itself computes from the chamber: every fayth's
/// `FAYTH_LABELS`, comma-split, trimmed, de-duplicated, excluding the empty string and
/// (when non-empty) `scope_label`. `personas` is `(name, raw FAYTH_LABELS)` pairs, already
/// read (via the `.fayth` bridge, in main.rs — this function is the pure part).
pub fn chamber_partitions(personas: &[(String, String)], scope_label: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for (_, raw) in personas {
        for tok in raw.split(',') {
            let l = tok.trim();
            if l.is_empty() || l == scope_label {
                continue;
            }
            if !seen.iter().any(|s: &String| s == l) {
                seen.push(l.to_string());
            }
        }
    }
    seen
}

/// Parses `bd list --all --limit 0 --json`'s payload into the ids `_bead_lint --all`
/// sweeps: every issue whose `issue_type` is not `event`, in the store's own order. Any
/// parse failure reads as "no ids" (matching the bash's `2>/dev/null || true` fallback)
/// rather than a hard error — the list call itself already failed silently in that case.
pub fn parse_list_ids(json: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter(|d| d.get("issue_type").and_then(|t| t.as_str()) != Some("event"))
        .filter_map(|d| d.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect()
}

/// A bead's `bd show --json` row, the fields `_bead_lint`'s sweep reads.
#[derive(Debug, Clone, Default)]
pub struct ShowRow {
    pub labels: Vec<String>,
    pub status: String,
    pub issue_type: String,
}

/// Parses one `bd show --json` payload — an object, or a one-element array of one (both
/// shapes appear depending on how many ids were asked for).
pub fn parse_show_row(json: &str) -> Option<ShowRow> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = match &v {
        serde_json::Value::Array(a) => a.first()?,
        other => other,
    };
    let labels = obj
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let status = obj
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let issue_type = obj
        .get("issue_type")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Some(ShowRow {
        labels,
        status,
        issue_type,
    })
}

/// Parses one `bd dep list <id> --type blocks --json` payload into the list of
/// `depends_on_id`s, reading whichever key the store actually used (`depends_on_id` for a
/// batch shape, `id` for a single-id shape — `_bead_lint`'s own comment on why both exist).
pub fn parse_blocks_targets(json: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|d| {
            d.get("depends_on_id")
                .and_then(|x| x.as_str())
                .or_else(|| d.get("id").and_then(|x| x.as_str()))
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// The `branch:` label a bead carries, if any (first one found, matching the bash's
/// `break` on first match).
pub fn branch_label(labels: &[String]) -> Option<&str> {
    labels.iter().find_map(|l| l.strip_prefix("branch:"))
}

/// `branch:<candidate>` with a leading `spira/` stripped, the same normalisation
/// `_bead_lint` applies before comparing it to the bead's own id.
pub fn branch_candidate(raw: &str) -> &str {
    raw.strip_prefix("spira/").unwrap_or(raw)
}

// ---------------------------------------------------------------------------------------
// `dep add` — the incident-blocks refusal.
// ---------------------------------------------------------------------------------------

/// The three `dep add` type spellings that produce a real `blocks` edge — every one bd
/// accepts as an alias (`bd dep add --help`), and so every one this refusal must catch.
pub fn is_blocks_type(dep_type: Option<&str>) -> bool {
    matches!(
        dep_type.unwrap_or("blocks"),
        "blocks" | "blocked-by" | "depends-on"
    )
}

/// `_bead_dep_add`'s refusal text when `depid` carries the incident label and `dep_type`
/// would wire a `blocks` edge onto it.
pub fn incident_blocks_refusal(id: &str, depid: &str, incident_label: &str) -> String {
    format!(
        "bead: dep add: refusing — {depid} carries the {incident_label} label and can never finish (an alarm re-arms on every recurrence); a blocks edge onto it has no completion path. Use: bd dep relate {id} {depid}"
    )
}

// ---------------------------------------------------------------------------------------
// `contract` — formatting only; the three sections' data comes from main.rs's subprocess
// calls (`fayth_names`/`fayth_get`, `schema.sh kinds`, `repos_by_name`).
// ---------------------------------------------------------------------------------------

/// One `PERSONAS` line: `  {name:<14} {labels-or-"(no labels)"}`.
pub fn persona_line(name: &str, labels: &str) -> String {
    let shown = if labels.is_empty() {
        "(no labels)"
    } else {
        labels
    };
    format!("  {name:<14} {shown}")
}

/// One `REPOS` line per name, or the map's absence.
pub fn repos_section(repos: &BTreeMap<String, RepoSection>) -> String {
    if repos.is_empty() {
        return "  (no repository map)\n".to_string();
    }
    let mut out = String::new();
    for name in repos.keys() {
        out.push_str(&format!("  {name}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- repositories (spira-config's own converter — see DESIGN.md "Decisions") --------

    // literal-ok: test fixture mirroring the lane vocabulary's shipped spelling
    const MAECHEN: &str = "maechen-sweep";
    // literal-ok: test fixture mirroring schema.sh's shipped default for `no_loop`
    const NO_LOOP: &str = "no-loop";

    #[test]
    fn repos_by_name_reads_names_and_typed_lanes() {
        let map = "# comment row, must not appear as a repo\ntestrepo  | /tmp/test | push | origin/main | | true | plan\ndev-repo  | /tmp/dev  | push | origin/main | | true | develop\nself-repo | /tmp/self | push | origin/main | | true | self\n";
        let repos = repos_by_name(map);
        assert_eq!(
            repos.keys().cloned().collect::<Vec<_>>(),
            vec!["dev-repo", "self-repo", "testrepo"]
        );
        assert_eq!(repos["testrepo"].lanes, vec![Lane::Plan]);
        assert_eq!(
            repos["dev-repo"].lanes,
            vec![Lane::Plan, Lane::Incident, Lane::Groom, Lane::Spike]
        );
        assert_eq!(
            repos["self-repo"].lanes,
            vec![
                Lane::Plan,
                Lane::Incident,
                Lane::Groom,
                Lane::Spike,
                Lane::MaechenSweep,
                Lane::CzarTrigger
            ]
        );
    }

    #[test]
    fn repos_by_name_empty_on_no_map() {
        assert!(repos_by_name("").is_empty());
    }

    #[test]
    fn repos_by_name_empty_map_when_any_row_names_an_unknown_lane() {
        // Fail closed (DESIGN.md "Observed but preserved"): one bad row makes every repo
        // read as unmapped, rather than only the row that named the bad lane.
        let map = "good | /tmp/g | push | origin/main | | true | plan\nbad  | /tmp/b | push | origin/main | | true | not-a-lane\n";
        assert!(repos_by_name(map).is_empty());
    }

    #[test]
    fn label_to_lane_round_trips_every_shipped_spelling() {
        for (label, lane) in [
            ("plan", Lane::Plan),
            ("incident", Lane::Incident),
            ("groom", Lane::Groom),
            (MAECHEN, Lane::MaechenSweep),
            ("spike", Lane::Spike),
            ("czar-trigger", Lane::CzarTrigger),
        ] {
            assert_eq!(label_to_lane(label), Some(lane));
            assert_eq!(lane_label(lane), label);
        }
        assert_eq!(label_to_lane("alpha-work"), None);
    }

    #[test]
    fn fayth_partition_last_match_wins() {
        assert_eq!(fayth_partition("testscope,plan"), Some(Lane::Plan));
        assert_eq!(fayth_partition("plan,incident"), Some(Lane::Incident));
        assert_eq!(fayth_partition("testscope,alpha-work"), None);
        assert_eq!(fayth_partition(""), None);
    }

    fn repo_map_fixture() -> BTreeMap<String, RepoSection> {
        repos_by_name(
            "dev-repo  | /tmp/dev  | push | origin/main | | true | develop\nself-repo | /tmp/self | push | origin/main | | true | self\n",
        )
    }

    #[test]
    fn lane_check_admits_self_repo_for_any_partition() {
        let repos = repo_map_fixture();
        let result = lane_check(&format!("testscope,{MAECHEN}"), "self-repo", &repos, false);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test]
    fn lane_check_refuses_maechen_on_a_develop_repo() {
        let repos = repo_map_fixture();
        match lane_check(&format!("testscope,{MAECHEN}"), "dev-repo", &repos, false) {
            LaneCheck::Refused(msg) => {
                assert!(msg.contains("dev-repo"));
                assert!(msg.contains(MAECHEN));
                assert!(msg.contains("admitted lanes"));
                assert!(msg.contains("SPIRA_BEAD_LANE_OVERRIDE"));
            }
            LaneCheck::Admitted => panic!("expected a refusal"),
        }
    }

    #[test]
    fn lane_check_override_admits_everything() {
        let repos = repo_map_fixture();
        let result = lane_check(&format!("testscope,{MAECHEN}"), "dev-repo", &repos, true);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test]
    fn lane_check_plan_is_always_admitted() {
        let repos = repo_map_fixture();
        let result = lane_check("testscope,plan", "dev-repo", &repos, false);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    // -- label composition ----------------------------------------------------------------

    #[test]
    fn work_labels_appends_repo_and_express() {
        assert_eq!(
            work_labels("testscope,plan", "testrepo", "express", false),
            "testscope,plan,repo:testrepo"
        );
        assert_eq!(
            work_labels("testscope,plan", "testrepo", "express", true),
            "testscope,plan,repo:testrepo,express"
        );
    }

    #[test]
    fn non_work_labels_event_has_no_partition() {
        let got = non_work_labels("testscope", None, None, "express", false);
        assert_eq!(got, "testscope");
        assert!(!got.contains("plan"));
    }

    #[test]
    fn non_work_labels_insight_carries_its_label() {
        let got = non_work_labels("testscope", Some("insight"), None, "express", false);
        assert_eq!(got, "testscope,insight");
    }

    #[test]
    fn non_work_labels_repo_and_express() {
        let got = non_work_labels("testscope", None, Some("testrepo"), "express", true);
        assert_eq!(got, "testscope,repo:testrepo,express");
    }

    // -- lint judge -------------------------------------------------------------------------

    // literal-ok: test fixture partition vocabulary, mirroring the bash suite's own PART
    const PART: &str = "plan maechen-sweep spike czar-trigger incident groom";

    #[test]
    fn lint_judge_rows_match_the_bash_suite_table() {
        let add_one = format!("no partition label; add one or mark {NO_LOOP}");
        let with_no_loop = format!("repo:spira {NO_LOOP}");
        let rows: Vec<(&str, &str, &str, &str, u32, Vec<&str>)> = vec![
            ("repo:spira plan", "open", "task", PART, 0, vec![]),
            ("plan", "open", "task", PART, 1, vec!["no repo: label"]),
            (
                "repo:spira",
                "open",
                "task",
                PART,
                1,
                vec![add_one.as_str()],
            ),
            (with_no_loop.as_str(), "open", "task", PART, 0, vec![]),
            ("", "open", "event", PART, 0, vec![]),
            ("repo:spira", "open", "epic", PART, 0, vec![]),
            ("repo:spira", "closed", "task", PART, 0, vec![]),
            (
                "",
                "open",
                "task",
                PART,
                2,
                vec!["no repo: label", add_one.as_str()],
            ),
            ("repo:spira", "open", "bug", PART, 1, vec![add_one.as_str()]),
        ];
        for (labels, status, ty, part, want_n, want_lines) in &rows {
            let (n, lines) = lint_judge(labels, status, ty, part, NO_LOOP);
            assert_eq!(n, *want_n, "labels={labels:?} status={status} type={ty}");
            let want: Vec<String> = want_lines.iter().map(|s| s.to_string()).collect();
            assert_eq!(lines, want, "labels={labels:?} status={status} type={ty}");
        }
    }

    #[test]
    fn lint_judge_no_loop_label_unset_has_no_add_one_suffix() {
        let (n, lines) = lint_judge("repo:spira", "open", "task", PART, "");
        assert_eq!(n, 1);
        assert_eq!(lines, vec!["no partition label".to_string()]);
    }

    #[test]
    fn chamber_partitions_dedupes_and_excludes_scope() {
        let personas = vec![
            ("alpha".to_string(), "testscope,plan".to_string()),
            ("beta".to_string(), "testscope,incident".to_string()),
            ("gamma".to_string(), "plan".to_string()),
            ("empty".to_string(), "".to_string()),
        ];
        let got = chamber_partitions(&personas, "testscope");
        assert_eq!(got, vec!["plan".to_string(), "incident".to_string()]);
    }

    // -- show/dep-list JSON -----------------------------------------------------------------

    #[test]
    fn parse_show_row_handles_bare_object_and_one_element_array() {
        let obj = r#"{"labels":["repo:spira","plan"],"status":"open","issue_type":"task"}"#;
        let row = parse_show_row(obj).unwrap();
        assert_eq!(row.status, "open");
        assert_eq!(row.issue_type, "task");
        assert_eq!(row.labels, vec!["repo:spira", "plan"]);

        let arr = format!("[{obj}]");
        let row2 = parse_show_row(&arr).unwrap();
        assert_eq!(row2.labels, row.labels);
    }

    #[test]
    fn parse_show_row_none_on_garbage() {
        assert!(parse_show_row("not json").is_none());
    }

    #[test]
    fn parse_blocks_targets_reads_either_key() {
        let json = r#"[{"depends_on_id":"sp-a"},{"id":"sp-b"},{"depends_on_id":""}]"#;
        assert_eq!(
            parse_blocks_targets(json),
            vec!["sp-a".to_string(), "sp-b".to_string()]
        );
    }

    #[test]
    fn parse_list_ids_excludes_events() {
        let json = r#"[{"id":"sp-a","issue_type":"task"},{"id":"sp-ev.1","issue_type":"event"},{"id":"sp-b","issue_type":"bug"}]"#;
        assert_eq!(
            parse_list_ids(json),
            vec!["sp-a".to_string(), "sp-b".to_string()]
        );
    }

    #[test]
    fn parse_list_ids_empty_on_garbage() {
        assert_eq!(parse_list_ids("nope"), Vec::<String>::new());
    }

    #[test]
    fn parse_blocks_targets_empty_on_garbage() {
        assert_eq!(parse_blocks_targets("nope"), Vec::<String>::new());
    }

    #[test]
    fn branch_label_finds_first_and_strips_spira_prefix() {
        let labels = vec!["repo:spira".to_string(), "branch:spira/sp-x".to_string()];
        let raw = branch_label(&labels).unwrap();
        assert_eq!(branch_candidate(raw), "sp-x");
    }

    // -- dep add ------------------------------------------------------------------------------

    #[test]
    fn is_blocks_type_covers_every_alias() {
        assert!(is_blocks_type(None));
        assert!(is_blocks_type(Some("blocks")));
        assert!(is_blocks_type(Some("blocked-by")));
        assert!(is_blocks_type(Some("depends-on")));
        assert!(!is_blocks_type(Some("relates-to")));
    }

    #[test]
    fn incident_blocks_refusal_names_the_alternative() {
        let msg = incident_blocks_refusal("sp-a", "sp-b", "incident-test");
        assert!(msg.contains("incident-test"));
        assert!(msg.contains("bd dep relate sp-a sp-b"));
    }

    // -- contract -------------------------------------------------------------------------

    #[test]
    fn persona_line_no_labels_fallback() {
        assert_eq!(
            persona_line("alpha", "testscope,alpha-work"),
            "  alpha          testscope,alpha-work"
        );
        assert_eq!(persona_line("beta", ""), "  beta           (no labels)");
    }

    #[test]
    fn repos_section_lists_names_or_reports_absence() {
        let repos = repos_by_name("custrepo-one | /tmp/one | push | origin/main | | true | plan\n");
        assert_eq!(repos_section(&repos), "  custrepo-one\n");
        assert_eq!(repos_section(&BTreeMap::new()), "  (no repository map)\n");
    }
}
