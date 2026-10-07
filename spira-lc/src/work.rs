//! The aeon semantic layer (design §3.5): show/note/submit/done/blocked/file-followup/
//! split/superseded-by, dispatched from the same socket and `Conn` as show/list/history/
//! event. This dispatch is itself generic — it acts on whatever bead id its first
//! argument names, like every other spira-lc verb — because binding to one summoned bead
//! is the `work` client crate's job (a separate, thinner binary that never links a direct
//! DB connection at all), not this service's. See main.rs's `dispatch` for the socket path
//! this shares with `show`/`list`/`history`/`event`.
//!
//! Deploys inert: nothing calls `work` over the socket yet.

use lifecycle::bead::{self, BeadEventKind, HoldKind};
use lifecycle::reason::HoldCause;

use crate::db::Conn;
use crate::{apply_bead_event, flag, rows, CANNOT_TELL, REFUSED};

pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "work: missing <bead-id>".into());
    };
    let Some(verb) = args.get(1) else {
        return (CANNOT_TELL, "work: missing <verb>".into());
    };
    let rest = &args[2..];
    // The persona gate (sp-st0mm): every verb, before anything it does.
    if let Err(refusal) = gate(verb, rest) {
        return refusal;
    }
    match verb.as_str() {
        "show" => cmd_show(bead_id, conn),
        "note" => cmd_note(bead_id, rest),
        "submit" => cmd_submit(bead_id, rest, conn),
        "done" => cmd_done(bead_id, rest, conn),
        "blocked" => cmd_blocked(bead_id, rest, conn),
        "file-followup" => cmd_file(bead_id, rest, "file-followup", conn),
        "split" => cmd_file(bead_id, rest, "split", conn),
        "superseded-by" => cmd_superseded_by(bead_id, rest, conn),
        "fence" => cmd_fence(rest),
        other if LANE_VERBS.contains(&other) => cmd_lane(bead_id, other, rest, conn),
        other => (
            CANNOT_TELL,
            format!("work: unknown verb {other:?} (want show, note, submit, done, blocked, file-followup, split, superseded-by, {})", LANE_VERBS.join(", ")),
        ),
    }
}

fn require_actor(args: &[String]) -> Result<String, (i32, String)> {
    flag(args, "--actor").ok_or((CANNOT_TELL, "work: --actor is required".to_string()))
}

