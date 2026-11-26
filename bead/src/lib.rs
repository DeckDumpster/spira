//! Pure logic behind the `bead` binary. Contract: DESIGN.md. Kept separate from `main.rs`
//! (which does the `.rune`/`schema.sh`/`mail` subprocess work) so the CLI's own
//! rules — the lane-admission guard, label composition, the lint judge — are unit-testable
//! with no database, no runes and no subprocess.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod bdq;
#[path = "../tests/fixtures/bdsim.rs"]
pub mod bdsim;
pub mod door;
pub mod filing;
pub mod claimdesc;
pub mod guard;
pub mod lc;
pub mod event;
pub mod identity;
pub mod statute;

use std::collections::{BTreeMap, HashMap};

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

/// A raw `RUNE_LABELS`/lane token, read into the typed vocabulary — `None` when it names
/// no lane at all (an ordinary label like `testscope` or `alpha-work`).
pub fn label_to_lane(label: &str) -> Option<Lane> {
    serde_json::from_value(serde_json::Value::String(label.to_string())).ok()
}

/// The configured spelling of a lane (`Lane::MimirSweep` -> `"mimir-sweep"`) — the
/// inverse of [`label_to_lane`], read from the same typed source rather than a literal
/// copied here (which `literal-lint` refuses outside `schema.sh`/`conf.sh`).
pub fn lane_label(lane: Lane) -> String {
    serde_json::to_value(lane)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The partition a persona's comma-joined `RUNE_LABELS` carries, per `_bead_file`'s own
/// scan: the LAST label (in order) that matches the lane vocabulary wins — not the first,
/// matching the bash's un-`break`-ed loop.
pub fn rune_partition(rune_labels: &str) -> Option<Lane> {
    let mut partition = None;
    for raw in rune_labels.split(',') {
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

/// `_bead_file`'s lane-admission guard. `repos` is [`repos_by_name`]'s result; `rune_labels`
/// is the filing persona's raw `RUNE_LABELS`. `override_set` is `SPIRA_BEAD_LANE_OVERRIDE`
/// being non-empty, which admits everything unconditionally.
pub fn lane_check(
    rune_labels: &str,
    repo: &str,
    repos: &BTreeMap<String, RepoSection>,
    override_set: bool,
) -> LaneCheck {
    if override_set {
        return LaneCheck::Admitted;
    }
    let partition = match rune_partition(rune_labels) {
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

/// The `-l` label list for a `work`-kind filing: the persona's own `RUNE_LABELS`, plus
/// `repo:<repo>`. Express is lifecycle state, never a label.
pub fn work_labels(rune_labels: &str, repo: &str) -> String {
    format!("{rune_labels},repo:{repo}")
}

/// `labels` for a filing that repairs its repo's landing contract: the one label that admits a
/// work filing for a repo that breaks it.
pub fn with_contract_fix(labels: String, contract_fix: bool) -> String {
    if contract_fix {
        format!("{labels},{}", spira_config::scoped_stop::CONTRACT_FIX_LABEL)
    } else {
        labels
    }
}

fn depends_raw(body: &str) -> Option<&str> {
    body.lines().find_map(|l| l.strip_prefix("Depends:"))
}

fn is_depends_token(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    let draft = lower.strip_prefix("draft ").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let id = t.strip_prefix("sp-").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'));
    lower == "none" || draft || id
}

/// The first token of a body's `Depends:` line that is not a bead id, `draft N` or `none`.
pub fn depends_prose(body: &str) -> Option<String> {
    depends_raw(body)?.split(',').map(str::trim).find(|t| !t.is_empty() && !is_depends_token(t)).map(str::to_string)
}

const DRAFT_HEADING: &str = "### Draft:";

/// The last (title, body) of each trimmed draft title in `notes`, in order of those last
/// occurrences. A heading counts only at the start of a line.
fn last_drafts(notes: &str) -> Vec<(String, String)> {
    let notes = notes.replace("\r\n", "\n");
    let mut drafts: Vec<(String, String)> = Vec::new();
    for line in notes.split_inclusive('\n') {
        if let Some(title) = line.strip_prefix(DRAFT_HEADING) {
            drafts.push((title.trim().to_string(), String::new()));
        } else if let Some((_, body)) = drafts.last_mut() {
            body.push_str(line);
        }
    }
    let mut last: Vec<(String, String)> = Vec::new();
    for (title, body) in drafts.into_iter().rev() {
        if !last.iter().any(|(t, _)| *t == title) {
            last.push((title, body));
        }
    }
    last.reverse();
    last
}

/// One line per draft title whose last `### Draft:` note has a `Depends:` line carrying a token
/// `depends_prose` rejects.
pub fn draft_depends_refusals(notes: &str) -> Vec<String> {
    last_drafts(notes)
        .into_iter()
        .filter_map(|(title, body)| depends_prose(&body).map(|t| format!("draft \"{title}\": `Depends:` carries `{t}`")))
        .collect()
}

/// One line per draft title whose last `### Draft:` note has a non-blank line after its `Open:`
/// line other than a harness note: `Open:` ends a draft's note, and a correction is the whole
/// draft again. A no-progress note is two paragraphs, and both are the harness's.
pub fn draft_trailer_refusals(notes: &str) -> Vec<String> {
    last_drafts(notes)
        .into_iter()
        .filter_map(|(title, body)| {
            let mut second_paragraph_due = false;
            let line = body.lines().skip_while(|l| !l.starts_with("Open:")).skip(1).map(str::trim).filter(|l| !l.is_empty()).find(|l| {
                if std::mem::take(&mut second_paragraph_due) {
                    return false;
                }
                if l.starts_with(HARNESS_NO_PROGRESS_NOTE) {
                    second_paragraph_due = true;
                    return false;
                }
                !is_harness_note(l)
            })?;
            Some(format!("draft \"{title}\": after its Open: line comes `{line}`; a correction is the whole draft again as a new note, never a paragraph after one"))
        })
        .collect()
}

/// The `Depends: <id>[, <id>…]` line of a body; `none` names no one, and `draft N` is a draft
/// the filer has since given an id.
fn depends_line(body: &str) -> Option<Vec<String>> {
    let line = depends_raw(body)?;
    Some(line.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("none")).collect())
}

/// Why a Skuld-drafted body's dependencies and the `--depends-on` set disagree, or `None`.
/// Prose alone files no edge, so "depends on" needs the line; the line needs the flag.
pub fn depends_refusal(body: &str, depends_on: &[String]) -> Option<String> {
    if let Some(t) = depends_prose(body) {
        return Some(format!("the body's `Depends:` line carries `{t}`; a Depends line carries only bead ids, `draft N` or `none`; move the prose into Context"));
    }
    let Some(named) = depends_line(body) else {
        return body.to_ascii_lowercase().contains("depends on").then(|| {
            "the body says it \"depends on\" something but carries no `Depends: <id>[, <id>…]` line, and prose files no edge. Add the line to the draft, and pass each id as --depends-on <id>".to_string()
        });
    };
    let (drafts, ids): (Vec<&String>, Vec<&String>) = named.iter().partition(|t| t.to_ascii_lowercase().starts_with("draft"));
    let missing: Vec<&String> = ids.iter().filter(|i| !depends_on.contains(i)).copied().collect();
    let extra: Vec<&String> = depends_on.iter().filter(|d| !ids.contains(d)).collect();
    if !missing.is_empty() || extra.len() != drafts.len() {
        return Some(format!(
            "the body's `Depends:` line names [{}] but --depends-on is [{}]. Make them match: pass --depends-on <id> for each id the line names (a `draft N` is passed as the id it was filed under), or correct the line",
            named.join(", "),
            depends_on.join(", ")
        ));
    }
    None
}

/// Why `base` cannot be stacked under a new bead of `repo`, or `None` when it can: only a
/// live work bead of the same repo carries commits the new branch can be cut from.
pub fn stack_after_refusal(base: &str, row: Option<&ShowRow>, terminal: bool, repo: &str) -> Option<String> {
    let Some(row) = row else {
        return Some(format!("--stack-after {base}: no such bead"));
    };
    if matches!(row.issue_type.as_str(), "epic" | "decision" | "event" | "molecule" | "gate" | "proposal") {
        return Some(format!("--stack-after {base}: a {} carries no commits to stack on", row.issue_type));
    }
    if terminal {
        return Some(format!("--stack-after {base}: already closed; file without --stack-after"));
    }
    if !row.labels.iter().any(|l| l == &format!("repo:{repo}")) {
        return Some(format!("--stack-after {base}: not a repo:{repo} bead; a stack stays within one repo"));
    }
    None
}

/// The `-l` label list for a non-`work` kind: the scope label, `insight` for the `insight`
/// kind, and `repo:<repo>` when a repo was given.
pub fn non_work_labels(scope_label: &str, insight_label: Option<&str>, repo: Option<&str>) -> String {
    let mut out = scope_label.to_string();
    if let Some(ins) = insight_label {
        out.push(',');
        out.push_str(ins);
    }
    if let Some(r) = repo {
        out.push_str(",repo:");
        out.push_str(r);
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
pub const PARTITION_CHECK_TYPES: &[&str] = &["task", "bug", "feature", "chore", "spike"];

/// Whether a work bead still waits for dispatch, the one fact the partition check needs: the
/// lifecycle row's answer (READY/REWORK), never bd's `status` (design §3.4, sp-mve9i). A bead
/// with no row can never be claimed, so it waits for nothing.
pub fn awaits_dispatch(row: Option<&spira_store::lc_state::Row>) -> bool {
    row.is_some_and(|r| r.claimable())
}

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
/// space-separated, matching the bash's own calling convention. `awaits_dispatch` replaces
/// the bash's `status = open` ([`awaits_dispatch`]; a bead being filed awaits it).
pub fn lint_judge(
    labels: &str,
    awaits_dispatch: bool,
    ty: &str,
    partitions: &str,
    no_loop_label: &str,
) -> (u32, Vec<String>) {
    let mut out = Vec::new();

    if REPO_CHECK_TYPES.contains(&ty) && !has_label_starting_with(labels, "repo:") {
        out.push("no repo: label".to_string());
    }

    if awaits_dispatch && PARTITION_CHECK_TYPES.contains(&ty) {
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

/// The heading a bead filed for a persona that requires a build spec must carry.
pub const BUILD_HEADING: &str = "## Build";

/// Whether `text` carries a build spec: a `## Build` heading with at least one non-blank line
/// under it before the next heading. An outcome with no named deliverable is design work.
pub fn has_build_spec(text: &str) -> bool {
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim_end().eq_ignore_ascii_case(BUILD_HEADING) {
            for body in lines.by_ref() {
                let b = body.trim();
                if b.starts_with('#') {
                    break;
                }
                if !b.is_empty() {
                    return true;
                }
            }
            return false;
        }
    }
    false
}

/// The start of the note dwarf/src/teardown.rs writes on a submit; bead cannot depend on dwarf.
pub const HARNESS_SUBMIT_NOTE: &str = "Submitted: work committed on branch";
/// The start of the two-paragraph note teardown writes on a no-progress exit.
pub const HARNESS_NO_PROGRESS_NOTE: &str = "No progress (";

fn is_harness_note(line: &str) -> bool {
    line.starts_with(HARNESS_SUBMIT_NOTE) || line.starts_with(HARNESS_NO_PROGRESS_NOTE)
}

/// The refusal for a Concierge builder filing that names no Skuld request.
pub const SKULD_NEEDS_DRAFT: &str = "a builder bead the Concierge files needs --drafted-by <request-id>: the id of a Skuld request whose note holds this exact text. Get a draft or review first: bead.sh file \"<title>\" --for skuld --repo <r> --body-file <the intent, or your draft for review>, then file the note's text with --drafted-by <that id>";

/// Whether the Skuld fence binds a filing: a Concierge's own, unless its parent process is
/// `rule` (argv[0] basename `rule` or `rule.sh`, from `/proc/<ppid>/cmdline`), whose landing
/// bead is plumbing.
pub fn skuld_fence_binds(concierge: bool, requires_skuld: bool, parent_cmdline: &[u8]) -> bool {
    concierge && requires_skuld && !parent_is_rule(parent_cmdline)
}

/// Whether NUL-separated `cmdline` bytes name a program whose basename is exactly `rule` or
/// `rule.sh`: the shim runs `exec -a "$0"`, so the process's argv[0] is the shim's path.
pub fn parent_is_rule(cmdline: &[u8]) -> bool {
    let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or(&[]);
    matches!(argv0.rsplit(|b| *b == b'/').next(), Some(b"rule") | Some(b"rule.sh"))
}

/// `Ok` when `notes` holds `### Draft: <title>`, a blank line, then `body` trimmed and verbatim
/// (ending the note, or followed by an `Open:` line, another draft, or a harness note),
/// on a request labelled `skuld_label`; else the refusal text.
pub fn skuld_drafted(title: &str, body: &str, skuld_label: &str, request: &str, labels: &[String], notes: &str) -> Result<(), String> {
    let how = "get a draft or review first: bead.sh file \"<title>\" --for skuld --repo <r> --body-file <the intent, or your draft for review>; then file the text of the note Skuld leaves with --drafted-by <that id>";
    let body = body.replace("\r\n", "\n");
    let body = body.trim();
    if body.is_empty() {
        return Err(format!("the body is empty; {how}"));
    }
    if !labels.iter().any(|l| l == skuld_label) {
        return Err(format!("{request} is not a Skuld request (no `{skuld_label}` label); {how}"));
    }
    let wanted = format!("### Draft: {}\n\n{body}", title.trim());
    let notes = notes.replace("\r\n", "\n");
    let held = notes.match_indices(&wanted).any(|(at, m)| {
        let rest = notes[at + m.len()..].trim_start();
        rest.is_empty() || rest.starts_with("Open:") || rest.starts_with("### Draft:") || is_harness_note(rest)
    });
    if held {
        Ok(())
    } else if let Some(line) = notes.match_indices(&wanted).find_map(|(at, m)| {
        let rest = &notes[at + m.len()..];
        rest.starts_with('\n').then(|| rest.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)).flatten()
    }) {
        Err(format!("a `### Draft: {}` note on {request} holds this body, but the note continues with `{line}`; ask Skuld to resend the whole draft; {how}", title.trim()))
    } else {
        Err(format!("no `### Draft: {}` note on {request} holds this body verbatim; {how}", title.trim()))
    }
}

/// A Skuld draft read from request notes: the text under its heading, and the `Open:` line that ended it.
#[derive(Debug, PartialEq)]
pub struct Draft {
    pub body: String,
    pub open: Option<String>,
}

/// The last `### Draft: <title>` in `notes` (a correction is the whole draft resent): the text after
/// the heading and a blank line, up to another draft, an `Open:` line, a harness note or the end.
pub fn draft_body(notes: &str, title: &str) -> Option<Draft> {
    let heading = format!("### Draft: {}\n\n", title.trim());
    let notes = notes.replace("\r\n", "\n");
    let at = notes.match_indices(&heading).filter(|(i, _)| *i == 0 || notes[..*i].ends_with('\n')).last()?.0;
    let rest = &notes[at + heading.len()..];
    let mut end = rest.len();
    let mut open = None;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end();
        if bare.starts_with("### Draft:") || is_harness_note(bare) {
            end = offset;
            break;
        }
        if bare.starts_with("Open:") {
            end = offset;
            open = Some(bare.to_string());
            break;
        }
        offset += line.len();
    }
    let body = rest[..end].trim().to_string();
    (!body.is_empty()).then_some(Draft { body, open })
}

/// One piece of a request's notes: a draft under its heading (`title` set), or other text.
struct Piece {
    title: Option<String>,
    text: String,
}

/// `notes` cut into drafts and the text between them. A draft ends after its `Open:` line, or
/// before the next draft heading or harness note.
fn pieces(notes: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut drafting = false;
    for line in notes.replace("\r\n", "\n").split_inclusive('\n') {
        let bare = line.trim_end();
        let heading = bare.strip_prefix(DRAFT_HEADING);
        if heading.is_some() || is_harness_note(bare) || out.is_empty() || !drafting && out.last().is_some_and(|p| p.title.is_some()) {
            out.push(Piece { title: heading.map(|t| t.trim().to_string()), text: String::new() });
            drafting = heading.is_some();
        }
        let last = out.last_mut().expect("a piece was just pushed");
        last.text.push_str(line);
        if drafting && bare.starts_with("Open:") {
            drafting = false;
        }
    }
    out
}

/// `notes` with each `### Draft:` of `text` put in place of the same-titled draft (later
/// duplicates dropped) or appended, and the rest of `text` appended; `None` when `text` has no
/// draft heading, so a plain note stays an append.
pub fn with_drafts(notes: &str, text: &str) -> Option<String> {
    let incoming = pieces(text);
    if !incoming.iter().any(|p| p.title.is_some()) {
        return None;
    }
    let mut kept = pieces(notes);
    let mut tail: Vec<Piece> = Vec::new();
    for piece in incoming {
        match &piece.title {
            Some(title) => {
                match kept.iter_mut().chain(tail.iter_mut()).find(|p| p.title.as_ref() == Some(title)) {
                    Some(held) => held.text = piece.text,
                    None => tail.push(piece),
                }
            }
            None => tail.push(piece),
        }
    }
    let mut seen: Vec<String> = Vec::new();
    kept.retain(|p| match &p.title {
        Some(t) if seen.contains(t) => false,
        Some(t) => {
            seen.push(t.clone());
            true
        }
        None => true,
    });
    let mut parts: Vec<&str> = kept.iter().chain(&tail).map(|p| p.text.trim_end_matches('\n')).collect();
    parts.retain(|p| !p.is_empty());
    Some(parts.join("\n"))
}

/// The body of the Skuld request filed for a follow-up a dwarf filed: it asks for one draft headed
/// with the follow-up's title verbatim, and carries the filer's text.
pub fn review_request_body(followup: &str, title: &str, filer: &str, bound: &str, filer_text: &str) -> String {
    format!(
        "Review {followup} (\"{title}\"), a follow-up {filer} filed from {bound}. The follow-up waits on this request.\n\n\
         Write exactly one draft with `work note -`. Its first line is the heading below, then a blank line, then the item; on `work done` it replaces {followup}'s body:\n\n\
         ### Draft: {title}\n\n\
         ## The filer's text\n{}\n",
        filer_text.trim()
    )
}

pub const INTENT_HEADING: &str = "## Intent";
pub const USER_PATH_HEADING: &str = "## User path";

/// The non-blank text under `heading` up to the next heading of any level, or `None` when the
/// heading is absent or has nothing under it.
pub fn section(text: &str, heading: &str) -> Option<String> {
    let mut lines = text.lines();
    lines.find(|l| l.trim_end().eq_ignore_ascii_case(heading))?;
    let body: Vec<&str> = lines.take_while(|l| !l.trim_start().starts_with('#')).collect();
    let body = body.join("\n").trim().to_string();
    (!body.is_empty()).then_some(body)
}

#[derive(Debug, Default, PartialEq)]
pub struct PilgrimageArgs {
    pub design: String,
    pub repo: String,
    pub priority: Option<String>,
    pub after: Vec<String>,
}

pub fn pilgrimage_args(args: &[String]) -> Result<PilgrimageArgs, String> {
    let usage = "usage: bead.sh pilgrimage <design-file> --repo <name> --after <build-id>... [--priority N]";
    let mut design: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut after: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => {
                i += 1;
                repo = args.get(i).cloned();
            }
            "--priority" | "-p" => {
                i += 1;
                priority = args.get(i).cloned();
            }
            "--after" => {
                let taken = args[i + 1..].iter().take_while(|a| !a.starts_with('-')).count();
                if taken == 0 {
                    return Err(usage.to_string());
                }
                after.extend(args[i + 1..=i + taken].iter().cloned());
                i += taken;
            }
            a if a.starts_with('-') => return Err(format!("bead: unknown option: {a}\n{usage}")),
            a => {
                if let Some(first) = &design {
                    return Err(format!("bead: pilgrimage: two design files: {first} and {a}"));
                }
                design = Some(a.to_string());
            }
        }
        i += 1;
    }
    let (Some(design), Some(repo)) = (design, repo) else {
        return Err(usage.to_string());
    };
    Ok(PilgrimageArgs { design, repo, priority, after })
}

/// The title and body of a design's pilgrimage bead: its Intent and its User path, and the
/// acceptance of a clean walk. A design without a User path is refused, since the walk is built from it.
pub fn pilgrimage_spec(design: &str) -> Result<(String, String), String> {
    let name = design
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or("the design has no `# ` title")?;
    let intent = section(design, INTENT_HEADING).ok_or_else(|| format!("the design has no `{INTENT_HEADING}` section"))?;
    let path = section(design, USER_PATH_HEADING).ok_or_else(|| {
        format!("the design has no `{USER_PATH_HEADING}` section: the steps a customer takes, which the pilgrimage walks")
    })?;
    let body = format!(
        "{INTENT_HEADING}\n\n{intent}\n\n{USER_PATH_HEADING}\n\n{path}\n\n## Acceptance\n\nA clean walk: every step of the user path done as written, with no defect filed.\n"
    );
    Ok((format!("Pilgrimage: {name}"), body))
}

/// Whether a bead carrying `labels` is builder work: every `builder_labels` token is present.
pub fn is_builder_bead(labels: &[String], builder_labels: &[String]) -> bool {
    !builder_labels.is_empty() && builder_labels.iter().all(|b| labels.iter().any(|l| l == b))
}

/// The partition vocabulary `_bead_lint` itself computes from the runes: every rune's
/// `RUNE_LABELS`, comma-split, trimmed, de-duplicated, excluding the empty string and
/// (when non-empty) `scope_label`. `personas` is `(name, raw RUNE_LABELS)` pairs, already
/// read (via the `.rune` bridge, in main.rs — this function is the pure part).
pub fn runes_partitions(personas: &[(String, String)], scope_label: &str) -> Vec<String> {
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
    pub description: String,
    pub notes: String,
    pub parent: Option<String>,
}

/// An open idea in `bd list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaRow {
    pub id: String,
    pub title: String,
    pub created_at: String,
}

/// The open rows of a `bd list --json` array, newest first. A row bd reports as anything
/// but open is not in the holding area.
pub fn idea_rows(json: &str) -> Vec<IdeaRow> {
    use spira_config::nonwork::{is_open, Kind};
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let mut out: Vec<IdeaRow> = rows
        .iter()
        .filter(|r| is_open(Kind::Idea, spira_config::nonwork::status_of(Kind::Idea, r)))
        .map(|r| IdeaRow { id: text(r, "id"), title: text(r, "title"), created_at: text(r, "created_at") })
        .filter(|r| !r.id.is_empty())
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    out
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
    let description = obj
        .get("description")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let notes = obj.get("notes").and_then(|s| s.as_str()).unwrap_or("").to_string();
    Some(ShowRow {
        labels,
        status,
        issue_type,
        description,
        notes,
        parent: obj
            .get("parent")
            .and_then(|p| p.as_str())
            .map(str::to_string),
    })
}

/// The `title` of one `bd show --json` payload (object or one-element array).
pub fn parse_title(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = match &v {
        serde_json::Value::Array(a) => a.first()?,
        other => other,
    };
    obj.get("title").and_then(|s| s.as_str()).map(str::to_string)
}

/// The `priority` field of one `bd show --json` payload (object or one-element array).
pub fn parse_priority(json: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = match &v {
        serde_json::Value::Array(a) => a.first()?,
        other => other,
    };
    obj.get("priority").and_then(|p| p.as_i64())
}

/// The `issue_type` field of one `bd show --json` payload (object or one-element array).
pub fn parse_issue_type(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = match &v {
        serde_json::Value::Array(a) => a.first()?,
        other => other,
    };
    obj.get("issue_type").and_then(|p| p.as_str()).map(str::to_string)
}

/// Whether a priority change on this bead is mirrored to a lifecycle row: only a work kind has one.
pub fn carries_lifecycle_row(show_json: &str) -> bool {
    spira_config::nonwork::is_work_type(parse_issue_type(show_json).as_deref())
}

/// The `description` field of one `bd show --json` payload (object or one-element array).
pub fn parse_description(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = match &v {
        serde_json::Value::Array(a) => a.first()?,
        other => other,
    };
    Some(obj.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string())
}

/// What a replace prints so the loss is visible: the old body's length and first lines.
pub fn replaced_body_summary(old: &str) -> String {
    let head: Vec<&str> = old.lines().take(3).collect();
    format!(
        "replaced description ({} bytes, {} lines); old body began:\n  {}",
        old.len(),
        old.lines().count(),
        head.join("\n  ")
    )
}

/// The old body with `addition` kept beneath a dated separator.
pub fn appended_body(old: &str, addition: &str, date: &str) -> String {
    format!("{}\n\n--- appended {date} ---\n\n{}\n", old.trim_end(), addition.trim_end())
}

/// A priority argument: 0..=4, optionally written `P1`.
pub fn parse_priority_arg(arg: &str) -> Option<i64> {
    let n: i64 = arg.strip_prefix('P').unwrap_or(arg).parse().ok()?;
    (0..=4).contains(&n).then_some(n)
}

/// The note recorded on a bead whose priority was changed through `amend`.
pub fn priority_note(actor: &str, old: Option<i64>, new: i64, reason: Option<&str>) -> String {
    let old = old.map_or("unknown".to_string(), |o| format!("P{o}"));
    let mut note = format!("Priority changed P{new} (was {old}) by {actor}.");
    if let Some(r) = reason {
        note.push_str(&format!(" Reason: {r}"));
    }
    note
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

/// The `relates-to` link targets in `rel_json` that `blocks` does not already name. Err when
/// the list is not a JSON array: an unreadable answer is never "no links".
pub fn relates_only_targets(blocks: &[String], rel_json: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(rel_json).map_err(|e| format!("not JSON: {e}"))?;
    if !v.is_array() {
        return Err("not a JSON array".into());
    }
    Ok(parse_blocks_targets(rel_json).into_iter().filter(|t| !blocks.iter().any(|b| b == t)).collect())
}

/// The id `bd create` printed: bare under `--silent`, or the `id` of the object (or
/// one-element array) under `--json`.
pub fn parse_created_id(out: &str) -> Option<String> {
    let out = out.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(out) {
        let obj = match &v {
            serde_json::Value::Array(a) => a.first()?,
            other => other,
        };
        return obj.get("id").and_then(|x| x.as_str()).map(str::to_string);
    }
    let last = out.lines().last()?.trim();
    (!last.is_empty() && !last.contains(char::is_whitespace)).then(|| last.to_string())
}

/// What reading a title back from the store says about a create whose reply was lost.
#[derive(Debug, PartialEq)]
pub enum Readback {
    Found(String),
    Absent,
    Ambiguous,
    Unreadable,
}

/// Reads a `bd list --json` answer for the bead a lost create may have written: the one
/// non-event bead titled `title` created at or after `since` (a UTC `YYYY-MM-DDTHH:MM:SS`
/// prefix), so an older bead of the same title is never taken for this one. A listing that
/// failed or is not JSON is `Unreadable`, never `Absent`: only a readable listing without the
/// bead proves nothing was written. A row whose id is `except` (the request a draft came from)
/// is not a hit, so a draft is not answered with the request that shares its title.
pub fn readback(list_code: i32, listing: &str, title: &str, since: &str, except: Option<&str>) -> Readback {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(listing) else { return Readback::Unreadable };
    let Some(rows) = v.as_array().filter(|_| list_code == 0) else { return Readback::Unreadable };
    let mut hits = rows.iter().filter(|d| {
        let at = d.get("created_at").and_then(|t| t.as_str()).unwrap_or("");
        d.get("title").and_then(|t| t.as_str()) == Some(title)
            && except.is_none_or(|e| d.get("id").and_then(|i| i.as_str()) != Some(e))
            && d.get("issue_type").and_then(|t| t.as_str()) != Some("event")
            && at.ends_with('Z')
            && at.get(..19).is_some_and(|p| p >= since)
    });
    let Some(first) = hits.next() else { return Readback::Absent };
    match (first.get("id").and_then(|i| i.as_str()), hits.next()) {
        (Some(id), None) => Readback::Found(id.to_string()),
        _ => Readback::Ambiguous,
    }
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

/// The refusal text of a bd dependency add when `depid` carries the incident label and `dep_type`
/// would wire a `blocks` edge onto it.
pub fn incident_blocks_refusal(id: &str, depid: &str, incident_label: &str) -> String {
    format!(
        "bead: dep add: refusing — {depid} carries the {incident_label} label and can never finish (an alarm re-arms on every recurrence); a blocks edge onto it has no completion path. Use: bd dep relate {id} {depid}"
    )
}

/// The refusal for a `blocks` edge onto an epic: an epic closes only when its children
/// close, so the child could never become ready.
pub fn epic_blocks_refusal(id: &str, depid: &str) -> String {
    format!(
        "bead: dep add: refusing — {depid} is an epic, which closes only when its children close; a blocks edge onto it deadlocks {id}. Use: bead.sh dep add {id} {depid} --type parent-child"
    )
}

// ---------------------------------------------------------------------------------------
// `contract` — formatting only; the three sections' data comes from main.rs's subprocess
// calls (`rune_names`/`rune_get`, `schema.sh kinds`, `repos_by_name`).
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

// ---------------------------------------------------------------------------------------
// lint: the loop over beads, behind a seam that hands it the facts it reads.
// ---------------------------------------------------------------------------------------

/// Everything `lint` reads about the store; `main.rs` backs it with `bd`, tests with rows.
pub trait LintSource {
    fn ids(&self) -> Vec<String>;
    fn show(&self, id: &str) -> Option<ShowRow>;
    /// `None` when the dep list could not be read: unreadable, never "no blockers".
    fn blocks_targets(&self, id: &str) -> Option<Vec<String>>;
    fn relates_targets(&self, id: &str) -> Result<Vec<String>, String>;
    fn lifecycle(&self) -> Result<HashMap<String, spira_store::lc_state::Row>, String>;
}

pub struct LintCfg {
    pub partitions: Vec<String>,
    pub spec_partitions: Vec<Vec<String>>,
    pub no_loop_label: String,
    pub ask_label: String,
    pub incident_label: String,
}

pub type Lifecycle = Option<Result<HashMap<String, spira_store::lc_state::Row>, String>>;

/// Finished by the lifecycle machine's judgement, never bd status; an unreadable state counts as not finished.
pub fn lc_done(src: &dyn LintSource, lc: &mut Lifecycle, id: &str) -> bool {
    match lc.get_or_insert_with(|| src.lifecycle()) {
        Ok(rows) => rows.get(id).is_some_and(|r| r.terminal()),
        Err(_) => false,
    }
}

/// Whether a bead carrying `labels` is an incident; the one comparison filing relies on.
pub fn is_incident(labels: &[String], incident_label: &str) -> bool {
    labels.iter().any(|l| l == incident_label)
}

/// The edge filing a remedy under `parent` adds: the incident blocks on the new bead, since a
/// bd parent cannot be blocked on its own child. `None` under an ordinary parent.
pub fn remedy_edge(parent_labels: &[String], incident_label: &str, new_id: &str, parent_id: &str) -> Option<(String, String)> {
    is_incident(parent_labels, incident_label).then(|| (parent_id.to_string(), new_id.to_string()))
}

/// Judges every bead `src` lists; returns the exit code and the stderr lines, in order.
pub fn lint(src: &dyn LintSource, cfg: &LintCfg) -> (i32, Vec<String>) {
    let partitions_joined = cfg.partitions.join(" ");
    let mut out: Vec<String> = Vec::new();
    let mut label_cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut lc: Lifecycle = None;

    for id in &src.ids() {
        if id.is_empty() {
            continue;
        }
        let Some(row) = src.show(id) else {
            out.push(format!("bead: {id}: unreadable (bd show failed)"));
            continue;
        };
        let labels_joined = row.labels.join(" ");

        if let Some(braw) = branch_label(&row.labels) {
            let cand = branch_candidate(braw);
            if !cand.is_empty() && cand != id && src.show(cand).is_some() {
                out.push(format!("bead: {id}: branch: label names {cand}, not itself"));
            }
        }

        let Some(blocks) = src.blocks_targets(id) else {
            out.push(format!("bead: {id}: unreadable (bd dep list failed)"));
            continue;
        };
        for oid in &blocks {
            label_cache
                .entry(oid.clone())
                .or_insert_with(|| src.show(oid).map(|r| r.labels).unwrap_or_default());
        }
        let blocked_has = |oid: &String, label: &str| label_cache.get(oid).is_some_and(|ls| ls.iter().any(|l| l == label));

        if !cfg.ask_label.is_empty() && row.labels.contains(&cfg.ask_label) {
            for oid in blocks.iter().filter(|o| blocked_has(o, &cfg.ask_label)) {
                out.push(format!("bead: {id}: blocks edge to ask-labelled {oid} (asks must not block each other; use bd dep relate)"));
            }
        }
        for oid in blocks.iter().filter(|o| blocked_has(o, &cfg.incident_label)) {
            out.push(format!("bead: {id}: blocks edge to incident-labelled {oid} (alarms must not block work; use bd dep relate)"));
        }

        let awaits = if PARTITION_CHECK_TYPES.contains(&row.issue_type.as_str()) {
            let first = lc.is_none();
            match lc.get_or_insert_with(|| src.lifecycle()) {
                Ok(rows) => awaits_dispatch(rows.get(id.as_str())),
                Err(e) => {
                    if first {
                        out.push(format!("bead: lifecycle state unreadable ({e}) — the partition check cannot be judged"));
                    }
                    false
                }
            }
        } else {
            false
        };
        if awaits && cfg.spec_partitions.iter().any(|p| is_builder_bead(&row.labels, p)) && !has_build_spec(&row.description) {
            out.push(format!("bead: {id}: no build spec: a builder bead needs a `{BUILD_HEADING}` section naming what to build (or route it --for spike)"));
        }
        if row.labels.contains(&cfg.incident_label) && !lc_done(src, &mut lc, id) {
            let related = match src.relates_targets(id) {
                Ok(t) => t,
                Err(e) => {
                    out.push(format!("bead: {id}: unreadable (bd dep list --type relates-to{e})"));
                    continue;
                }
            };
            for oid in related.into_iter().filter(|o| !blocks.contains(o)) {
                let Some(orow) = src.show(&oid) else { continue };
                if lc_done(src, &mut lc, &oid) || orow.labels.contains(&cfg.incident_label) {
                    continue;
                }
                out.push(format!("bead: {id}: open remedy {oid} is linked relates-to only (an incident with a fix in flight must block on it; use bd dep add {id} {oid})"));
            }
        }

        let (_, lines) = lint_judge(&labels_joined, awaits, &row.issue_type, &partitions_joined, &cfg.no_loop_label);
        for line in lines {
            out.push(format!("bead: {id}: {line}"));
        }
    }
    (i32::from(!out.is_empty()), out)
}

#[cfg(test)]
mod tests {
    use super::{depends_prose, depends_refusal, draft_body, draft_depends_refusals, review_request_body, with_drafts, Draft};

    
    #[test_id(tmuoozk)]
    #[test]
    fn a_second_write_of_one_title_replaces_the_first_in_place() {
        let first = "### Draft: A\n\nold a\n\nOpen: x\n### Draft: B\n\nb text\nOpen: y";
        let merged = with_drafts(first, "### Draft: A\n\nnew a\n\nOpen: z").unwrap();
        assert_eq!(merged, "### Draft: A\n\nnew a\n\nOpen: z\n### Draft: B\n\nb text\nOpen: y");
        assert!(!merged.contains("old a"));
    }

    
    #[test_id(tfxsh4v)]
    #[test]
    fn two_titles_are_both_held_and_trailing_text_and_duplicates_are_handled() {
        let merged = with_drafts("### Draft: A\n\na1", "### Draft: B\n\nb1").unwrap();
        assert_eq!(merged, "### Draft: A\n\na1\n### Draft: B\n\nb1");
        let dup = "### Draft: A\n\na1\nOpen: o\nkept note\n### Draft: A\n\na2";
        let merged = with_drafts(dup, "### Draft: A\n\na3\nOpen: p").unwrap();
        assert_eq!(merged, "### Draft: A\n\na3\nOpen: p\nkept note");
    }

    
    #[test_id(t6isv9q)]
    #[test]
    fn a_draft_ends_before_a_harness_note_and_a_plain_note_is_not_merged() {
        let notes = "### Draft: A\n\na1\nSubmitted: work committed on branch x";
        let merged = with_drafts(notes, "### Draft: A\n\na2").unwrap();
        assert_eq!(merged, "### Draft: A\n\na2\nSubmitted: work committed on branch x");
        assert_eq!(with_drafts(notes, "just a note\n## Draft: not a heading"), None);
    }

    #[test_id(t7mmyaf)]
    #[test]
    fn prose_on_the_depends_line_is_named_not_reported_as_a_flag_mismatch() {
        let body = "Depends: none. Related: sp-a (x). Both touch y; sequence them\n";
        let tok = depends_prose(body).expect("prose found");
        assert!(tok.starts_with("none."), "{tok}");
        let why = depends_refusal(body, &[]).expect("refused");
        assert!(why.contains(&tok) && why.contains("move the prose into Context"), "{why}");
        for ok in ["Depends: none", "Depends: sp-a, sp-b.1", "Depends: draft 2, sp-c"] {
            assert_eq!(depends_prose(ok), None, "{ok}");
        }
    }

    #[test_id(tqpawsr)]
    #[test]
    fn only_the_draft_with_prose_is_named() {
        let notes = "### Draft: First\n\nDepends: none\n\n### Draft: Second\n\nDepends: sp-a and more\n";
        let found = draft_depends_refusals(notes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("Second"), "{found:?}");
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(t2qxiht)]
    #[test]
    fn prose_depends_on_with_no_depends_line_is_refused() {
        let why = depends_refusal("This item depends on draft 1.\n", &[]).expect("refused");
        assert!(why.contains("Depends:") && why.contains("--depends-on"), "{why}");
        assert_eq!(depends_refusal("Nothing to wait for.\n", &[]), None, "the control: no prose, no line");
    }

    #[test_id(tcszd0i)]
    #[test]
    fn a_depends_line_matching_the_flags_is_accepted() {
        assert_eq!(depends_refusal("depends on sp-a\n\nDepends: sp-a\n", &ids(&["sp-a"])), None);
        assert_eq!(depends_refusal("Depends: sp-a, sp-b\n", &ids(&["sp-b", "sp-a"])), None);
        assert_eq!(depends_refusal("Depends: none\n", &[]), None);
    }

    #[test_id(tg8hx6a)]
    #[test]
    fn a_depends_line_the_flags_do_not_match_is_refused_both_ways() {
        assert!(depends_refusal("Depends: sp-b\n", &[]).is_some());
        assert!(depends_refusal("Depends: sp-b\n", &ids(&["sp-a"])).is_some());
        assert!(depends_refusal("Depends: sp-a\n", &ids(&["sp-a", "sp-c"])).is_some(), "a flag the line omits");
    }

    #[test_id(tl5gluc)]
    #[test]
    fn a_draft_n_on_the_line_is_matched_by_one_extra_flag() {
        assert_eq!(depends_refusal("Depends: draft 1, sp-c\n", &ids(&["sp-c", "sp-x"])), None);
        assert!(depends_refusal("Depends: draft 1, sp-c\n", &ids(&["sp-c"])).is_some());
    }


    #[test_id(ta85j73)]
    #[test]
    fn skuld_drafted_passes_an_exact_match_and_an_open_line_after_it() {
        let l = vec!["draft".to_string()];
        let notes = "### Draft: T\n\nbody line\nmore\n";
        assert!(skuld_drafted("T", "body line\nmore\n\n", "draft", "sp-r", &l, notes).is_ok());
        let open = "chatter\n### Draft: T\n\nbody line\nmore\n\nOpen: which repo?\n";
        assert!(skuld_drafted("T", "body line\nmore", "draft", "sp-r", &l, open).is_ok());
    }

    #[test_id(t8n13bm)]
    #[test]
    fn the_skuld_fence_spares_only_a_parent_named_rule() {
        assert!(!skuld_fence_binds(true, true, b"rule\0enact\0law-x\0"));
        assert!(!skuld_fence_binds(true, true, b"bin/rule\0code\0--land\0"));
        assert!(!skuld_fence_binds(true, true, b"spira-releases/4bf2391c328e783a572d4a6b50a8caa62a79e9e5/rule.sh\0code\0--land\0"));
        assert!(!skuld_fence_binds(true, true, b"rule.sh\0enact\0law-x\0"));
        assert!(skuld_fence_binds(true, true, b"bash\0-c\0rule\0"));
        assert!(skuld_fence_binds(true, true, b"x/rule-wrapper\0enact\0"));
        assert!(skuld_fence_binds(true, true, b"x/myrule.sh\0"));
        assert!(skuld_fence_binds(true, true, b""));
        assert!(!skuld_fence_binds(false, true, b"bash\0"));
        assert!(SKULD_NEEDS_DRAFT.starts_with("a builder bead the Concierge files needs --drafted-by <request-id>"));
    }

    #[test_id(teu5pxv)]
    #[test]
    fn draft_trailer_refusals_name_prose_after_an_open_line_only() {
        let sp = "### Draft: First\n\nbody\nOpen: q; default: d\nCorrection to \"First\": 12, not 11.\n### Draft: Second\n\nbody2\n";
        let found = draft_trailer_refusals(sp);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("draft \"First\"") && found[0].contains("Correction to"), "{found:?}");
        assert!(draft_trailer_refusals("### Draft: T\n\nbody\nmore\n").is_empty());
        assert!(draft_trailer_refusals("### Draft: T\n\nbody\nOpen: q\n").is_empty());
        assert!(draft_trailer_refusals("### Draft: T\n\nbody\nOpen: q\n\nSubmitted: work committed on branch x\n").is_empty());
    }

    const HELD: &str = "No progress (no-commit): nothing moved\n\nHeld for 30m (no-progress streak 2, branch stuck at abc) — not re-claimed until 2026-11-24T10:00:00Z. No attempt charged.\n";
    const AFTER: &str = "No progress (no-commit): nothing moved\n\nAfter 3 consecutive no-progress exits at abc, routed to the Concierge as a blocking question: the bead is not claimable again until it is answered. No attempt charged.\n";

    #[test_id(tmq3lh7)]
    #[test]
    fn a_clean_resend_supersedes_a_draft_with_prose_after_its_open_line() {
        let notes = "### Draft: T\n\nbody\nOpen: q\nConcierge: a note\n\n### Draft: T\n\nbody\nOpen: q\n";
        assert!(draft_trailer_refusals(notes).is_empty(), "{:?}", draft_trailer_refusals(notes));
        let guard = "### Draft: T\n\nbody\nOpen: q\n\n### Draft: T\n\nbody\nOpen: q\nConcierge: a note\n";
        assert_eq!(draft_trailer_refusals(guard).len(), 1);
    }

    #[test_id(t5nd2cx)]
    #[test]
    fn a_clean_resend_supersedes_a_draft_with_a_prose_depends_line() {
        let notes = "### Draft: T\n\nDepends: after the other\nbody\n\n### Draft: T\n\nDepends: none\nbody\n";
        assert!(draft_depends_refusals(notes).is_empty(), "{:?}", draft_depends_refusals(notes));
        let guard = "### Draft: T\n\nDepends: none\nbody\n\n### Draft: T\n\nDepends: after the other\nbody\n";
        assert_eq!(draft_depends_refusals(guard).len(), 1);
    }

    #[test_id(tw8k4pe)]
    #[test]
    fn a_no_progress_note_after_a_last_draft_is_not_a_trailer() {
        for np in [HELD, AFTER] {
            let notes = format!("### Draft: T\n\nbody\nOpen: q\n\n{np}");
            assert!(draft_trailer_refusals(&notes).is_empty(), "{:?}", draft_trailer_refusals(&notes));
            let more = format!("{notes}\nCorrection: x\n");
            let found = draft_trailer_refusals(&more);
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(found[0].contains("`Correction: x`"), "{found:?}");
        }
    }

    #[test_id(tj6c9ra)]
    #[test]
    fn a_no_progress_note_ends_a_draft_that_has_no_open_line() {
        let l = vec!["draft".to_string()];
        for np in [HELD, AFTER] {
            let notes = format!("### Draft: T\n\nbody line\nmore\n\n{np}");
            assert!(skuld_drafted("T", "body line\nmore", "draft", "sp-r", &l, &notes).is_ok());
            let got = draft_body(&notes, "T").expect("a draft");
            assert_eq!(got.body, "body line\nmore");
            assert_eq!(got.open, None);
        }
    }

    #[test_id(tz4h1vb)]
    #[test]
    fn a_heading_quoted_mid_line_does_not_start_a_draft() {
        let notes = "### Draft: Real\n\nsee the ### Draft: Fake heading\nOpen: q\nprose\n";
        let found = draft_trailer_refusals(notes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("draft \"Real\"") && !found[0].contains("Fake\":"), "{found:?}");
    }

    #[test_id(tb1y7ms)]
    #[test]
    fn drafts_of_two_titles_are_each_judged() {
        let notes = "### Draft: A\n\nx\nOpen: q\nprose a\n### Draft: B\n\ny\nOpen: q\nprose b\n";
        assert_eq!(draft_trailer_refusals(notes).len(), 2);
    }

    #[test_id(tui95nw)]
    #[test]
    fn skuld_drafted_names_the_line_that_follows_a_body() {
        let l = vec!["draft".to_string()];
        let notes = "### Draft: T\n\nbody\n\nCorrection to T: fixed.\n";
        let err = skuld_drafted("T", "body", "draft", "sp-r", &l, notes).unwrap_err();
        assert!(err.contains("continues with `Correction to"), "{err}");
        let err = skuld_drafted("T", "body", "draft", "sp-r", &l, "### Draft: T\n\nbody line\n").unwrap_err();
        assert!(err.contains("holds this body verbatim"), "{err}");
    }

    #[test_id(t7lt8f9)]
    #[test]
    fn skuld_drafted_passes_a_draft_followed_by_the_harness_submit_note() {
        let l = vec!["draft".to_string()];
        let notes = "### Draft: T\n\nbody line\nmore\n\nSubmitted: work committed on branch and marked submitted; the landing pass closes this bead when it lands, citing the merge commit. No attempt charged.\n";
        assert!(skuld_drafted("T", "body line\nmore", "draft", "sp-r", &l, notes).is_ok());
    }

    #[test_id(t3qvtzd)]
    #[test]
    fn skuld_drafted_refuses_an_edited_body_a_foreign_request_and_an_empty_body() {
        let l = vec!["draft".to_string()];
        let notes = "### Draft: T\n\nbody line\n";
        let edited = skuld_drafted("T", "body line EDITED", "draft", "sp-r", &l, notes).unwrap_err();
        assert!(edited.contains("--for skuld"));
        assert!(skuld_drafted("T", "body", "draft", "sp-r", &l, notes).is_err(), "a prefix of a line is not a match");
        let longer = "### Draft: T\n\na\nb\nc\n";
        assert!(skuld_drafted("T", "a\nb", "draft", "sp-r", &l, longer).is_err(), "a truncated body is not verbatim");
        let submitted = "### Draft: T\n\na\nb\n\nSubmitted: work committed on branch and marked submitted.\n";
        assert!(skuld_drafted("T", "a", "draft", "sp-r", &l, submitted).is_err(), "the submit note does not loosen verbatim matching");
        assert!(skuld_drafted("U", "body line", "draft", "sp-r", &l, notes).is_err());
        assert!(skuld_drafted("T", "body line", "draft", "sp-r", &[], notes).unwrap_err().contains("--for skuld"));
        assert!(skuld_drafted("T", "  \n", "draft", "sp-r", &l, notes).unwrap_err().contains("--for skuld"));
    }

    #[test_id(tv0u7k8)]
    #[test]
    fn draft_body_returns_the_body_of_a_matching_draft() {
        let d = draft_body("chatter\n### Draft: T\n\nline a\nline b\n", "T").unwrap();
        assert_eq!(d, Draft { body: "line a\nline b".into(), open: None });
    }

    #[test_id(tykymm6)]
    #[test]
    fn draft_body_takes_the_later_of_two_matching_notes() {
        let d = draft_body("### Draft: T\n\nold\n\n### Draft: T\n\nnew\n", "T").unwrap();
        assert_eq!(d.body, "new");
    }

    #[test_id(t7iv5gm)]
    #[test]
    fn draft_body_stops_at_an_open_line_and_returns_it() {
        let d = draft_body("### Draft: T\n\nbody\nOpen: which repo?; default: spira\nmore\n", "T").unwrap();
        assert_eq!(d, Draft { body: "body".into(), open: Some("Open: which repo?; default: spira".into()) });
    }

    #[test_id(t8jkas4)]
    #[test]
    fn draft_body_is_none_for_a_heading_that_differs_in_its_title() {
        assert_eq!(draft_body("### Draft: T two\n\nbody\n", "T"), None);
        assert_eq!(draft_body("### Draft: T\n\nbody\n", "T two"), None);
    }

    #[test_id(thkmwu6)]
    #[test]
    fn review_request_body_names_the_followup_and_the_verbatim_heading() {
        let b = review_request_body("sp-f1", "Fix the thing", "builder", "sp-b0", "## Build\nx\n");
        assert!(b.contains("sp-f1") && b.contains("sp-b0") && b.contains("builder"));
        assert!(b.contains("### Draft: Fix the thing\n\n"));
        assert!(b.contains("## The filer's text\n## Build\nx"));
    }

    const DESIGN: &str = "# Agent tools\n\n## Intent\n\nLet an agent save a card.\n\n## User path\n\n1. Sign in.\n2. Save a card.\n\n## Test strategy\n\nfoo\n";

    fn sv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(tqp61dp)]
    #[test]
    fn pilgrimage_after_takes_every_id_up_to_the_next_flag() {
        let p = pilgrimage_args(&sv(&["d.md", "--repo", "spira", "--after", "sp-a", "sp-b", "sp-c"])).unwrap();
        assert_eq!(p.after, sv(&["sp-a", "sp-b", "sp-c"]));
        assert_eq!(p.design, "d.md");
    }

    #[test_id(tfdrp16)]
    #[test]
    fn pilgrimage_after_repeated_accumulates() {
        let p = pilgrimage_args(&sv(&["d.md", "--repo", "r", "--after", "sp-a", "--after", "sp-b"])).unwrap();
        assert_eq!(p.after, sv(&["sp-a", "sp-b"]));
    }

    #[test_id(tkm5267)]
    #[test]
    fn pilgrimage_after_with_no_id_is_usage() {
        assert!(pilgrimage_args(&sv(&["d.md", "--after", "--repo", "r"])).unwrap_err().contains("usage"));
        assert!(pilgrimage_args(&sv(&["d.md", "--repo", "r", "--after"])).unwrap_err().contains("usage"));
    }

    #[test_id(tvn9yid)]
    #[test]
    fn pilgrimage_two_design_files_are_refused_naming_both() {
        let e = pilgrimage_args(&sv(&["a.md", "b.md", "--repo", "r", "--after", "sp-a"])).unwrap_err();
        assert!(e.contains("two design files: a.md and b.md"), "{e}");
    }

    #[test_id(t9twz39)]
    #[test]
    fn pilgrimage_spec_carries_intent_and_user_path() {
        let (title, body) = pilgrimage_spec(DESIGN).unwrap();
        assert_eq!(title, "Pilgrimage: Agent tools");
        assert!(body.contains("Let an agent save a card."));
        assert!(body.contains("2. Save a card."));
        assert!(body.contains("A clean walk"));
        assert!(!body.contains("foo"));
    }

    #[test_id(t3lt7id)]
    #[test]
    fn pilgrimage_spec_refuses_a_design_without_a_user_path() {
        let planted = DESIGN.replace("## User path", "## Walkthrough");
        let err = pilgrimage_spec(&planted).unwrap_err();
        assert!(err.contains("User path"), "{err}");
        let empty = DESIGN.replace("1. Sign in.\n2. Save a card.\n", "");
        assert!(pilgrimage_spec(&empty).is_err());
    }
    use super::*;

    // -- repositories (spira-config's own converter — see DESIGN.md "Decisions") --------

    // literal-ok: test fixture mirroring the lane vocabulary's shipped spelling
    const MIMIR: &str = "mimir-sweep";
    // literal-ok: test fixture mirroring schema.sh's shipped default for `no_loop`
    const NO_LOOP: &str = "no-loop";

    #[test_id(tb5yyjq)]
    #[test]
    fn appended_body_keeps_old_text_under_a_dated_separator() {
        let got = appended_body("old text\n", "new text\n", "2026-10-11");
        assert_eq!(got, "old text\n\n--- appended 2026-10-11 ---\n\nnew text\n");
    }

    #[test_id(tsvtbkb)]
    #[test]
    fn replaced_body_summary_names_length_and_head() {
        let got = replaced_body_summary("a\nb\nc\nd");
        assert!(got.contains("7 bytes") && got.contains("4 lines") && got.contains("a\n  b\n  c"));
        assert!(!got.contains("b\n  c\n  d"));
    }

    #[test_id(th8w2db)]
    #[test]
    fn parse_description_reads_array_and_object() {
        assert_eq!(parse_description(r#"[{"description":"x"}]"#).as_deref(), Some("x"));
        assert_eq!(parse_description(r#"{"id":"a"}"#).as_deref(), Some(""));
        assert_eq!(parse_description("nope"), None);
    }

    #[test_id(txlhi7c)]
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
                Lane::MimirSweep,
                Lane::CzarTrigger
            ]
        );
    }

    #[test_id(tj7ind2)]
    #[test]
    fn repos_by_name_empty_on_no_map() {
        assert!(repos_by_name("").is_empty());
    }

    #[test_id(tfarpoq)]
    #[test]
    fn repos_by_name_empty_map_when_any_row_names_an_unknown_lane() {
        // Fail closed (DESIGN.md "Observed but preserved"): one bad row makes every repo
        // read as unmapped, rather than only the row that named the bad lane.
        let map = "good | /tmp/g | push | origin/main | | true | plan\nbad  | /tmp/b | push | origin/main | | true | not-a-lane\n";
        assert!(repos_by_name(map).is_empty());
    }

    #[test_id(tlpqv03)]
    #[test]
    fn label_to_lane_round_trips_every_shipped_spelling() {
        for (label, lane) in [
            ("plan", Lane::Plan),
            ("incident", Lane::Incident),
            ("groom", Lane::Groom),
            (MIMIR, Lane::MimirSweep),
            ("spike", Lane::Spike),
            ("czar-trigger", Lane::CzarTrigger),
        ] {
            assert_eq!(label_to_lane(label), Some(lane));
            assert_eq!(lane_label(lane), label);
        }
        assert_eq!(label_to_lane("alpha-work"), None);
    }

    #[test_id(ty09bel)]
    #[test]
    fn rune_partition_last_match_wins() {
        assert_eq!(rune_partition("testscope,plan"), Some(Lane::Plan));
        assert_eq!(rune_partition("plan,incident"), Some(Lane::Incident));
        assert_eq!(rune_partition("testscope,alpha-work"), None);
        assert_eq!(rune_partition(""), None);
    }

    fn repo_map_fixture() -> BTreeMap<String, RepoSection> {
        repos_by_name(
            "dev-repo  | /tmp/dev  | push | origin/main | | true | develop\nself-repo | /tmp/self | push | origin/main | | true | self\n",
        )
    }

    #[test_id(txwf6ds)]
    #[test]
    fn lane_check_admits_self_repo_for_any_partition() {
        let repos = repo_map_fixture();
        let result = lane_check(&format!("testscope,{MIMIR}"), "self-repo", &repos, false);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test_id(tda1lzo)]
    #[test]
    fn lane_check_refuses_mimir_on_a_develop_repo() {
        let repos = repo_map_fixture();
        match lane_check(&format!("testscope,{MIMIR}"), "dev-repo", &repos, false) {
            LaneCheck::Refused(msg) => {
                assert!(msg.contains("dev-repo"));
                assert!(msg.contains(MIMIR));
                assert!(msg.contains("admitted lanes"));
                assert!(msg.contains("SPIRA_BEAD_LANE_OVERRIDE"));
            }
            LaneCheck::Admitted => panic!("expected a refusal"),
        }
    }

    #[test_id(trylqk6)]
    #[test]
    fn lane_check_override_admits_everything() {
        let repos = repo_map_fixture();
        let result = lane_check(&format!("testscope,{MIMIR}"), "dev-repo", &repos, true);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    #[test_id(tr33whz)]
    #[test]
    fn lane_check_plan_is_always_admitted() {
        let repos = repo_map_fixture();
        let result = lane_check("testscope,plan", "dev-repo", &repos, false);
        assert!(matches!(result, LaneCheck::Admitted));
    }

    // -- label composition ----------------------------------------------------------------

    #[test_id(t0bbs2l)]
    #[test]
    fn work_labels_appends_repo_and_never_express() {
        assert_eq!(work_labels("testscope,plan", "testrepo"), "testscope,plan,repo:testrepo");
    }

    #[test_id(tqlnj6t)]
    #[test]
    fn the_contract_fix_flag_adds_its_label_and_its_absence_changes_nothing() {
        let base = work_labels("testscope,plan", "testrepo");
        assert_eq!(with_contract_fix(base.clone(), false), base);
        assert_eq!(with_contract_fix(base, true), "testscope,plan,repo:testrepo,repo-contract-fix");
    }

    #[test_id(t99ud3x)]
    #[test]
    fn non_work_labels_event_has_no_partition() {
        let got = non_work_labels("testscope", None, None);
        assert_eq!(got, "testscope");
        assert!(!got.contains("plan"));
    }

    #[test_id(tn5r2ct)]
    #[test]
    fn idea_rows_are_open_only_newest_first() {
        let got = idea_rows(r#"[{"id":"sp-a","title":"old","status":"open","created_at":"2026-10-01T00:00:00Z"},{"id":"sp-b","title":"new","status":"open","created_at":"2026-10-20T00:00:00Z"},{"id":"sp-c","title":"gone","status":"closed","created_at":"2026-10-25T00:00:00Z"}]"#);
        assert_eq!(got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sp-b", "sp-a"]);
        assert!(idea_rows("not json").is_empty());
    }

    #[test_id(tpr56p7)]
    #[test]
    fn idea_kind_carries_the_idea_label_and_no_partition() {
        assert_eq!(non_work_labels("testscope", Some("idea"), Some("spira")), "testscope,idea,repo:spira");
    }

    #[test_id(tuarkk5)]
    #[test]
    fn non_work_labels_insight_carries_its_label() {
        let got = non_work_labels("testscope", Some("insight"), None);
        assert_eq!(got, "testscope,insight");
    }

    #[test_id(tzn9s3a)]
    #[test]
    fn non_work_labels_carry_the_repo() {
        let got = non_work_labels("testscope", None, Some("testrepo"));
        assert_eq!(got, "testscope,repo:testrepo");
    }

    // -- lint judge -------------------------------------------------------------------------

    // literal-ok: test fixture partition vocabulary, mirroring the bash suite's own PART
    const PART: &str = "plan mimir-sweep spike czar-trigger incident groom";

    #[test_id(tk8pj6e)]
    #[test]
    fn lint_judge_rows_match_the_bash_suite_table() {
        let add_one = format!("no partition label; add one or mark {NO_LOOP}");
        let with_no_loop = format!("repo:spira {NO_LOOP}");
        #[allow(clippy::type_complexity)]
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
            let (n, lines) = lint_judge(labels, *status == "open", ty, part, NO_LOOP); // literal-ok: the bash table's own column
            assert_eq!(n, *want_n, "labels={labels:?} status={status} type={ty}");
            let want: Vec<String> = want_lines.iter().map(|s| s.to_string()).collect();
            assert_eq!(lines, want, "labels={labels:?} status={status} type={ty}");
        }
    }

    /// sp-mve9i: the partition check runs for a bead the machine has waiting for a builder
    /// and for no other, whatever bd's status says.
    #[test_id(tnolkv2)]
    #[test]
    fn the_partition_check_follows_the_lifecycle_row_not_bd_status() {
        use spira_store::lc_state::Row;
        let row = |st: &str| Row { bead_id: "sp-a".into(), state: st.into(), ..Default::default() };
        for (st, want) in [("READY", 1), ("REWORK", 1), ("WORKING", 0), ("SUBMITTED", 0), ("LANDED", 0)] {
            let (n, _) = lint_judge("repo:spira", awaits_dispatch(Some(&row(st))), "task", PART, NO_LOOP);
            assert_eq!(n, want, "{st}");
        }
        assert!(!awaits_dispatch(None), "a bead with no row can never be claimed");
    }

    #[test_id(t7ohrz5)]
    #[test]
    fn lint_judge_no_loop_label_unset_has_no_add_one_suffix() {
        let (n, lines) = lint_judge("repo:spira", true, "task", PART, "");
        assert_eq!(n, 1);
        assert_eq!(lines, vec!["no partition label".to_string()]);
    }

    #[test_id(t7668gy)]
    #[test]
    fn runes_partitions_dedupes_and_excludes_scope() {
        let personas = vec![
            ("alpha".to_string(), "testscope,plan".to_string()),
            ("beta".to_string(), "testscope,incident".to_string()),
            ("gamma".to_string(), "plan".to_string()),
            ("empty".to_string(), "".to_string()),
        ];
        let got = runes_partitions(&personas, "testscope");
        assert_eq!(got, vec!["plan".to_string(), "incident".to_string()]);
    }

    // -- show/dep-list JSON -----------------------------------------------------------------

    #[test_id(tftkc11)]
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

    #[test_id(tbepon4)]
    #[test]
    fn priority_helpers() {
        assert_eq!(parse_priority(r#"[{"priority":2}]"#), Some(2));
        assert_eq!(parse_priority(r#"{"priority":0}"#), Some(0));
        assert_eq!(parse_priority("{}"), None);
        assert_eq!(parse_issue_type(r#"[{"issue_type":"decision"}]"#).as_deref(), Some("decision"));
        assert_eq!(parse_issue_type("{}"), None);
        for ty in ["decision", "event", "proposal"] {
            assert!(!carries_lifecycle_row(&format!(r#"[{{"issue_type":"{ty}"}}]"#)), "{ty}");
        }
        assert!(carries_lifecycle_row(r#"[{"issue_type":"task"}]"#));
        assert_eq!(parse_priority_arg("P1"), Some(1));
        assert_eq!(parse_priority_arg("4"), Some(4));
        assert_eq!(parse_priority_arg("5"), None);
        assert_eq!(parse_priority_arg("high"), None);
        assert_eq!(
            priority_note("ryan", Some(2), 1, Some("blocks the round")),
            "Priority changed P1 (was P2) by ryan. Reason: blocks the round"
        );
        assert_eq!(priority_note("ryan", None, 1, None), "Priority changed P1 (was unknown) by ryan.");
    }

    #[test_id(tfkd0fk)]
    #[test]
    fn a_build_spec_is_a_build_heading_with_content_under_it() {
        assert!(has_build_spec("Why.\n\n## Build\n\nA proxy in spira/foo.\n"));
        assert!(has_build_spec("## Build\nx\n## Acceptance\ny"));
        assert!(!has_build_spec("Outcome: no failed requests, whatever the mechanism."));
        assert!(!has_build_spec("## Build\n\n## Acceptance\ny"));
        assert!(!has_build_spec("## Build\n   \n"));
        assert!(!has_build_spec("Rebuild the ## Build step"));
    }

    #[test_id(tdgnk7j)]
    #[test]
    fn parse_show_row_none_on_garbage() {
        assert!(parse_show_row("not json").is_none());
    }

    #[test_id(t60tbd7)]
    #[test]
    fn parse_blocks_targets_reads_either_key() {
        let json = r#"[{"depends_on_id":"sp-a"},{"id":"sp-b"},{"depends_on_id":""}]"#;
        assert_eq!(
            parse_blocks_targets(json),
            vec!["sp-a".to_string(), "sp-b".to_string()]
        );
    }

    #[test_id(tw79efl)]
    #[test]
    fn relates_only_targets_drops_blockers_and_refuses_an_unreadable_list() {
        let blocks = vec!["sp-a".to_string()];
        let json = r#"[{"depends_on_id":"sp-a"},{"id":"sp-b"}]"#;
        assert_eq!(relates_only_targets(&blocks, json), Ok(vec!["sp-b".to_string()]));
        assert!(relates_only_targets(&blocks, "warning: slow").is_err(), "text is never an empty link list");
        assert!(relates_only_targets(&blocks, r#"{"error":"x"}"#).is_err());
    }

    #[test_id(t9pznv6)]
    #[test]
    fn parse_created_id_reads_silent_and_json_shapes() {
        assert_eq!(parse_created_id("sp-abc1\n").as_deref(), Some("sp-abc1"));
        assert_eq!(parse_created_id("{\"id\":\"sp-x\"}").as_deref(), Some("sp-x"));
        assert_eq!(parse_created_id("[{\"id\":\"sp-y\"}]").as_deref(), Some("sp-y"));
        assert_eq!(parse_created_id("created a thing"), None);
        assert_eq!(parse_created_id(""), None);
    }

    #[test_id(t3r2o06)]
    #[test]
    fn readback_tells_a_missing_bead_from_an_unreadable_store() {
        let since = "2026-11-13T10:00:00";
        let row = |id: &str, title: &str, at: &str| format!(r#"{{"id":"{id}","title":"{title}","created_at":"{at}"}}"#);
        let new = row("sp-a", "t", "2026-11-13T10:00:05.1Z");
        let old = row("sp-old", "t", "2026-11-01T10:00:05Z");
        assert_eq!(readback(0, &format!("[{new},{}]", row("sp-b", "u", "2026-11-13T10:00:06Z")), "t", since, None), Readback::Found("sp-a".into()));
        assert_eq!(readback(0, &format!("[{old},{new}]"), "t", since, None), Readback::Found("sp-a".into()), "an older bead of the same title is not this create");
        assert_eq!(readback(0, &format!("[{old}]"), "t", since, None), Readback::Absent);
        assert_eq!(readback(0, &format!("[{new}]"), "gone", since, None), Readback::Absent);
        assert_eq!(readback(0, &format!("[{new},{}]", row("sp-c", "t", "2026-11-13T10:00:09Z")), "t", since, None), Readback::Ambiguous);
        assert_eq!(readback(0, &format!("[{}]", row("sp-z", "t", "2026-11-13T10:00:09+02:00")), "t", since, None), Readback::Absent, "a zone it cannot compare is not proof");
        assert_eq!(readback(2, "", "t", since, None), Readback::Unreadable);
        assert_eq!(readback(2, "[]", "t", since, None), Readback::Unreadable);
        assert_eq!(readback(0, "{}", "t", since, None), Readback::Unreadable);
        let (req, item, other) = (row("sp-req", "t", "2026-11-13T10:00:05Z"), row("sp-item", "t", "2026-11-13T10:00:06Z"), row("sp-o", "t", "2026-11-13T10:00:07Z"));
        assert_eq!(readback(0, &format!("[{req}]"), "t", since, Some("sp-req")), Readback::Absent);
        assert_eq!(readback(0, &format!("[{req},{item}]"), "t", since, Some("sp-req")), Readback::Found("sp-item".into()));
        assert_eq!(readback(0, &format!("[{req},{item},{other}]"), "t", since, Some("sp-req")), Readback::Ambiguous);
        assert_eq!(readback(2, "[]", "t", since, Some("sp-req")), Readback::Unreadable);
    }

    #[test_id(tbefgmn)]
    #[test]
    fn parse_list_ids_excludes_events() {
        let json = r#"[{"id":"sp-a","issue_type":"task"},{"id":"sp-ev.1","issue_type":"event"},{"id":"sp-b","issue_type":"bug"}]"#;
        assert_eq!(
            parse_list_ids(json),
            vec!["sp-a".to_string(), "sp-b".to_string()]
        );
    }

    #[test_id(th58qik)]
    #[test]
    fn parse_list_ids_empty_on_garbage() {
        assert_eq!(parse_list_ids("nope"), Vec::<String>::new());
    }

    #[test_id(tywwn0g)]
    #[test]
    fn parse_blocks_targets_empty_on_garbage() {
        assert_eq!(parse_blocks_targets("nope"), Vec::<String>::new());
    }

    #[test_id(tlmik8x)]
    #[test]
    fn branch_label_finds_first_and_strips_spira_prefix() {
        let labels = vec!["repo:spira".to_string(), "branch:spira/sp-x".to_string()];
        let raw = branch_label(&labels).unwrap();
        assert_eq!(branch_candidate(raw), "sp-x");
    }

    // -- dep add ------------------------------------------------------------------------------

    #[test_id(t59lw6t)]
    #[test]
    fn is_blocks_type_covers_every_alias() {
        assert!(is_blocks_type(None));
        assert!(is_blocks_type(Some("blocks")));
        assert!(is_blocks_type(Some("blocked-by")));
        assert!(is_blocks_type(Some("depends-on")));
        assert!(!is_blocks_type(Some("relates-to")));
    }

    #[test_id(ttj64zy)]
    #[test]
    fn epic_blocks_refusal_names_parent_child() {
        let msg = epic_blocks_refusal("sp-a", "sp-e");
        assert!(msg.contains("--type parent-child"));
        assert!(msg.contains("sp-e"));
    }

    #[test_id(tdf5hij)]
    #[test]
    fn incident_blocks_refusal_names_the_alternative() {
        let msg = incident_blocks_refusal("sp-a", "sp-b", "incident-test");
        assert!(msg.contains("incident-test"));
        assert!(msg.contains("bd dep relate sp-a sp-b"));
    }

    // -- contract -------------------------------------------------------------------------

    #[test_id(tmw85sm)]
    #[test]
    fn persona_line_no_labels_fallback() {
        assert_eq!(
            persona_line("alpha", "testscope,alpha-work"),
            "  alpha          testscope,alpha-work"
        );
        assert_eq!(persona_line("beta", ""), "  beta           (no labels)");
    }

    #[test_id(tlv7rxz)]
    #[test]
    fn repos_section_lists_names_or_reports_absence() {
        let repos = repos_by_name("custrepo-one | /tmp/one | push | origin/main | | true | plan\n");
        assert_eq!(repos_section(&repos), "  custrepo-one\n");
        assert_eq!(repos_section(&BTreeMap::new()), "  (no repository map)\n");
    }

    #[test_id(t23ego9)]
    #[test]
    fn a_stack_base_must_be_live_same_repo_work() {
        let row = |ty: &str, st: &str, l: &str| ShowRow { labels: vec![l.into()], status: st.into(), issue_type: ty.into(), description: String::new(), notes: String::new(), parent: None };
        assert_eq!(stack_after_refusal("sp-a", Some(&row("task", "open", "repo:r")), false, "r"), None);
        assert!(stack_after_refusal("sp-a", None, false, "r").is_some());
        assert!(stack_after_refusal("sp-a", Some(&row("epic", "open", "repo:r")), false, "r").is_some());
        assert!(stack_after_refusal("sp-a", Some(&row("task", "open", "repo:r")), true, "r").is_some());
        assert!(stack_after_refusal("sp-a", Some(&row("task", "open", "repo:other")), false, "r").is_some());
    }

    // -- lint over a LintSource ----------------------------------------------------------

    use spira_store::lc_state::Row as LcRow;

    const INCIDENT: &str = "incident-test";
    const ASK: &str = "needs-op-test";
    const SPEC: &str = "Why.\n\n## Build\n\nThe change, in spira/x.\n";

    #[derive(Default)]
    struct Rows {
        beads: Vec<(String, ShowRow)>,
        blocks: Vec<(String, String)>,
        relates: Vec<(String, String)>,
        lc: Vec<(String, String)>,
        lc_err: Option<String>,
    }

    impl Rows {
        fn bead(mut self, id: &str, ty: &str, labels: &[&str], desc: &str) -> Self {
            let row = ShowRow {
                labels: labels.iter().map(|l| l.to_string()).collect(),
                status: "open".into(),
                issue_type: ty.into(),
                description: desc.into(),
                notes: String::new(),
                parent: None,
            };
            self.beads.push((id.into(), row));
            self.lc.push((id.into(), "READY".into()));
            self
        }
        fn state(mut self, id: &str, st: &str) -> Self {
            self.lc.retain(|(i, _)| i != id);
            self.lc.push((id.into(), st.into()));
            self
        }
        fn blocks(mut self, from: &str, to: &str) -> Self {
            self.blocks.push((from.into(), to.into()));
            self
        }
        fn relates(mut self, from: &str, to: &str) -> Self {
            self.relates.push((from.into(), to.into()));
            self
        }
    }

    fn edges(v: &[(String, String)], id: &str) -> Vec<String> {
        v.iter().filter(|(f, _)| f == id).map(|(_, t)| t.clone()).collect()
    }

    impl LintSource for Rows {
        fn ids(&self) -> Vec<String> {
            self.beads.iter().filter(|(_, r)| r.issue_type != "event").map(|(i, _)| i.clone()).collect()
        }
        fn show(&self, id: &str) -> Option<ShowRow> {
            self.beads.iter().find(|(i, _)| i == id).map(|(_, r)| r.clone())
        }
        fn blocks_targets(&self, id: &str) -> Option<Vec<String>> {
            Some(edges(&self.blocks, id))
        }
        fn relates_targets(&self, id: &str) -> Result<Vec<String>, String> {
            Ok(edges(&self.relates, id))
        }
        fn lifecycle(&self) -> Result<HashMap<String, LcRow>, String> {
            if let Some(e) = &self.lc_err {
                return Err(e.clone());
            }
            Ok(self
                .lc
                .iter()
                .map(|(id, st)| {
                    let row = LcRow {
                        bead_id: id.clone(),
                        state: st.clone(),
                        holder: None,
                        lease_until: None,
                        holds: vec![],
                        express: false,
                        stack: vec![],
                        phase: None,
                        disposition: None,
                        disposition_note: None,
                        version: None,
                        reason: None,
                        since: None,
                        ask_since: None,
                        builder_claims: None,
                    };
                    (id.clone(), row)
                })
                .collect())
        }
    }

    fn cfg() -> LintCfg {
        LintCfg {
            partitions: vec!["plan".into(), "incident-test".into()],
            spec_partitions: vec![vec!["plan".into()]],
            no_loop_label: NO_LOOP.into(),
            ask_label: ASK.into(),
            incident_label: INCIDENT.into(),
        }
    }

    /// Lints `ids` (or `--all` when empty) and returns the exit code and the joined lines.
    fn run(src: &Rows, ids: &[&str]) -> (i32, String) {
        struct Only<'a>(&'a Rows, Vec<String>);
        impl LintSource for Only<'_> {
            fn ids(&self) -> Vec<String> { self.1.clone() }
            fn show(&self, id: &str) -> Option<ShowRow> { self.0.show(id) }
            fn blocks_targets(&self, id: &str) -> Option<Vec<String>> { self.0.blocks_targets(id) }
            fn relates_targets(&self, id: &str) -> Result<Vec<String>, String> { self.0.relates_targets(id) }
            fn lifecycle(&self) -> Result<HashMap<String, LcRow>, String> { self.0.lifecycle() }
        }
        let (rc, lines) = if ids.is_empty() {
            lint(src, &cfg())
        } else {
            lint(&Only(src, ids.iter().map(|i| i.to_string()).collect()), &cfg())
        };
        (rc, lines.join("\n"))
    }

    #[test_id(tsbn7re)]
    #[test]
    fn a_failed_dep_list_is_unreadable_not_no_blockers() {
        struct DepsDown(Rows);
        impl LintSource for DepsDown {
            fn ids(&self) -> Vec<String> { self.0.ids() }
            fn show(&self, id: &str) -> Option<ShowRow> { self.0.show(id) }
            fn blocks_targets(&self, _: &str) -> Option<Vec<String>> { None }
            fn relates_targets(&self, _: &str) -> Result<Vec<String>, String> { Err(" failed".into()) }
            fn lifecycle(&self) -> Result<HashMap<String, LcRow>, String> { self.0.lifecycle() }
        }
        let src = DepsDown(Rows::default().bead("dep-a", "task", &["repo:spira", "plan"], SPEC));
        let (rc, lines) = lint(&src, &cfg());
        assert_eq!(rc, 1);
        assert_eq!(lines, vec!["bead: dep-a: unreadable (bd dep list failed)".to_string()]);
    }

    #[test_id(tl3m7ep)]
    #[test]
    fn a_failed_relates_list_on_an_incident_is_unreadable_with_the_original_text() {
        struct RelatesDown(Rows);
        impl LintSource for RelatesDown {
            fn ids(&self) -> Vec<String> { self.0.ids() }
            fn show(&self, id: &str) -> Option<ShowRow> { self.0.show(id) }
            fn blocks_targets(&self, id: &str) -> Option<Vec<String>> { self.0.blocks_targets(id) }
            fn relates_targets(&self, _: &str) -> Result<Vec<String>, String> { Err(" failed".into()) }
            fn lifecycle(&self) -> Result<HashMap<String, LcRow>, String> { self.0.lifecycle() }
        }
        let src = RelatesDown(Rows::default().bead("inc-a", "task", &["repo:spira", "plan", INCIDENT], SPEC));
        let (_, lines) = lint(&src, &cfg());
        assert!(lines.contains(&"bead: inc-a: unreadable (bd dep list --type relates-to failed)".to_string()), "{lines:?}");
    }

    fn mix() -> Rows {
        Rows::default()
            .bead("mix-claim", "task", &["repo:spira", "plan"], SPEC)
            .bead("mix-noloop", "task", &["repo:spira", NO_LOOP], "")
            .bead("mix-nopart", "task", &["repo:spira"], "")
            .bead("mix-norepo", "task", &["plan"], SPEC)
            .bead("mix-ev.1", "event", &[], "")
            .bead("mix-brgood", "task", &["repo:spira", "plan", "branch:spira/mix-brgood"], SPEC)
            .bead("mix-brbad", "task", &["repo:spira", "plan", "branch:spira/mix-brgood"], SPEC)
            .bead("spec-none", "task", &["repo:spira", "plan"], "A routine deploy causes no failed requests.")
            .bead("spec-empty", "task", &["repo:spira", "plan"], "Why.\n\n## Build\n\n## Acceptance\nx\n")
            .bead("spec-done", "task", &["repo:spira", "plan"], "")
            .state("spec-done", "LANDED")
            .bead("spec-noloop", "task", &["repo:spira", NO_LOOP], "x")
    }

    // covers: UC-dispatch-04
    #[test_id(t1a6fu8)]
    #[test]
    fn all_exits_1_when_offenders_are_present() {
        assert_eq!(run(&mix(), &[]).0, 1);
    }

    // covers: UC-dispatch-04
    #[test_id(txz6lu2)]
    #[test]
    fn all_reports_exactly_the_offenders() {
        let (_, out) = run(&mix(), &[]);
        for want in [
            "mix-nopart: no partition label",
            "mix-norepo: no repo: label",
            "mix-brbad: branch: label names mix-brgood, not itself",
            "spec-none: no build spec",
            "spec-empty: no build spec",
        ] {
            assert!(out.contains(want), "missing {want:?} in {out}");
        }
        for not in ["mix-claim", "mix-noloop", "mix-ev", "mix-brgood: branch:", "spec-done", "spec-noloop"] {
            assert!(!out.contains(not), "{not:?} reported in {out}");
        }
    }

    // covers: UC-dispatch-04
    #[test_id(td8wy4y)]
    #[test]
    fn a_builder_bead_without_a_build_spec_exits_1_alone_and_with_one_exits_0() {
        assert_eq!(run(&mix(), &["spec-none"]).0, 1);
        assert_eq!(run(&mix(), &["mix-claim"]).0, 0);
    }

    // covers: UC-dispatch-04
    #[test_id(thp0x7m)]
    #[test]
    fn an_unreadable_id_is_unreadable_not_a_label_defect() {
        let (rc, out) = run(&mix(), &["mix-nonexistent"]);
        assert_eq!(rc, 1);
        assert!(out.contains("mix-nonexistent: unreadable"), "{out}");
        assert!(!out.contains("no repo: label"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(thv5gys)]
    #[test]
    fn a_branch_label_naming_the_bead_itself_passes() {
        assert_eq!(run(&mix(), &["mix-brgood"]).0, 0);
    }

    // covers: UC-dispatch-04
    #[test_id(t33233x)]
    #[test]
    fn a_branch_label_naming_another_bead_exits_1_and_is_reported() {
        let (rc, out) = run(&mix(), &["mix-brbad"]);
        assert_eq!(rc, 1);
        assert!(out.contains("mix-brbad: branch: label names mix-brgood, not itself"), "{out}");
    }

    fn asks() -> Rows {
        Rows::default()
            .bead("ask-a", "decision", &[ASK], "")
            .bead("ask-b", "decision", &[ASK], "")
            .blocks("ask-b", "ask-a")
            .bead("ask-rel-a", "decision", &[ASK], "")
            .bead("ask-rel-b", "decision", &[ASK], "")
            .relates("ask-rel-b", "ask-rel-a")
            .bead("ask-work", "task", &["repo:spira", "plan"], SPEC)
            .bead("ask-target", "decision", &[ASK], "")
            .blocks("ask-target", "ask-work")
    }

    // covers: UC-dispatch-04
    #[test_id(txat8sj)]
    #[test]
    fn a_blocks_edge_between_two_asks_is_flagged() {
        let (rc, out) = run(&asks(), &[]);
        assert_eq!(rc, 1);
        assert!(out.contains("ask-b: blocks edge to ask-labelled ask-a"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(t6w3cug)]
    #[test]
    fn relates_to_between_asks_and_a_work_bead_under_an_ask_are_not_flagged() {
        let (_, out) = run(&asks(), &[]);
        assert!(!out.contains("ask-rel-b: blocks edge"), "{out}");
        assert!(!out.contains("ask-target: blocks edge"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(tr0kdv9)]
    #[test]
    fn the_non_blocking_end_of_a_flagged_ask_edge_passes_alone() {
        assert_eq!(run(&asks(), &["ask-a"]).0, 0);
    }

    fn incidents() -> Rows {
        Rows::default()
            .bead("inc-alarm", "task", &[INCIDENT, "repo:spira", NO_LOOP], "")
            .bead("inc-work", "task", &["repo:spira", "plan"], SPEC)
            .blocks("inc-work", "inc-alarm")
            .bead("inc-rel", "task", &["repo:spira", "plan"], SPEC)
            .relates("inc-rel", "inc-alarm")
            .bead("work-a", "task", &["repo:spira", "plan"], SPEC)
            .bead("work-b", "task", &["repo:spira", "plan"], SPEC)
            .blocks("work-b", "work-a")
    }

    // covers: UC-dispatch-04
    #[test_id(tdqh41a)]
    #[test]
    fn a_blocks_edge_onto_an_incident_is_flagged_by_the_configured_label() {
        let (rc, out) = run(&incidents(), &[]);
        assert_eq!(rc, 1);
        assert!(out.contains("inc-work: blocks edge to incident-labelled inc-alarm"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(t23rj3x)]
    #[test]
    fn relates_to_an_incident_and_work_onto_work_are_not_flagged() {
        let (_, out) = run(&incidents(), &[]);
        assert!(!out.contains("inc-rel: blocks edge"), "{out}");
        assert!(!out.contains("work-b: blocks edge"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(t55ov43)]
    #[test]
    fn the_alarm_alone_and_work_b_alone_pass() {
        assert_eq!(run(&incidents(), &["inc-alarm"]).0, 0);
        assert_eq!(run(&incidents(), &["work-b"]).0, 0);
    }

    fn remedies() -> Rows {
        Rows::default()
            .bead("rem-bad", "task", &[INCIDENT, "repo:spira", NO_LOOP], "")
            .bead("rem-fix1", "task", &["repo:spira", "plan"], SPEC)
            .relates("rem-bad", "rem-fix1")
            .bead("rem-good", "task", &[INCIDENT, "repo:spira", NO_LOOP], "")
            .bead("rem-fix2", "task", &["repo:spira", "plan"], SPEC)
            .blocks("rem-good", "rem-fix2")
            .relates("rem-good", "rem-fix2")
            .bead("rem-done", "task", &[INCIDENT, "repo:spira", NO_LOOP], "")
            .bead("rem-fix3", "task", &["repo:spira", "plan"], "")
            .state("rem-fix3", "LANDED")
            .relates("rem-done", "rem-fix3")
    }

    // covers: UC-dispatch-04
    #[test_id(tzppr95)]
    #[test]
    fn an_open_remedy_linked_relates_to_only_is_flagged() {
        let (rc, out) = run(&remedies(), &["rem-bad"]);
        assert_eq!(rc, 1);
        assert!(out.contains("rem-bad: open remedy rem-fix1 is linked relates-to only"), "{out}");
    }

    // covers: UC-dispatch-04
    #[test_id(t82zjuh)]
    #[test]
    fn a_blocks_linked_remedy_passes() {
        assert_eq!(run(&remedies(), &["rem-good"]).0, 0);
    }

    // covers: UC-dispatch-04
    #[test_id(tjwadnc)]
    #[test]
    fn a_relates_to_remedy_that_already_landed_passes() {
        assert_eq!(run(&remedies(), &["rem-done"]).0, 0);
    }

    // covers: UC-dispatch-04
    #[test_id(t41yi2u)]
    #[test]
    fn filing_a_remedy_under_an_incident_adds_the_blocks_edge() {
        let labels = vec![INCIDENT.to_string(), "repo:spira".to_string()];
        assert_eq!(remedy_edge(&labels, INCIDENT, "sp-new", "sp-inc"), Some(("sp-inc".to_string(), "sp-new".to_string())));
    }

    // covers: UC-dispatch-04
    #[test_id(t3x4cn9)]
    #[test]
    fn filing_under_an_ordinary_parent_adds_no_edge() {
        let labels = vec!["repo:spira".to_string(), "plan".to_string()];
        assert_eq!(remedy_edge(&labels, INCIDENT, "sp-new", "sp-par"), None);
    }

    // covers: UC-dispatch-04
    #[test_id(t72yxeq)]
    #[test]
    fn an_unreadable_lifecycle_state_fails_the_partition_check_and_is_said_once() {
        let mut rows = mix();
        rows.lc_err = Some("down".into());
        let (rc, out) = run(&rows, &[]);
        assert_eq!(rc, 1);
        assert_eq!(out.matches("lifecycle state unreadable").count(), 1, "{out}");
    }
}
