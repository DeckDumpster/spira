//! Pure logic behind the `bead` binary. Contract: DESIGN.md. Kept separate from `main.rs`
//! (which does the `bdq`/`.fayth`/`schema.sh`/`mail.sh` subprocess work) so the CLI's own
//! rules — repo-map parsing, the lane-admission guard, label composition, the lint judge —
//! are unit-testable with no database, no chamber and no subprocess.

// ---------------------------------------------------------------------------------------
// Repo-map: `name | path | land | base | format | ? | lanes`, pipe-delimited, `#`-comments.
// Read directly from `$SPIRA_REPO_MAP` (not `spira.toml`) because every existing fixture
// pins this format — see DESIGN.md "Decisions".
// ---------------------------------------------------------------------------------------

/// One row of the repo-map: its name and the raw `lanes` column (empty when absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRow {
    pub name: String,
    pub lanes_raw: String,
}

/// A comment (optionally indented `#`) or blank line, skipped before any field-splitting —
/// matching the awk `/^[ \t]*#/ { next }` the bash used.
fn is_comment_or_blank(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty() || t.starts_with('#')
}

/// A bare identifier (`^[A-Za-z][A-Za-z0-9,_-]*$`) — the heuristic `repo_field`'s awk used
/// to tell a `lanes` column (an identifier or comma-list, or empty) from a `gate` column
/// (always contains spaces/slashes/`$`).
fn looks_like_lanes(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == ',' || c == '_' || c == '-')
}

/// Every row of `content` (the repo-map's own text) that names a repository, in order.
/// A row with fewer than 2 pipe-delimited fields (including a blank line, which splits to
/// exactly one empty field) is not a repository row and is skipped — matching `repo_names`'
/// `NF > 1` check.
pub fn parse_repo_map(content: &str) -> Vec<RepoRow> {
    let mut rows = Vec::new();
    for line in content.lines() {
        if is_comment_or_blank(line) {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        let name = fields[0].trim();
        if name.is_empty() || fields.len() <= 1 {
            continue;
        }
        // `_lanes_col_idx`: present only from the 7th field on, and only when that last
        // field is empty or looks like an identifier/comma-list (never a gate fragment).
        let lanes_raw = if fields.len() >= 7 {
            let last = fields[fields.len() - 1].trim();
            if last.is_empty() || looks_like_lanes(last) {
                last.to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        rows.push(RepoRow {
            name: name.to_string(),
            lanes_raw,
        });
    }
    rows
}

/// Every repository name in `content`, in order — `repo_names`.
pub fn repo_names(content: &str) -> Vec<String> {
    parse_repo_map(content)
        .into_iter()
        .map(|r| r.name)
        .collect()
}

/// The raw `lanes` column for `name`, or `None` when `name` is not in the map —
/// `repo_field <name> lanes`.
pub fn repo_lanes_raw<'a>(rows: &'a [RepoRow], name: &str) -> Option<&'a str> {
    rows.iter()
        .find(|r| r.name == name)
        .map(|r| r.lanes_raw.as_str())
}

// ---------------------------------------------------------------------------------------
// Lane labels — the six partition labels a repo-map row's `lanes` column, a persona's
// `FAYTH_LABELS`, and the lint judge's partition vocabulary all draw from.
// ---------------------------------------------------------------------------------------

/// The six configurable lane/partition label names, each read from its own env var with
/// the same default the bash used (`schema.sh name plan|incident|groomer|maechen|spike`,
/// inlined here since `bead.sh` itself never called `schema.sh` for these — it used the
/// bash `${VAR:-default}` fallback directly).
#[derive(Debug, Clone)]
pub struct LaneLabels {
    pub plan: String,
    pub incident: String,
    pub groom: String,
    pub maechen: String,
    pub spike: String,
    pub czar: String,
}

impl LaneLabels {
    pub fn from_env() -> Self {
        fn v(key: &str, default: &str) -> String {
            std::env::var(key)
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default.to_string())
        }
        LaneLabels {
            plan: v("SPIRA_PLAN_LABEL", "plan"),
            incident: v("SPIRA_INCIDENT_LABEL", "incident"),
            groom: v("SPIRA_GROOMER_LABEL", "groom"),
            maechen: v("SPIRA_MAECHEN_LABEL", "maechen-sweep"),
            spike: v("SPIRA_SPIKE_LABEL", "spike"),
            czar: v("SPIRA_CZAR_LABEL", "czar-trigger"),
        }
    }

    /// All six, the order `spira_repo_lanes` defaults to when a row has no `lanes` column
    /// (no restriction declared).
    pub fn all(&self) -> Vec<String> {
        vec![
            self.plan.clone(),
            self.incident.clone(),
            self.groom.clone(),
            self.maechen.clone(),
            self.spike.clone(),
            self.czar.clone(),
        ]
    }

    fn known(&self, item: &str) -> bool {
        [
            &self.plan,
            &self.incident,
            &self.groom,
            &self.maechen,
            &self.spike,
            &self.czar,
        ]
        .iter()
        .any(|l| l.as_str() == item)
    }
}