fn cmd_show(bead_id: &str, conn: &Conn) -> (i32, String) {
    let text = match crate::bd::show(bead_id) {
        Ok(text) => text,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match crate::cmd_show(&[bead_id.to_string()], conn) {
        (0, row) => (0, with_lifecycle(&text, &row)),
        (1, _) => (0, format!("{text}\nLIFECYCLE: no row\n")),
        (_, e) => (CANNOT_TELL, e),
    }
}

/// bd's `show` text with its status word replaced by the lifecycle state, then the row itself:
/// a session reads the machine's state, never bd's status, as where the bead stands.
fn with_lifecycle(bd_text: &str, show_json: &str) -> String {
    let row: serde_json::Value = serde_json::from_str(show_json).unwrap_or_default();
    let bead = &row["bead"];
    let state = bead["state"].as_str().unwrap_or("?");
    let mut lines: Vec<String> = bd_text.lines().map(str::to_string).collect();
    if let Some(head) = lines.first_mut() {
        if let (Some(dot), true) = (head.rfind(" · "), head.ends_with(']')) {
            head.replace_range(dot + " · ".len().., &format!("{state}]"));
        }
    }
    let field = |k: &str| bead[k].as_str().filter(|v| !v.is_empty()).map(|v| format!("  {k}: {v}\n")).unwrap_or_default();
    format!(
        "{}\n\nLIFECYCLE: {state}\n{}{}{}{}{}",
        lines.join("\n"),
        field("holds"),
        field("reason"),
        field("holder"),
        field("tip"),
        row["delivery"]["state"].as_str().map(|d| format!("  delivery: {d}\n")).unwrap_or_default(),
    )
}

fn cmd_note(bead_id: &str, args: &[String]) -> (i32, String) {
    let Some(text) = args.first() else {
        return (CANNOT_TELL, "work note: missing <text>".into());
    };
    // `work note -`: the text arrived on the client's stdin, carried as `--stdin` (prose
    // belongs on stdin — law-commit-messages-via-stdin).
    if text == "-" {
        let call = split_reserved(args);
        let Some(body) = call.stdin else {
            return (CANNOT_TELL, "work note: `-` given but no text arrived on stdin".into());
        };
        return match crate::bd::run_stdin(&[s("note"), bead_id.to_string(), s("--stdin")], Some(&body)) {
            Ok(out) => (0, out),
            Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
        };
    }
    match crate::bd::note(bead_id, text) {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn cmd_submit(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(tip) = flag(args, "--tip") else {
        return (CANNOT_TELL, "work submit: --tip is required (the client reads it from the worktree; the verb itself never takes one)".into());
    };
    let row = match rows::fetch_bead(conn, bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work submit: no bead row for {bead_id}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if !row.stack.is_empty() {
        let ids: Vec<&String> = row.stack.keys().collect();
        let prereqs = match ids.iter().map(|id| rows::fetch_bead(conn, id)).collect::<Result<Vec<_>, _>>() {
            Ok(rs) => rs.into_iter().flatten().collect::<Vec<_>>(),
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
        };
        if let Some(refusal) = bead::submit_refusal_for_stale_stack(&row, &prereqs) {
            let lifecycle::Refusal::StackStale { prereqs: stale } = &refusal else { unreachable!() };
            return (
                REFUSED,
                format!(
                    "refused: base withdrawn on {} — rebase {bead_id} onto its current stack before resubmitting",
                    stale.join(", ")
                ),
            );
        }
    }
    apply_bead_event(conn, bead_id, &actor, BeadEventKind::Submit { tip })
}

fn cmd_done(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(delivers) = flag(args, "--delivers") else {
        return (CANNOT_TELL, "work done: --delivers <path> is required".into());
    };
    apply_bead_event(conn, bead_id, &actor, BeadEventKind::Done { delivers })
}

fn cmd_blocked(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(question) = args.first() else {
        return (CANNOT_TELL, "work blocked: missing \"<question>\"".into());
    };
    let Some(default) = flag(args, "--default") else {
        return (CANNOT_TELL, "work blocked: --default <default> is required".into());
    };
    let (code, out) = apply_bead_event(
        conn,
        bead_id,
        &actor,
        BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some(question.clone()) },
    );
    if code != 0 {
        return (code, out);
    }
    // mail's own "question" kind requires a filled "## Question" and "## Default"
    // section in the body (every "## " heading in its template is a required section,
    // not just the X-Spira-Default header) — a bare question string is refused.
    let body = format!("## Question\n{question}\n\n## Default\n{default}\n");
    match crate::bd::ask_operator(&format!("{actor} <{actor}@spira>"), question, &default, bead_id, &body) {
        Ok(_) => (0, format!("hold applied; ask filed for {bead_id}")),
        Err(e) => (CANNOT_TELL, format!("hold applied, but filing the ask failed: {e}")),
    }
}

fn cmd_file(bead_id: &str, args: &[String], verb: &str, conn: &Conn) -> (i32, String) {
    let Some(title) = args.first() else {
        return (CANNOT_TELL, format!("work {verb}: missing <title>"));
    };
    let Some(persona) = flag(args, "--for") else {
        return (CANNOT_TELL, format!("work {verb}: --for <persona> is required"));
    };
    let repo = match crate::bd::repo_label(bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work {verb}: {bead_id} carries no repo: label")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match crate::bd::file_child(title, &persona, &repo, bead_id) {
        Ok(new_id) => match crate::cutover::cmd_create_bead(&[new_id.clone()], conn) {
            (0, _) => (0, new_id),
            (_, e) => (
                CANNOT_TELL,
                format!("LIFECYCLE: {new_id} was filed but has NO lifecycle row and cannot be claimed (do not file it again): {e}"),
            ),
        },
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn cmd_superseded_by(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(successor) = args.first() else {
        return (CANNOT_TELL, "work superseded-by: missing <successor-id>".into());
    };
    // Not lifecycle::bead::BeadEventKind::Supersede: that is the authoritative, terminal
    // transition, reserved for the groomer or operator's own confirmation (design §3.5 —
    // "a supersede request, confirmed by the groomer or operator"). The aeon's own request
    // is a hold plus an ask; nothing here moves the bead to SUPERSEDED.
    let (code, out) = apply_bead_event(
        conn,
        bead_id,
        &actor,
        BeadEventKind::Hold { kind: HoldKind::Operator, cause: HoldCause::SupersedeRequest, detail: Some(successor.clone()) },
    );
    if code != 0 {
        return (code, out);
    }
    // mail refuses a Subject that leads with a bead id — the id belongs in --bead,
    // which this call already carries. Same "## Question"/"## Default" requirement as
    // cmd_blocked, from mail's "question" kind template.
    let subject = format!("superseded-by {successor}?");
    let question = format!("{bead_id} requests confirmation that it is superseded by {successor}.");
    let body = format!("## Question\n{question}\n\n## Default\n{successor}\n");
    match crate::bd::ask_operator(&format!("{actor} <{actor}@spira>"), &subject, successor, bead_id, &body) {
        Ok(_) => (0, format!("hold applied; supersede-by-{successor} ask filed for {bead_id}")),
        Err(e) => (CANNOT_TELL, format!("hold applied, but filing the ask failed: {e}")),
    }
}

// ---- the lane verbs and the persona gate (sp-st0mm) --------------------------------------
//
// Operator order 2026-10-05: no aeon model runs `bd`. Everything a persona legitimately does
// to the graph — including to beads OTHER than the one it was summoned for (a groomer closes
// duplicates, ops adds a dependency, czar ejects a member) — is a named verb here, and this
// broker is the only thing that calls bd or a bd-calling tool on its behalf. Each verb is one
// narrow operation with its arguments validated; there is deliberately no "run this bd
// command" passthrough, because that would re-open exactly the hole this closes.
//
// Who may run what is ONE table, [`ALLOW`]. The caller's persona is the `--actor` the `work`
// client appends from its own environment (`SPIRA_FAYTH`, else `SPIRA_WORK_ACTOR`); the
// client refuses a user-typed `--actor`; serve binds it to the peer's uid ([`bind_peer`]) for
// every uid listed in `<socket>.personas`, since the environment is the caller's to set.

/// Every verb that is not bound to the summoned bead. The bound verbs above act on the
/// bead the client is bound to; these name their target (or none) themselves.
pub const LANE_VERBS: &[&str] = &[
    "ask", "read", "list", "search", "note-on", "label-add", "label-remove", "dep-add", "relate", "reopen", "close-other", "file", "groom", "incident",
    "sop", "census", "queue", "landing-pass", "strand", "fence",
];

const BOUND_VERBS: &[&str] = &["show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by"];

/// The tool verbs: `work <verb> <sub> ...` runs `<program> <sub> ...` — a harness tool that
/// itself reaches bd, with `<sub>` one of the operations [`ALLOW`] names for it. `census`
/// takes no subcommand.
const TOOLS: &[(&str, &str)] = &[
    ("groom", "groomer"),
    ("incident", "incident.sh"),
    ("sop", "sop"),
    ("census", "census"),
    ("queue", "queue"),
    ("landing-pass", "landing-pass"),
    ("strand", "strand"),
];

/// A tool call's wall: a groomer sweep or a queue step reads the whole graph, so this is the
/// tool's own runtime bound, not a query deadline.
// batch-job: a broker-run harness tool (groomer sweep, queue step), bounded at 300 s.
const TOOL_SECS: u64 = 300;

const ANY: &[&str] = &["*"];

/// THE allow table: operation → the personas that may run it (`*`: every persona). An
/// operation absent from this table is refused for everyone. A tool verb's operation is
/// `"<verb> <sub>"`.
pub const ALLOW: &[(&str, &[&str])] = &[
    // Bound to the summoned bead: every persona acts on its own bead.
    ("show", ANY),
    ("note", ANY),
    ("submit", ANY),
    ("done", ANY),
    ("blocked", ANY),
    ("file-followup", ANY),
    ("split", ANY),
    ("superseded-by", ANY),
    // Reads and the operator mailbox.
    ("ask", ANY),
    ("read", ANY),
    ("list", ANY),
    ("search", ANY),
    // Writes to other beads.
    ("note-on", &["groomer", "czar", "maechen", "ops", "batcher", "archivist", "warden"]),
    ("label-add", &["groomer", "batcher", "maechen"]),
    ("label-remove", &["groomer", "batcher"]),
    ("dep-add", &["ops", "groomer", "batcher", "czar", "warden"]),
    ("relate", &["archivist", "groomer"]),
    ("reopen", &["groomer", "czar", "maechen"]),
    ("close-other", &["czar"]),
    ("file", &["ops", "spike", "archivist", "maechen", "czar", "groomer", "batcher", "warden"]),
    // Tools.
    ("groom sweep", &["groomer"]),
    ("groom split-piece", &["groomer"]),
    ("groom supersede", &["groomer"]),
    ("groom close", &["groomer"]),
    ("groom correct-lane", &["groomer"]),
    ("groom depends-on-fix", &["groomer"]),
    ("groom unpoison", &["groomer"]),
    ("groom triage-poison", &["groomer"]),
    ("groom deadlocked", &["groomer"]),
    ("groom unwanted", &["groomer"]),
    ("incident list", &["ops", "czar"]),
    ("incident file", &["ops", "czar"]),
    ("incident collapse", &["ops", "czar"]),
    ("sop match", &["ops"]),
    ("sop show", &["ops"]),
    ("sop list", &["ops"]),
    ("sop log", &["ops"]),
    ("sop applied", &["ops"]),
    ("sop write", &["ops"]),
    ("census", &["maechen"]),
    ("queue stats", &["czar"]),
    ("queue step", &["czar"]),
    ("queue eject", &["czar"]),
    ("queue abandon", &["czar"]),
    ("landing-pass halt", &["czar"]),
    ("strand report", &["czar", "groomer"]),
    ("strand detect-livelocked", &["groomer"]),
    ("fence", &["czar"]),
];

/// A request's own arguments with the client's reserved trailer split off: `... [--stdin
/// <text>] --actor <persona>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub actor: Option<String>,
    actors: usize,
}

pub fn split_reserved(rest: &[String]) -> Call {
    let mut args = rest.to_vec();
    let (mut actor, mut stdin) = (None, None);
    if args.len() >= 2 && args[args.len() - 2] == "--actor" {
        actor = args.pop();
        args.pop();
    }
    if args.len() >= 2 && args[args.len() - 2] == "--stdin" {
        stdin = args.pop();
        args.pop();
    }
    let actors = rest.iter().filter(|a| *a == "--actor").count();
    Call { args, stdin, actor, actors }
}

/// `<uid> <persona>` lines (`#` comments): the personas whose own OS account is the identity.
pub fn parse_persona_uids(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            let mut it = l.split_whitespace();
            let (uid, persona) = (it.next()?.parse().ok()?, it.next()?.to_string());
            it.next().is_none().then_some((uid, persona))
        })
        .collect()
}

/// Binds a `work` request's persona to the connecting process's uid. A uid in `map` IS its
/// persona: a different `--actor` is refused, never overridden, and an absent one is filled
/// in. A uid not in `map` keeps the client-sent `--actor` (nothing else identifies it).
pub fn bind_peer(argv: &[String], peer_uid: Option<u32>, map: &[(u32, String)]) -> Result<Vec<String>, (i32, String)> {
    if argv.first().map(String::as_str) != Some("work") || map.is_empty() {
        return Ok(argv.to_vec());
    }
    let Some(uid) = peer_uid else {
        return Err((REFUSED, "refused: the caller's uid could not be read from the socket, and personas are bound to uids here".into()));
    };
    let Some((_, persona)) = map.iter().find(|(u, _)| *u == uid) else {
        return Ok(argv.to_vec());
    };
    let sent = argv.iter().filter(|a| *a == "--actor").count();
    let mut out = argv.to_vec();
    match sent {
        0 => out.extend(["--actor".to_string(), persona.clone()]),
        1 if out.len() >= 2 && out[out.len() - 2] == "--actor" => {
            if out[out.len() - 1] != *persona {
                return Err((REFUSED, format!("refused: uid {uid} is persona {persona}, not {}", out[out.len() - 1])));
            }
        }
        _ => return Err((REFUSED, "refused: work: --actor is the client's trailer alone, never the caller's".into())),
    }
    Ok(out)
}

/// The operation key [`ALLOW`] is read with.
pub fn op_key(verb: &str, args: &[String]) -> String {
    match TOOLS.iter().find(|(v, _)| *v == verb) {
        Some(_) if verb != "census" => format!("{verb} {}", args.first().map(String::as_str).unwrap_or("")),
        _ => verb.to_string(),
    }
}

/// May `actor` run `op`? `Err` is the refusal to answer with.
pub fn permitted(op: &str, actor: Option<&str>) -> Result<(), (i32, String)> {
    let Some((_, who)) = ALLOW.iter().find(|(k, _)| *k == op) else {
        return Err((REFUSED, format!("refused: `work {op}` is not an operation any persona may run")));
    };
    if who.contains(&"*") {
        return Ok(());
    }
    let Some(actor) = actor else {
        return Err((REFUSED, format!("refused: `work {op}` needs the caller's persona (--actor) and none was sent")));
    };
    if who.contains(&actor) {
        Ok(())
    } else {
        Err((REFUSED, format!("refused: persona {actor} may not run `work {op}` (allowed: {})", who.join(", "))))
    }
}

fn gate(verb: &str, rest: &[String]) -> Result<(), (i32, String)> {
    if !BOUND_VERBS.contains(&verb) && !LANE_VERBS.contains(&verb) {
        return Ok(()); // dispatch's own "unknown verb" answer
    }
    let call = split_reserved(rest);
    if call.actors > 1 {
        return Err((REFUSED, format!("refused: work {verb}: more than one --actor — the persona is the client's to send, never the caller's")));
    }
    permitted(&op_key(verb, &call.args), call.actor.as_deref())
}

/// One thing a lane verb does: a bd call, a harness tool, — after a `bead.sh file` for a
/// persona — the lifecycle row for the id it printed, or — after a bound session's
/// question reached the operator — the `ask` hold on the asking session's own bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Bd { args: Vec<String>, stdin: Option<String> },
    /// `work list`: the lifecycle rows in these states (every non-terminal state when empty,
    /// every state with `all`), joined to bd's content rows filtered by `bd_args`.
    LcList { states: Vec<String>, all: bool, bd_args: Vec<String>, json: bool, limit: Option<usize> },
    Tool { program: &'static str, args: Vec<String>, stdin: Option<String> },
    CreateRow,
    /// `Hold { Ask, OperatorQuestion }` on `bead`: the bead waits on the operator's answer
    /// (only a `Reply` or `AskWithdrawn` lifts it), and aeon's teardown reads the hold to
    /// release the session operator-wait, uncharged (sp-v62vn follow-up: it replaced the
    /// `<bead>.operator-wait` marker `mail` wrote, which a restricted model cannot run).
    AskHold { bead: String, question: String },
    /// `mail send operator ...` for `work ask`, run with `BEAD_ID=<classify_as>`: mail's
    /// escalation-class check (`mail::cmds::class_refusal`) engages only when BEAD_ID is set —
    /// the old direct-mail path had it from the aeon's own environment, the broker's process
    /// does not — so without it an unclassed question went straight to the operator. With it,
    /// an ask lacking a PERMISSIONS/POLICY/DESTRUCTIVE `--class` and a `## Class basis` is
    /// routed to the concierge mailbox (law-escalate-decisions-not-problems).
    Ask { args: Vec<String>, stdin: String, classify_as: String },
}

fn s(x: &str) -> String {
    x.to_string()
}

fn usage(verb: &str, msg: &str) -> (i32, String) {
    (CANNOT_TELL, format!("work {verb}: {msg}"))
}

pub(crate) fn is_bead_id(x: &str) -> bool {
    x.len() > 3 && x[..3].eq_ignore_ascii_case("sp-") && x[3..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

fn bead_arg(verb: &str, args: &[String], i: usize, what: &str) -> Result<String, (i32, String)> {
    match args.get(i) {
        Some(id) if is_bead_id(id) => Ok(id.clone()),
        Some(other) => Err(usage(verb, &format!("{what} must be a bead id, not {other:?}"))),
        None => Err(usage(verb, &format!("missing {what}"))),
    }
}

/// `--flag value` pairs and bare switches from `args[from..]`, every token checked against
/// `values`/`switches`; anything else is refused rather than forwarded.
fn parse_flags(verb: &str, args: &[String], from: usize, values: &[&str], switches: &[&str]) -> Result<Vec<(String, Option<String>)>, (i32, String)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < args.len() {
        let a = args[i].as_str();
        if values.contains(&a) {
            let Some(v) = args.get(i + 1) else { return Err(usage(verb, &format!("{a} needs a value"))) };
            out.push((a.to_string(), Some(v.clone())));
            i += 2;
        } else if switches.contains(&a) {
            out.push((a.to_string(), None));
            i += 1;
        } else {
            return Err(usage(verb, &format!("unexpected argument {a:?}")));
        }
    }
    Ok(out)
}

fn get<'a>(flags: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
    flags.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_deref())
}

fn flatten(flags: &[(String, Option<String>)]) -> Vec<String> {
    flags.iter().flat_map(|(k, v)| std::iter::once(k.clone()).chain(v.clone())).collect()
}

/// The mail display name a persona sends as (`From: <name> <persona@spira>`).
pub fn display_name(actor: &str) -> String {
    match actor {
        "batcher" => "Judge".to_string(),
        _ => {
            let mut c = actor.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

/// The label a lane verb never adds or removes: poison is lifted only by `groom unpoison`
/// with its cause and evidence (sp-pl7zg's `spira-claim unpoison` discipline), never by a
/// bare label edit.
const POISON_LABEL: &str = "spira-poison";

/// What a lane verb does, decided from its arguments alone — no I/O, so every rule below is
/// covered without a fixture. `bound` is the client's summoned bead (`-` when unbound).
pub fn plan(verb: &str, bound: &str, call: &Call) -> Result<Vec<Step>, (i32, String)> {
    let a = &call.args;
    let actor = call.actor.as_deref().unwrap_or("aeon");
    let bd = |args: Vec<String>| Step::Bd { args, stdin: None };
    match verb {
        "read" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &[], &["--json"])?;
            Ok(vec![bd([vec![s("show"), id], flatten(&f)].concat())])
        }
        "list" => {
            if a.iter().any(|x| x == "--status") {
                return Err(usage(verb, "--status is bd's status; a bead's state is the lifecycle's — use --state <STATE[,STATE]>"));
            }
            let f = parse_flags(verb, a, 0, &["--state", "--label", "--title-contains", "--limit"], &["--json", "--all"])?;
            let mut states = Vec::new();
            for st in get(&f, "--state").into_iter().flat_map(|v| v.split(',')) {
                let up = st.to_ascii_uppercase();
                if bead::BeadState::from_str(&up).is_none() {
                    return Err(usage(verb, &format!("--state {st:?} is not a lifecycle state")));
                }
                states.push(up);
            }
            let limit = match get(&f, "--limit") {
                Some(n) => Some(n.parse::<usize>().map_err(|_| usage(verb, &format!("--limit {n:?} is not a number")))?),
                None => None,
            };
            let mut bd_args = Vec::new();
            for k in ["--label", "--title-contains"] {
                if let Some(v) = get(&f, k) {
                    bd_args.extend([s(k), v.to_string()]);
                }
            }
            Ok(vec![Step::LcList { states, all: f.iter().any(|(k, _)| k == "--all"), bd_args, json: f.iter().any(|(k, _)| k == "--json"), limit }])
        }
        "search" => {
            let Some(q) = a.first().filter(|q| !q.is_empty() && !q.starts_with('-')) else {
                return Err(usage(verb, "missing <query>"));
            };
            let f = parse_flags(verb, a, 1, &["--limit"], &["--json"])?;
            Ok(vec![bd([vec![s("search"), q.clone()], flatten(&f)].concat())])
        }
        "note-on" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            match (a.get(1).map(String::as_str), a.len()) {
                (Some("-"), 2) => match &call.stdin {
                    Some(text) => Ok(vec![Step::Bd { args: vec![s("note"), id, s("--stdin")], stdin: Some(text.clone()) }]),
                    None => Err(usage(verb, "`-` given but no text arrived on stdin")),
                },
                (Some(text), 2) if !text.is_empty() => Ok(vec![bd(vec![s("note"), id, text.to_string()])]),
                _ => Err(usage(verb, "want <bead-id> \"<text>\" (or - for stdin)")),
            }
        }
        "label-add" | "label-remove" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let Some(label) = a.get(1).filter(|_| a.len() == 2) else {
                return Err(usage(verb, "want <bead-id> <label>"));
            };
            if label.is_empty() || label.starts_with('-') || label.contains(',') || label.chars().any(char::is_whitespace) {
                return Err(usage(verb, &format!("{label:?} is not one label")));
            }
            if label == POISON_LABEL {
                return Err((REFUSED, format!("refused: work {verb}: poison is lifted with `work groom unpoison <id> --cause .. --evidence ..`, never by a label edit")));
            }
            let op = if verb == "label-add" { "add" } else { "remove" };
            Ok(vec![bd(vec![s("label"), s(op), id, label.clone()])])
        }
        "dep-add" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let on = bead_arg(verb, a, 1, "<depends-on-id>")?;
            let f = parse_flags(verb, a, 2, &["--type"], &[])?;
            if let Some(t) = get(&f, "--type") {
                if !["blocks", "related", "parent-child", "discovered-from"].contains(&t) {
                    return Err(usage(verb, &format!("--type {t:?} (want blocks, related, parent-child, discovered-from)")));
                }
            }
            // bead.sh's own wrapper, not raw `bd dep add`: it refuses a blocks edge onto an
            // incident bead, which has no completion path (law-a-refusal-names-its-exit).
            Ok(vec![Step::Tool { program: "bead.sh", args: [vec![s("dep"), s("add"), id, on], flatten(&f)].concat(), stdin: None }])
        }
        "relate" => {
            let x = bead_arg(verb, a, 0, "<bead-id>")?;
            let y = bead_arg(verb, a, 1, "<other-bead-id>")?;
            if a.len() != 2 {
                return Err(usage(verb, "want <bead-id> <other-bead-id>"));
            }
            Ok(vec![bd(vec![s("dep"), s("relate"), x, y])])
        }
        "reopen" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &["--evidence"], &[])?;
            let Some(ev) = get(&f, "--evidence").filter(|e| !e.trim().is_empty()) else {
                return Err(usage(verb, "--evidence <text> is required: a reopen with no evidence hands the next session nothing"));
            };
            Ok(vec![bd(vec![s("reopen"), id, s("--reason"), format!("reopened by {actor}: {ev}")])])
        }
        "close-other" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &["--evidence"], &[])?;
            let Some(ev) = get(&f, "--evidence").filter(|e| !e.trim().is_empty()) else {
                return Err(usage(verb, "--evidence <text> is required"));
            };
            Ok(vec![Step::Tool { program: "groomer", args: vec![s("close"), id, s("--evidence"), ev.to_string()], stdin: None }])
        }
        "file" => {
            let Some(title) = a.first().filter(|t| !t.trim().is_empty() && !t.starts_with('-')) else {
                return Err(usage(verb, "missing \"<title>\""));
            };
            let f = parse_flags(verb, a, 1, &["--for", "--kind", "--repo", "--priority", "--parent", "--body-file"], &[])?;
            let persona = get(&f, "--for");
            if persona.is_some() == get(&f, "--kind").is_some() {
                return Err(usage(verb, "exactly one of --for <persona> (work) or --kind <kind> (a record) is required"));
            }
            if persona.is_some() && get(&f, "--repo").is_none() {
                return Err(usage(verb, "--for needs --repo <name>"));
            }
            if let Some(p) = get(&f, "--parent") {
                if !is_bead_id(p) {
                    return Err(usage(verb, &format!("--parent {p:?} is not a bead id")));
                }
            }
            match get(&f, "--body-file") {
                Some("-") if call.stdin.is_none() => return Err(usage(verb, "--body-file - given but no body arrived on stdin")),
                Some("-") | None => {}
                Some(other) => return Err(usage(verb, &format!("--body-file {other:?}: the client sends the body; the broker reads only -"))),
            }
            let mut steps = vec![Step::Tool {
                program: "bead.sh",
                args: [vec![s("file"), title.clone()], flatten(&f)].concat(),
                stdin: call.stdin.clone(),
            }];
            if persona.is_some() {
                steps.push(Step::CreateRow);
            }
            Ok(steps)
        }
        "ask" => {
            let f = parse_flags(verb, a, 0, &["--subject", "--kind", "--default", "--class", "--bead", "--body-file"], &["--dry-run"])?;
            let dry_run = f.iter().any(|(k, _)| k == "--dry-run");
            let (Some(subject), Some(kind), Some(default)) = (get(&f, "--subject"), get(&f, "--kind"), get(&f, "--default")) else {
                return Err(usage(verb, "--subject, --kind and --default are all required (an ask without a default is incomplete)"));
            };
            if !MAIL_KINDS.contains(&kind) {
                return Err(usage(verb, &format!("--kind {kind:?} (want one of {})", MAIL_KINDS.join(", "))));
            }
            let cited = match get(&f, "--bead") {
                Some(b) if is_bead_id(b) => Some(b.to_string()),
                Some(b) => return Err(usage(verb, &format!("--bead {b:?} is not a bead id"))),
                None if is_bead_id(bound) => Some(bound.to_string()),
                None => None,
            };
            let body = match (get(&f, "--body-file"), &call.stdin) {
                (Some("-"), Some(text)) => text.clone(),
                (Some(other), _) => return Err(usage(verb, &format!("--body-file {other:?}: the client sends the body on stdin"))),
                // mail's "question" kind requires filled "## Question"/"## Default" sections
                // (see cmd_blocked); a bare ask gets them from its own flags.
                (None, _) => format!("## Question\n{subject}\n\n## Default\n{default}\n"),
            };
            let mut args = vec![
                s("send"),
                s("operator"),
                s("--from"),
                format!("{} <{actor}@spira>", display_name(actor)),
                s("--subject"),
                subject.to_string(),
                s("--kind"),
                kind.to_string(),
                s("--default"),
                default.to_string(),
            ];
            if let Some(c) = get(&f, "--class") {
                args.extend([s("--class"), c.to_string()]);
            }
            let own = cited.as_deref() == Some(bound) && is_bead_id(bound);
            // mail reads BEAD_ID only as "an aeon is asking"; a caller bound to nothing and
            // citing nothing is still a persona asking, so it is classified too.
            let classify_as = cited.clone().unwrap_or_else(|| bound.to_string());
            if let Some(b) = cited {
                args.extend([s("--bead"), b]);
            }
            if dry_run {
                args.push(s("--dry-run"));
                return Ok(vec![Step::Ask { args, stdin: body, classify_as }]);
            }
            let mut steps = vec![Step::Ask { args, stdin: body, classify_as }];
            // A bound session's question about its OWN bead is a wait on the operator: that
            // bead is held once the question is delivered (the mail goes first, so a hold
            // never stands without a question behind it). A question citing another bead —
            // the groomer's "Close <id>?" — is not this session's wait, and holds nothing.
            if kind == "question" && own {
                steps.push(Step::AskHold { bead: bound.to_string(), question: subject.to_string() });
            }
            Ok(steps)
        }
        tool => {
            let Some(&(_, program)) = TOOLS.iter().find(|(v, _)| *v == tool) else {
                return Err(usage(tool, "unknown verb"));
            };
            if tool == "census" && !(a.is_empty() || a == &[s("--with-suppressed")]) {
                return Err(usage(tool, "takes only --with-suppressed"));
            }
            if tool == "groom" && a.first().map(String::as_str) == Some("split-piece") {
                split_piece_args(&a[1..])?;
            }
            Ok(vec![Step::Tool { program, args: a.clone(), stdin: call.stdin.clone() }])
        }
    }
}

/// `work fence <class>` — `czar-fence.sh`'s answer, given here so the czar needs no path
/// into the release: `act` exits 0, `shadow` (the default, and the answer when the stage
/// cannot be read) exits 1, anything else exits 2. The stage is this installation's own
/// `SPIRA_CZAR_STAGE_<CLASS>`, resolved by the broker — never the caller's environment.
pub fn fence_answer(class: &str, stage: Option<&str>) -> (i32, String) {
    if class.is_empty() || !class.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return (2, format!("czar-fence: class {class:?} is not a czar class"));
    }
    let var = format!("SPIRA_CZAR_STAGE_{}", class.to_ascii_uppercase().replace('-', "_"));
    match stage.unwrap_or("shadow") {
        "act" => (0, format!("czar-fence: {class} is act")),
        "shadow" => (1, format!("czar-fence: {class} is shadow — mutation refused ({var}=act to enable)")),
        other => (2, format!("czar-fence: unknown stage {other} in {var} (shadow or act)")),
    }
}

fn cmd_fence(rest: &[String]) -> (i32, String) {
    let call = split_reserved(rest);
    let [class] = call.args.as_slice() else {
        return usage("fence", "want <class>");
    };
    let var = format!("SPIRA_CZAR_STAGE_{}", class.to_ascii_uppercase().replace('-', "_"));
    // One source of config (per Ryan 2026-10-05): the declared config-file value, never this
    // process's own environment.
    let stage = spira_config::process::cfg(&var).ok();
    fence_answer(class, stage.as_deref())
}