/// `spira_repo_lanes`/`_spira_expand_lanes`: the granted lane set for a repo-map row's raw
/// `lanes` field. `None`/`Some("")` admits every lane (no restriction declared). `consume`,
/// `develop` and `self` are mode names that expand to their fixed set; anything else is
/// read as a comma-list of lane labels, each validated — an unrecognised mode name and an
/// unrecognised lane label are the same failure (there is no separate "bad mode" check in
/// the bash, only "bad single-item list").
pub fn expand_lanes(
    repo_name: &str,
    raw: Option<&str>,
    labels: &LaneLabels,
) -> Result<Vec<String>, String> {
    let raw = match raw {
        None => return Ok(labels.all()),
        Some(r) if r.trim().is_empty() => return Ok(labels.all()),
        Some(r) => r,
    };
    match raw {
        "consume" => return Ok(vec![labels.plan.clone()]),
        "develop" => {
            return Ok(vec![
                labels.plan.clone(),
                labels.incident.clone(),
                labels.groom.clone(),
                labels.spike.clone(),
            ])
        }
        "self" => {
            return Ok(vec![
                labels.plan.clone(),
                labels.incident.clone(),
                labels.groom.clone(),
                labels.spike.clone(),
                labels.maechen.clone(),
                labels.czar.clone(),
            ])
        }
        _ => {}
    }
    let mut result = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if labels.known(item) {
            result.push(item.to_string());
        } else {
            return Err(format!(
                "spira: repo:{repo_name} — unknown lane {item} (valid: {},{},{},{},{},{})",
                labels.plan,
                labels.incident,
                labels.groom,
                labels.maechen,
                labels.spike,
                labels.czar
            ));
        }
    }
    if result.is_empty() {
        return Ok(vec![labels.plan.clone()]);
    }
    Ok(result)
}