/// `groom split-piece <original-id> "<title>" [flags]`: the tail reaches `bd create`, so it
/// is held to the create flags a piece needs — never `-C`/`--db` or anything else bd takes.
fn split_piece_args(a: &[String]) -> Result<(), (i32, String)> {
    let verb = "groom split-piece";
    bead_arg(verb, a, 0, "<original-id>")?;
    const VALUES: &[&str] = &["--title", "-d", "--description", "-p", "--priority", "-t", "--type", "-l", "--labels"];
    let (mut i, mut positional) = (1, 0);
    while i < a.len() {
        let t = a[i].as_str();
        if VALUES.contains(&t) {
            if i + 1 >= a.len() {
                return Err(usage(verb, &format!("{t} needs a value")));
            }
            i += 2;
        } else if t.starts_with('-') {
            return Err(usage(verb, &format!("{t:?} is not a create flag a split piece may pass")));
        } else {
            positional += 1;
            i += 1;
        }
    }
    if positional > 1 {
        return Err(usage(verb, "one positional <title> only"));
    }
    Ok(())
}

/// The kinds mail has a template for; a test holds this equal to `spira/mail/kinds`.
/// `work list`: lifecycle state decides which beads, bd supplies what they say. A state filter
/// is the machine's; `--label`/`--title-contains` narrow by content through bd, and the two are
/// intersected here so neither side's idea of "open" is consulted.
fn list_with_content(conn: &Conn, states: &[String], all: bool, bd_args: &[String], json: bool, limit: Option<usize>) -> (i32, String) {
    let (rc, rows) = crate::cmd_list(&[], conn);
    if rc != 0 {
        return (rc, rows);
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&rows).unwrap_or_default();
    let keep = |st: &str| if !states.is_empty() { states.iter().any(|x| x == st) } else { all || !bead::BeadState::from_str(st).is_some_and(|b| b.is_terminal()) };
    let by_id: std::collections::HashMap<String, serde_json::Value> =
        rows.into_iter().filter(|r| keep(r["state"].as_str().unwrap_or(""))).filter_map(|r| Some((r["bead_id"].as_str()?.to_string(), r))).collect();
    let mut args = vec![s("list"), s("--status"), s("all"), s("--limit"), s("0"), s("--json")];
    args.extend(bd_args.iter().cloned());
    let content = match crate::bd::run_stdin(&args, None) {
        Ok(t) => t,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let content: Vec<serde_json::Value> = serde_json::from_str(content.trim()).unwrap_or_default();
    let mut out = Vec::new();
    for mut c in content {
        let Some(lc) = c["id"].as_str().and_then(|id| by_id.get(id)).cloned() else { continue };
        c["lifecycle_state"] = lc["state"].clone();
        c["lifecycle_holds"] = lc["holds"].clone();
        out.push(c);
    }
    out.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    if let Some(n) = limit {
        out.truncate(n);
    }
    if json {
        return (0, serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    let lines: Vec<String> =
        out.iter().map(|c| format!("{} {} {}", c["id"].as_str().unwrap_or("?"), c["lifecycle_state"].as_str().unwrap_or("?"), c["title"].as_str().unwrap_or(""))).collect();
    (0, lines.join("\n"))
}

const MAIL_KINDS: &[&str] = &["alert", "decision", "event", "note", "question", "suit"];

fn cmd_lane(bound: &str, verb: &str, rest: &[String], conn: &Conn) -> (i32, String) {
    let call = split_reserved(rest);
    let steps = match plan(verb, bound, &call) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let actor = call.actor.as_deref().unwrap_or("aeon");
    let mut out = String::new();
    for step in steps {
        let (code, text) = match step {
            Step::Bd { args, stdin } => match crate::bd::run_stdin(&args, stdin.as_deref()) {
                Ok(t) => (0, t),
                Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
            },
            Step::LcList { states, all, bd_args, json, limit } => list_with_content(conn, &states, all, &bd_args, json, limit),
            Step::Tool { program, args, stdin } => {
                // mail, by name; SPIRA_MAIL_SH is the harness-wide binary-override seam.
                let prog = if program == "mail" { std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| s("mail")) } else { s(program) };
                crate::bd::tool(&prog, &args, stdin.as_deref(), actor, TOOL_SECS)
            }
            Step::Ask { args, stdin, classify_as } => {
                let prog = std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| s("mail"));
                crate::bd::tool_env_noisy(&prog, &args, Some(&stdin), actor, TOOL_SECS, &[("BEAD_ID", &classify_as)])
            }
            Step::AskHold { bead, question } => {
                let hold = BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some(question) };
                match apply_bead_event(conn, &bead, actor, hold) {
                    (0, _) => (0, format!("question delivered; {bead} is held (ask) until the operator answers\n")),
                    (code, e) => (
                        code,
                        format!("the question WAS delivered (do not ask again), but the ask hold on {bead} was not applied: {e}"),
                    ),
                }
            }
            Step::CreateRow => {
                let new_id = out.trim().to_string();
                match crate::cutover::cmd_create_bead(&[new_id.clone()], conn) {
                    (0, _) => (0, String::new()),
                    (_, e) => (
                        CANNOT_TELL,
                        format!("LIFECYCLE: {new_id} was filed but has NO lifecycle row and cannot be claimed (do not file it again): {e}"),
                    ),
                }
            }
        };
        out.push_str(&text);
        if code != 0 {
            return (code, out);
        }
    }
    (0, out.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_of_prose_or_a_truncated_prefix_is_not_a_bead_id() {
        for bad in ["SPIRA_BEAD_LANE_OVERRIDE=1", "bead:", "override:", "sp-", "sp", "", "sp-a b", "sp-x/y", "x-abc"] {
            assert!(!is_bead_id(bad), "{bad:?}");
        }
        for ok in ["sp-b411iv", "sp-b411iv.1"] {
            assert!(is_bead_id(ok), "{ok}");
        }
    }

    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_uid_bound_to_a_persona_cannot_present_as_another() {
        let map = parse_persona_uids("# aeons\n1001 builder\n1002 czar  # the czar\nbad line here\n");
        assert_eq!(map, vec![(1001, "builder".to_string()), (1002, "czar".to_string())]);
        let forged = v(&["work", "sp-1", "queue", "eject", "sp-2", "--actor", "czar"]);
        let (code, msg) = bind_peer(&forged, Some(1001), &map).unwrap_err();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("builder"), "{msg}");
        assert_eq!(bind_peer(&forged, Some(1002), &map).unwrap(), forged);
        assert_eq!(bind_peer(&v(&["work", "sp-1", "show"]), Some(1001), &map).unwrap(), v(&["work", "sp-1", "show", "--actor", "builder"]));
        assert!(bind_peer(&v(&["work", "sp-1", "show", "--actor", "czar", "--actor", "builder"]), Some(1001), &map).is_err());
        assert!(bind_peer(&forged, None, &map).is_err());
        assert_eq!(bind_peer(&forged, Some(5), &map).unwrap(), forged);
        assert_eq!(bind_peer(&forged, Some(1001), &[]).unwrap(), forged);
        let other = v(&["show", "sp-1"]);
        assert_eq!(bind_peer(&other, Some(1001), &map).unwrap(), other);
    }

    fn call(xs: &[&str], actor: &str) -> Call {
        split_reserved(&[v(xs), v(&["--actor", actor])].concat())
    }

    fn tool(program: &'static str, args: &[&str]) -> Step {
        Step::Tool { program, args: v(args), stdin: None }
    }

    // ---- the gate ----

    #[test]
    fn every_allow_row_names_a_known_verb() {
        for (op, who) in ALLOW {
            let verb = op.split(' ').next().unwrap();
            assert!(BOUND_VERBS.contains(&verb) || LANE_VERBS.contains(&verb), "{op}");
            assert!(!who.is_empty(), "{op}");
        }
    }

    #[test]
    fn a_persona_may_not_run_a_verb_it_is_not_allowed() {
        let (code, msg) = permitted("groom close", Some("builder")).unwrap_err();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("builder") && msg.contains("groom close"), "{msg}");
        assert_eq!(gate("note-on", &v(&["sp-x1", "hi", "--actor", "builder"])).unwrap_err().0, REFUSED);
        assert_eq!(gate("queue", &v(&["eject", "sp-x1", "--actor", "groomer"])).unwrap_err().0, REFUSED);
    }

    #[test]
    fn the_allowed_persona_passes() {
        assert!(gate("groom", &v(&["close", "sp-x1", "--evidence", "e", "--actor", "groomer"])).is_ok());
        assert!(gate("queue", &v(&["eject", "sp-x1", "--actor", "czar"])).is_ok());
        assert!(gate("census", &v(&["--with-suppressed", "--actor", "maechen"])).is_ok());
    }

    #[test]
    fn a_tool_subcommand_absent_from_the_table_is_refused_for_everyone() {
        assert_eq!(gate("queue", &v(&["flush", "--actor", "czar"])).unwrap_err().0, REFUSED);
        assert_eq!(gate("groom", &v(&["--actor", "groomer"])).unwrap_err().0, REFUSED);
    }

    #[test]
    fn a_gated_verb_without_a_persona_is_refused() {
        assert_eq!(gate("note-on", &v(&["sp-x1", "hi"])).unwrap_err().0, REFUSED);
    }

    #[test]
    fn bound_verbs_need_no_persona() {
        assert!(gate("show", &[]).is_ok());
        assert!(gate("note", &v(&["hi"])).is_ok());
    }

    #[test]
    fn a_second_actor_is_refused() {
        assert_eq!(gate("note-on", &v(&["sp-x1", "--actor", "czar", "--actor", "builder"])).unwrap_err().0, REFUSED);
    }

    #[test]
    fn split_reserved_takes_the_trailer_only() {
        let c = split_reserved(&v(&["a", "-", "--stdin", "body", "--actor", "ops"]));
        assert_eq!(c.args, v(&["a", "-"]));
        assert_eq!(c.stdin.as_deref(), Some("body"));
        assert_eq!(c.actor.as_deref(), Some("ops"));
    }

    // ---- reads ----

    #[test]
    fn read_shows_another_bead() {
        assert_eq!(plan("read", "sp-me", &call(&["sp-x1", "--json"], "spike")).unwrap(), vec![Step::Bd { args: v(&["show", "sp-x1", "--json"]), stdin: None }]);
        assert!(plan("read", "sp-me", &call(&["not-an-id"], "spike")).is_err());
        assert!(plan("read", "sp-me", &call(&["sp-x1", "-C", "/elsewhere"], "spike")).is_err());
    }

    #[test]
    fn list_filters_by_lifecycle_state_and_content() {
        assert_eq!(
            plan("list", "-", &call(&["--state", "ready,rework", "--label", "plan", "--json"], "warden")).unwrap(),
            vec![Step::LcList { states: v(&["READY", "REWORK"]), all: false, bd_args: v(&["--label", "plan"]), json: true, limit: None }]
        );
        assert!(plan("list", "-", &call(&["--db", "/x"], "warden")).is_err());
        assert!(plan("list", "-", &call(&["--state", "open"], "warden")).is_err(), "a bd status word is not a lifecycle state");
        assert!(plan("list", "-", &call(&["--limit", "many"], "warden")).is_err());
    }

    #[test]
    fn list_refuses_bds_status_and_names_the_lifecycle_flag() {
        let e = plan("list", "-", &call(&["--status", "open"], "warden")).unwrap_err();
        assert!(e.1.contains("--state"), "{}", e.1);
    }

    #[test]
    fn show_replaces_bds_status_word_with_the_lifecycle_state() {
        let bd = "○ sp-1 · title   [P2 · OPEN]\nOwner: x\n";
        let row = r#"{"bead":{"state":"SUBMITTED","holds":"[\"wait\"]","reason":"snooze-until:9","holder":null},"delivery":null}"#;
        let out = with_lifecycle(bd, row);
        assert!(out.starts_with("○ sp-1 · title   [P2 · SUBMITTED]\n"), "{out}");
        assert!(!out.contains("OPEN"), "{out}");
        assert!(out.contains("LIFECYCLE: SUBMITTED\n  holds:"), "{out}");
        assert!(out.contains("  reason: snooze-until:9\n"), "{out}");
        assert!(!out.contains("holder"), "{out}");
    }

    #[test]
    fn search_needs_a_query() {
        assert_eq!(plan("search", "-", &call(&["merge queue"], "groomer")).unwrap(), vec![Step::Bd { args: v(&["search", "merge queue"]), stdin: None }]);
        assert!(plan("search", "-", &call(&["--status", "open"], "groomer")).is_err());
    }

    // ---- writes ----

    #[test]
    fn note_on_takes_text_or_stdin() {
        assert_eq!(plan("note-on", "-", &call(&["sp-x1", "hi"], "czar")).unwrap(), vec![Step::Bd { args: v(&["note", "sp-x1", "hi"]), stdin: None }]);
        let c = split_reserved(&v(&["sp-x1", "-", "--stdin", "long `prose`", "--actor", "czar"]));
        assert_eq!(plan("note-on", "-", &c).unwrap(), vec![Step::Bd { args: v(&["note", "sp-x1", "--stdin"]), stdin: Some("long `prose`".into()) }]);
        assert!(plan("note-on", "-", &call(&["sp-x1", "-"], "czar")).is_err(), "- with nothing on stdin");
        assert!(plan("note-on", "-", &call(&["sp-x1"], "czar")).is_err());
    }

    #[test]
    fn labels_are_one_label_and_never_poison() {
        assert_eq!(plan("label-add", "-", &call(&["sp-x1", "overseer"], "groomer")).unwrap(), vec![Step::Bd { args: v(&["label", "add", "sp-x1", "overseer"]), stdin: None }]);
        assert_eq!(
            plan("label-remove", "-", &call(&["sp-x1", "lane:foo"], "groomer")).unwrap(),
            vec![Step::Bd { args: v(&["label", "remove", "sp-x1", "lane:foo"]), stdin: None }]
        );
        assert!(plan("label-add", "-", &call(&["sp-x1", "a,b"], "groomer")).is_err());
        assert_eq!(plan("label-remove", "-", &call(&["sp-x1", POISON_LABEL], "groomer")).unwrap_err().0, REFUSED);
    }

    #[test]
    fn dep_add_goes_through_bead_sh_and_checks_the_type() {
        assert_eq!(plan("dep-add", "-", &call(&["sp-a1", "sp-b2"], "ops")).unwrap(), vec![tool("bead.sh", &["dep", "add", "sp-a1", "sp-b2"])]);
        assert!(plan("dep-add", "-", &call(&["sp-a1", "sp-b2", "--type", "weird"], "ops")).is_err());
        assert!(plan("dep-add", "-", &call(&["sp-a1"], "ops")).is_err());
    }

    #[test]
    fn relate_is_a_relates_edge() {
        assert_eq!(plan("relate", "-", &call(&["sp-a1", "sp-b2"], "archivist")).unwrap(), vec![Step::Bd { args: v(&["dep", "relate", "sp-a1", "sp-b2"]), stdin: None }]);
    }

    #[test]
    fn reopen_and_close_other_need_evidence() {
        assert!(plan("reopen", "-", &call(&["sp-a1"], "czar")).is_err());
        assert_eq!(
            plan("reopen", "-", &call(&["sp-a1", "--evidence", "red on main"], "czar")).unwrap(),
            vec![Step::Bd { args: v(&["reopen", "sp-a1", "--reason", "reopened by czar: red on main"]), stdin: None }]
        );
        assert!(plan("close-other", "-", &call(&["sp-a1"], "czar")).is_err());
        assert_eq!(
            plan("close-other", "-", &call(&["sp-a1", "--evidence", "duplicate of sp-b2"], "czar")).unwrap(),
            vec![tool("groomer", &["close", "sp-a1", "--evidence", "duplicate of sp-b2"])]
        );
    }

    #[test]
    fn file_for_a_persona_also_creates_the_lifecycle_row() {
        let c = split_reserved(&v(&["a title", "--for", "builder", "--repo", "spira", "--body-file", "-", "--stdin", "body", "--actor", "warden"]));
        assert_eq!(
            plan("file", "-", &c).unwrap(),
            vec![
                Step::Tool { program: "bead.sh", args: v(&["file", "a title", "--for", "builder", "--repo", "spira", "--body-file", "-"]), stdin: Some("body".into()) },
                Step::CreateRow
            ]
        );
        let rec = plan("file", "-", &call(&["a finding", "--kind", "insight", "--repo", "spira"], "archivist")).unwrap();
        assert_eq!(rec.len(), 1, "a record is no work: no lifecycle row");
    }

    #[test]
    fn file_refuses_a_bad_shape() {
        assert!(plan("file", "-", &call(&["t"], "ops")).is_err(), "neither --for nor --kind");
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--kind", "insight", "--repo", "r"], "ops")).is_err());
        assert!(plan("file", "-", &call(&["t", "--for", "builder"], "ops")).is_err(), "no repo");
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--repo", "r", "--body-file", "/etc/passwd"], "ops")).is_err());
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--repo", "r", "-l", "x"], "ops")).is_err());
    }

    #[test]
    fn a_dry_run_ask_forwards_the_flag_and_places_no_hold() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--dry-run"], "builder")).unwrap();
        assert_eq!(p.len(), 1, "no AskHold: {p:?}");
        let Step::Ask { args, .. } = &p[0] else { panic!() };
        assert!(args.contains(&s("--dry-run")), "{args:?}");
    }

    #[test]
    fn ask_builds_the_mail_with_the_persona_as_sender() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "Close sp-a1?", "--kind", "question", "--default", "yes"], "groomer")).unwrap();
        let Step::Ask { args, stdin, classify_as } = &p[0] else { panic!() };
        assert_eq!(args[..4], v(&["send", "operator", "--from", "Groomer <groomer@spira>"])[..]);
        assert!(args.ends_with(&v(&["--bead", "sp-me1"])), "cites the bound bead by default: {args:?}");
        assert!(stdin.contains("## Question\nClose sp-a1?") && stdin.contains("## Default\nyes"));
        assert_eq!(classify_as, "sp-me1");
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "question"], "ops")).is_err(), "no default");
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "bogus", "--default", "d"], "ops")).is_err());
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "fyi", "--default", "d", "--from", "Ryan"], "ops")).is_err(), "the sender is the broker's");
        assert_eq!(display_name("batcher"), "Judge");
    }

    /// sp-v62vn follow-up: a restricted model asks through `work ask`; a question from a
    /// bound session also places the `ask` hold on the bound bead's row — the record aeon's
    /// teardown reads to release the session operator-wait (the marker mail wrote is gone).
    #[test]
    fn a_bound_question_holds_the_bound_bead_after_the_mail() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "May I?", "--kind", "question", "--default", "no"], "builder")).unwrap();
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(matches!(&p[0], Step::Ask { .. }), "the question is delivered first: {p:?}");
        assert_eq!(p[1], Step::AskHold { bead: s("sp-me1"), question: s("May I?") });
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--bead", "sp-me1"], "builder")).unwrap();
        assert_eq!(p.last(), Some(&Step::AskHold { bead: s("sp-me1"), question: s("q") }), "citing its own bead explicitly");
        // No hold for a question about another bead (the groomer's "Close <id>?"), for an
        // fyi, nor for an unbound caller (nothing of its own to wait on).
        let other = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--bead", "sp-other"], "groomer")).unwrap();
        assert!(!other.iter().any(|x| matches!(x, Step::AskHold { .. })), "{other:?}");
        let fyi = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "note", "--default", "d"], "builder")).unwrap();
        assert!(!fyi.iter().any(|x| matches!(x, Step::AskHold { .. })), "{fyi:?}");
        let unbound = plan("ask", "-", &call(&["--subject", "q", "--kind", "question", "--default", "d"], "archivist")).unwrap();
        assert!(!unbound.iter().any(|x| matches!(x, Step::AskHold { .. })), "{unbound:?}");
    }

    /// sp-v62vn follow-up: the broker's `mail` ran without BEAD_ID, which mail reads as "not
    /// an aeon", so its escalation-class check never ran and an unclassed question went
    /// straight to the operator. Every ask step now carries the id mail classifies by —
    /// bound, citing another bead, or bound to nothing at all.
    #[test]
    fn every_ask_is_sent_for_classification() {
        let classify = |bound: &str, extra: &[&str]| {
            let args = [&["--subject", "q", "--kind", "question", "--default", "d"][..], extra].concat();
            match &plan("ask", bound, &call(&args, "builder")).unwrap()[0] {
                Step::Ask { classify_as, .. } => classify_as.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(classify("sp-me1", &[]), "sp-me1");
        assert_eq!(classify("sp-me1", &["--bead", "sp-other"]), "sp-other");
        assert_eq!(classify("-", &[]), "-", "an unbound caller is a persona asking too");
        // and --class rides through for mail to judge, never decided here
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--class", "policy"], "builder")).unwrap();
        let Step::Ask { args, .. } = &p[0] else { panic!() };
        assert!(args.windows(2).any(|w| w == v(&["--class", "policy"])), "{args:?}");
    }

    #[test]
    fn ask_kinds_are_the_kinds_mail_can_deliver() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path().file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        assert_eq!(on_disk, MAIL_KINDS);
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "fyi", "--default", "d"], "ops")).is_err(), "fyi has no template");
    }

    // ---- tools ----

    #[test]
    fn tools_forward_their_subcommand_and_stdin() {
        let c = split_reserved(&v(&["file", "a finding", "-", "--stdin", "payload", "--actor", "ops"]));
        assert_eq!(plan("incident", "-", &c).unwrap(), vec![Step::Tool { program: "incident.sh", args: v(&["file", "a finding", "-"]), stdin: Some("payload".into()) }]);
        assert_eq!(plan("groom", "-", &call(&["sweep"], "groomer")).unwrap(), vec![tool("groomer", &["sweep"])]);
        assert_eq!(plan("queue", "-", &call(&["eject", "sp-a1", "--red"], "czar")).unwrap(), vec![tool("queue", &["eject", "sp-a1", "--red"])]);
        assert_eq!(plan("strand", "-", &call(&["detect-livelocked"], "groomer")).unwrap(), vec![tool("strand", &["detect-livelocked"])]);
        assert_eq!(plan("sop", "-", &call(&["show", "x"], "ops")).unwrap(), vec![tool("sop", &["show", "x"])]);
    }

    #[test]
    fn ops_may_collapse_a_duplicate_incident_and_a_builder_may_not() {
        assert!(permitted(&op_key("incident", &v(&["collapse", "sp-a1", "--of", "sp-b2"])), Some("ops")).is_ok());
        assert!(permitted("incident collapse", Some("builder")).is_err());
        assert_eq!(
            plan("incident", "-", &call(&["collapse", "sp-a1", "--of", "sp-b2"], "ops")).unwrap(),
            vec![Step::Tool { program: "incident.sh", args: v(&["collapse", "sp-a1", "--of", "sp-b2"]), stdin: None }]
        );
    }

    #[test]
    fn fence_answers_like_czar_fence_sh() {
        assert_eq!(fence_answer("ci-red", Some("act")).0, 0);
        let (code, msg) = fence_answer("ci-red", None);
        assert_eq!(code, 1, "unreadable stage is shadow");
        assert!(msg.contains("SPIRA_CZAR_STAGE_CI_RED=act"), "{msg}");
        assert_eq!(fence_answer("ci-red", Some("loud")).0, 2);
        assert_eq!(fence_answer("../x", Some("act")).0, 2);
        assert_eq!(gate("fence", &v(&["ci-red", "--actor", "groomer"])).unwrap_err().0, REFUSED);
        assert!(gate("fence", &v(&["ci-red", "--actor", "czar"])).is_ok());
    }

    #[test]
    fn census_takes_only_its_one_flag() {
        assert_eq!(plan("census", "-", &call(&["--with-suppressed"], "maechen")).unwrap(), vec![tool("census", &["--with-suppressed"])]);
        assert!(plan("census", "-", &call(&["run-events"], "maechen")).is_err());
    }

    #[test]
    fn split_piece_never_reaches_bd_with_a_foreign_flag() {
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "piece one", "-p", "2", "-t", "task"], "groomer")).is_ok());
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "piece", "-C", "/other/db"], "groomer")).is_err());
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "a", "b"], "groomer")).is_err());
        assert!(plan("groom", "-", &call(&["split-piece", "nope"], "groomer")).is_err());
    }
}