/// The partition a persona's comma-joined `FAYTH_LABELS` carries, per `_bead_file`'s own
/// scan: the LAST label (in order) that matches the lane vocabulary wins — not the first,
/// matching the bash's un-`break`-ed loop.
pub fn fayth_partition(fayth_labels: &str, lane_labels: &LaneLabels) -> Option<String> {
    let mut partition = None;
    for raw in fayth_labels.split(',') {
        let l = raw.trim();
        if !l.is_empty() && lane_labels.known(l) {
            partition = Some(l.to_string());
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

/// `_bead_file`'s lane-admission guard. `repo_rows`/`lane_labels` come from the repo-map
/// and env; `fayth_labels` is the filing persona's raw `FAYTH_LABELS`. `override_set` is
/// `SPIRA_BEAD_LANE_OVERRIDE` being non-empty, which admits everything unconditionally.
pub fn lane_check(
    fayth_labels: &str,
    repo: &str,
    repo_rows: &[RepoRow],
    lane_labels: &LaneLabels,
    override_set: bool,
) -> LaneCheck {
    if override_set {
        return LaneCheck::Admitted;
    }
    let partition = match fayth_partition(fayth_labels, lane_labels) {
        Some(p) => p,
        None => return LaneCheck::Admitted,
    };
    let raw = repo_lanes_raw(repo_rows, repo);
    // `spira_repo_lanes ... 2>/dev/null || _repo_lanes="$_p"`: an expansion error (an
    // unrecognised mode/lane) is swallowed and treated as "only plan is admitted" —
    // preserved exactly, not "fixed", for parity.
    let admitted =
        expand_lanes(repo, raw, lane_labels).unwrap_or_else(|_| vec![lane_labels.plan.clone()]);
    if admitted.iter().any(|l| l == &partition) {
        return LaneCheck::Admitted;
    }
    let refuser = match raw {
        Some(r) if !r.is_empty() => format!("repo-map (lanes={r})"),
        _ => format!(
            "repo-map (no lanes column — defaults to {})",
            lane_labels.plan
        ),
    };
    LaneCheck::Refused(format!(
        "bead: repo:{repo} does not admit lane {partition} — refused by {refuser}\nbead: override: SPIRA_BEAD_LANE_OVERRIDE=1"
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
// calls (`fayth_names`/`fayth_get`, `schema.sh kinds`, the repo-map).
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

/// One `REPOS` line, or the map's absence.
pub fn repos_section(rows: &[RepoRow]) -> String {
    if rows.is_empty() {
        return "  (no repo-map)\n".to_string();
    }
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!("  {}\n", r.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- repo-map ------------------------------------------------------------------------

    #[test]
    fn repo_map_skips_comments_and_blanks() {
        let map = "# comment row, must not appear as a repo\ncustrepo-one | /tmp/one | push | origin/main | | true | plan\n\ncustrepo-two | /tmp/two | push | origin/main | | true | plan\n";
        let names = repo_names(map);
        assert_eq!(names, vec!["custrepo-one", "custrepo-two"]);
    }

    #[test]
    fn repo_map_six_field_row_has_no_lanes_column() {
        let map = "spira    | /srv/spira     | push | origin/main |  |\nwidget   | /srv/widget    | pr   | origin/main |  |\n";
        let rows = parse_repo_map(map);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "spira");
        assert_eq!(rows[0].lanes_raw, "");
    }

    #[test]
    fn repo_map_seven_field_row_has_a_lanes_column() {
        let map = "testrepo  | /tmp/test | push | origin/main | | true | plan\ndev-repo  | /tmp/dev  | push | origin/main | | true | develop\n";
        let rows = parse_repo_map(map);
        assert_eq!(repo_lanes_raw(&rows, "testrepo"), Some("plan"));
        assert_eq!(repo_lanes_raw(&rows, "dev-repo"), Some("develop"));
        assert_eq!(repo_lanes_raw(&rows, "nope"), None);
    }

    #[test]
    fn repo_map_empty_lanes_field_is_empty_not_absent() {
        // trailing "|" with nothing after it: 6 fields, no lanes column at all.
        let map = "r | /p | push | origin/main | |\n";
        let rows = parse_repo_map(map);
        assert_eq!(rows[0].lanes_raw, "");
    }

    // -- lanes ----------------------------------------------------------------------------

    fn test_labels() -> LaneLabels {
        LaneLabels {
            plan: "plan".into(),
            incident: "incident".into(),
            groom: "groom".into(),
            maechen: "maechen-sweep".into(),
            spike: "spike".into(),
            czar: "czar-trigger".into(),
        }
    }

    #[test]
    fn expand_lanes_no_column_admits_all() {
        let l = test_labels();
        let got = expand_lanes("r", None, &l).unwrap();
        assert!(got.contains(&"plan".to_string()));
        assert!(got.contains(&"maechen-sweep".to_string()));
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn expand_lanes_empty_field_admits_all() {
        let l = test_labels();
        let got = expand_lanes("r", Some(""), &l).unwrap();
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn expand_lanes_modes() {
        let l = test_labels();
        assert_eq!(
            expand_lanes("r", Some("consume"), &l).unwrap(),
            vec!["plan"]
        );
        assert_eq!(
            expand_lanes("r", Some("develop"), &l).unwrap(),
            vec!["plan", "incident", "groom", "spike"]
        );
        assert_eq!(
            expand_lanes("r", Some("self"), &l).unwrap(),
            vec![
                "plan",
                "incident",
                "groom",
                "spike",
                "maechen-sweep",
                "czar-trigger"
            ]
        );
    }

    #[test]
    fn expand_lanes_explicit_list() {
        let l = test_labels();
        assert_eq!(
            expand_lanes("r", Some("plan,incident"), &l).unwrap(),
            vec!["plan", "incident"]
        );
    }

    #[test]
    fn expand_lanes_unknown_is_an_error() {
        let l = test_labels();
        let err = expand_lanes("dev-repo", Some("bogus"), &l).unwrap_err();
        assert!(err.contains("repo:dev-repo"), "{err}");
        assert!(err.contains("unknown lane bogus"), "{err}");
    }

    #[test]
    fn fayth_partition_last_match_wins() {
        let l = test_labels();
        assert_eq!(
            fayth_partition("testscope,plan", &l),
            Some("plan".to_string())
        );
        assert_eq!(
            fayth_partition("plan,incident", &l),
            Some("incident".to_string())
        );
        assert_eq!(fayth_partition("testscope,alpha-work", &l), None);
        assert_eq!(fayth_partition("", &l), None);
    }

    #[test]
    fn lane_check_admits_self_repo_for_any_partition() {
        let rows = parse_repo_map("self-repo | /tmp/self | push | origin/main | | true | self\n");
        let l = test_labels();
        let result = lane_check("testscope,maechen-sweep", "self-repo", &rows, &l, false);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test]
    fn lane_check_refuses_maechen_on_a_develop_repo() {
        let rows =
            parse_repo_map("dev-repo  | /tmp/dev  | push | origin/main | | true | develop\n");
        let l = test_labels();
        match lane_check("testscope,maechen-sweep", "dev-repo", &rows, &l, false) {
            LaneCheck::Refused(msg) => {
                assert!(msg.contains("dev-repo"));
                assert!(msg.contains("maechen-sweep"));
                assert!(msg.contains("repo-map"));
                assert!(msg.contains("SPIRA_BEAD_LANE_OVERRIDE"));
            }
            LaneCheck::Admitted => panic!("expected a refusal"),
        }
    }

    #[test]
    fn lane_check_override_admits_everything() {
        let rows =
            parse_repo_map("dev-repo  | /tmp/dev  | push | origin/main | | true | develop\n");
        let l = test_labels();
        let result = lane_check("testscope,maechen-sweep", "dev-repo", &rows, &l, true);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test]
    fn lane_check_plan_is_always_admitted() {
        let rows =
            parse_repo_map("dev-repo  | /tmp/dev  | push | origin/main | | true | develop\n");
        let l = test_labels();
        let result = lane_check("testscope,plan", "dev-repo", &rows, &l, false);
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

    const PART: &str = "plan maechen-sweep spike czar-trigger incident groom";

    #[test]
    fn lint_judge_rows_match_the_bash_suite_table() {
        let rows: &[(&str, &str, &str, &str, u32, &[&str])] = &[
            ("repo:spira plan", "open", "task", PART, 0, &[]),
            ("plan", "open", "task", PART, 1, &["no repo: label"]),
            (
                "repo:spira",
                "open",
                "task",
                PART,
                1,
                &["no partition label; add one or mark no-loop"],
            ),
            ("repo:spira no-loop", "open", "task", PART, 0, &[]),
            ("", "open", "event", PART, 0, &[]),
            ("repo:spira", "open", "epic", PART, 0, &[]),
            ("repo:spira", "closed", "task", PART, 0, &[]),
            (
                "",
                "open",
                "task",
                PART,
                2,
                &[
                    "no repo: label",
                    "no partition label; add one or mark no-loop",
                ],
            ),
            (
                "repo:spira",
                "open",
                "bug",
                PART,
                1,
                &["no partition label; add one or mark no-loop"],
            ),
        ];
        for (labels, status, ty, part, want_n, want_lines) in rows {
            let (n, lines) = lint_judge(labels, status, ty, part, "no-loop");
            assert_eq!(n, *want_n, "labels={labels:?} status={status} type={ty}");
            assert_eq!(
                lines,
                want_lines.to_vec(),
                "labels={labels:?} status={status} type={ty}"
            );
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
        let rows = parse_repo_map("custrepo-one | /tmp/one | push | origin/main | | true | plan\n");
        assert_eq!(repos_section(&rows), "  custrepo-one\n");
        assert_eq!(repos_section(&[]), "  (no repo-map)\n");
    }
}
