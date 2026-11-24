//! `content <bd-subcommand> …` — the one door for bead content: its reads, and the narrow
//! writes a task makes (labels, comments, notes, metadata, a close, a filing), so nothing
//! outside the store forks `bd`. Reads are SQL against the beads database (`content_sql`).
//! Label, note, comment, metadata and priority writes are SQL too (`content_write`); the rest
//! fork bd with bd's own argv, which owns id allocation. State is never served here: the machine's own verbs answer that. Anything outside the allowlist is refused.

use crate::callers::Answer;

const REFUSED: i32 = 3;
const CANNOT_TELL: i32 = 2;

/// What an allowed sub-verb is: a read, or a write, which takes the request body as bd's stdin
/// when `body` (`note --stdin`, `create --body-file -`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Read,
    Write { body: bool },
}

const WRITE: Result<Kind, String> = Ok(Kind::Write { body: false });

fn word(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-')
}

pub fn check(args: &[String]) -> Result<Kind, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["list" | "show" | "ref" | "memories" | "state" | "search" | "events", ..] => Ok(Kind::Read),
        ["dep", "list", ..] => Ok(Kind::Read),
        ["label", "list", id] | ["label", "list", id, "--json"] if word(id) => Ok(Kind::Read),
        ["gate", "list", ..] => Ok(Kind::Read),
        // The planning store's id prefix, which a caller filing beads stamps its ids with.
        ["config", "get", key] if !key.starts_with('-') => Ok(Kind::Read),
        ["comments", "add", id, _text] if !id.starts_with('-') => WRITE,
        ["comments", sub, ..] if *sub != "add" => Ok(Kind::Read),
        ["label", "add" | "remove", id, label] if word(id) && word(label) => WRITE,
        ["note", id, "--stdin"] if word(id) => Ok(Kind::Write { body: true }),
        ["note", id, text] if word(id) && !text.starts_with('-') && !text.trim().is_empty() => WRITE,
        ["dep", "add", id, dep] if word(id) && word(dep) => WRITE,
        ["dep", "add", id, dep, "--type", t] if word(id) && word(dep) && DEP_TYPES.contains(t) => WRITE,
        ["dep", "remove" | "relate", a, b] if word(a) && word(b) => WRITE,
        ["set-state", id, kv] if word(id) && settable_state(kv) => WRITE,
        ["duplicate", id, "--of", canon] if word(id) && word(canon) => WRITE,
        ["recompute-blocked"] => WRITE,
        ["gate", "discover", "--branch", b, "--repo", name] if word(b) && word(name) => WRITE,
        ["gate", "check", "--type", t] if word(t) => WRITE,
        ["gate", "check", t] if t.strip_prefix("--type=").is_some_and(word) => WRITE,
        ["gate", "resolve", id] if word(id) => WRITE,
        ["kv", "get", k] | ["kv", "get", k, "--json"] if word(k) => Ok(Kind::Read),
        ["kv", "set" | "clear", k, ..] if *k == crate::plan::KV_KEY => Err(format!("`{k}` is the planning watermark, which only the planner writes; it is not settable through the door")),
        ["kv", "set", k, val] if word(k) && word(val) => WRITE,
        ["kv", "clear", k] if word(k) => WRITE,
        ["remember", "--key", k] if word(k) => Ok(Kind::Write { body: true }),
        ["forget", k] if word(k) => WRITE,
        ["recall", k] if word(k) => Ok(Kind::Read),
        ["close", id] if word(id) => WRITE,
        ["close", id, "--reason", reason] if word(id) && !reason.trim().is_empty() => WRITE,
        ["update", id, flags @ ..] if !id.starts_with('-') && !flags.is_empty() => update_flags(flags),
        ["create", rest @ ..] if !rest.is_empty() => create_flags(rest),
        ["ready", ..] => Err(unserved("ready", "`spira-lc list --state READY,REWORK` or `spira-lc claimable` (the claim view) or `spira-claim rune-ready`")),
        ["blocked", ..] => Err(unserved("blocked", "`spira-lc list` (the published blocked_by)")),
        ["history", ..] => Err(unserved("history", "`spira-lc fact list --ids` or `content list --updated-after`")),
        ["children", ..] => Err(unserved("children", "`content list --all --limit 0 --parent <id>`")),
        ["sql", ..] => Err(unserved("sql", "a typed read: add the read this query stands for to the door (content_sql) rather than sending SQL")),
        _ => Err(format!("`{}` is not a content verb", a.join(" "))),
    }
}

const DEP_TYPES: &[&str] = &["blocks", "relates-to", "parent-child", "discovered-from"];

/// The state dimensions a caller may set through the door: a branch or a lane, with a value.
fn settable_state(kv: &str) -> bool {
    matches!(kv.split_once('='), Some(("branch" | "lane", v)) if !v.is_empty())
}

fn unserved(verb: &str, instead: &str) -> String {
    format!("`{verb}` is not a content verb; use {instead}")
}

fn update_flags(flags: &[&str]) -> Result<Kind, String> {
    let mut it = flags.iter();
    let (mut body, mut forced, mut assignee, mut bd_force) = (false, false, false, false);
    while let Some(f) = it.next() {
        if *f == "--force" {
            bd_force = true;
            continue;
        }
        match (*f, it.next()) {
            ("--assignee", Some(_)) => assignee = true,
            ("--priority" | "-p", Some(p)) if matches!(*p, "0" | "1" | "2" | "3" | "4") => {}
            (bead::claimdesc::FORCE_FLAG, Some(reason)) if !reason.trim().is_empty() => forced = true,
            ("--add-label" | "--remove-label", Some(l)) if !l.is_empty() => {}
            ("--set-metadata", Some(kv)) if kv.split_once('=').is_some_and(|(k, _)| word(k)) => {}
            ("--body-file", Some(&"-")) => body = true,
            ("--body-file", _) => return Err("update takes its description only as --body-file - (the request body)".into()),
            _ => return Err(format!("update takes only --add-label/--remove-label <label>, --set-metadata <key=value>, --priority 0-4 and --force-claimed <reason> with a body, not `{f}`")),
        }
    }
    if bd_force && !assignee {
        return Err("update takes --force only with --assignee".into());
    }
    if forced && !body {
        return Err(format!("update takes {} only with --body-file - (a claimed description edit)", bead::claimdesc::FORCE_FLAG));
    }
    Ok(Kind::Write { body })
}

/// `create [<title>] <flag>…`: the filing flags the store's writers pass, a body only on stdin.
fn create_flags(args: &[&str]) -> Result<Kind, String> {
    let (mut titled, mut body) = (false, false);
    let mut it = args.iter().enumerate();
    while let Some((i, f)) = it.next() {
        match *f {
            t if i == 0 && word(t) => titled = true,
            "--silent" | "--json" | "--no-inherit-labels" => {}
            "--body-file" => match it.next() {
                Some((_, &"-")) => body = true,
                _ => return Err("create takes its body only as --body-file - (the request body)".into()),
            },
            "--title" | "--type" | "-t" | "--status" | "--priority" | "-p" | "--labels" | "--label" | "-l" | "--external-ref" | "--parent" | "--deps" | "--description" | "-d" | "--assignee" | "--id" => match it.next() {
                Some((_, v)) if !v.is_empty() => titled |= *f == "--title",
                _ => return Err(format!("create: `{f}` needs a value")),
            },
            _ => return Err(format!("create takes only a title and the filing flags, not `{f}`")),
        }
    }
    if !titled {
        return Err("create needs a title".into());
    }
    Ok(Kind::Write { body })
}

/// The programs whose ticks must not read the whole bead history (a caller is named
/// `<program>` or `<program>/<subcommand>`).
const TICK_DRIVEN: &[&str] = &["spira-runtime", "cockpit-collect", "panel"];

/// Refuses `content list --all --limit 0` (exit 3) from a scheduler task or the cockpit collector.
pub fn refuse_whole_history(args: &[String], caller: &str) -> Option<Answer> {
    let argv: Vec<&str> = std::iter::once("content").chain(args.iter().map(String::as_str)).collect();
    let program = caller.split('/').next().unwrap_or("");
    if !spira_store::lc_call::reads_whole_history(&argv) || !TICK_DRIVEN.contains(&program) {
        return None;
    }
    Some(Answer {
        code: REFUSED,
        stderr: format!("spira-lc content: `list --all --limit 0` reads every bead ever filed and is refused from {caller} (exit {REFUSED}); read open beads, or bound the read with --id, --parent or --updated-after/--closed-after\n"),
        ..Default::default()
    })
}

/// A content write: bd's argv, and its stdin when the sub-verb takes the request body.
pub type Write<'a> = &'a dyn Fn(&[String], Option<&str>) -> Result<String, String>;

/// Whether the client sends its stdin as the request body for `args` (after `content`).
pub fn takes_body(args: &[String]) -> bool {
    matches!(check(args), Ok(Kind::Write { body: true }))
}

/// What the guards read besides the machine: the caller's guard variables (off the request),
/// the ask label and the repo map (this installation's), and the time.
pub struct Guards<'a> {
    pub caller: &'a std::collections::BTreeMap<String, String>,
    pub ask_label: &'a dyn Fn() -> Result<String, String>,
    pub registry: &'a dyn Fn() -> spira_config::repos::Registry,
    /// The filing contract's policy: personas, repo lanes, identity terms and statutes.
    pub policy: &'a dyn Fn() -> Result<bead::filing::Policy, String>,
    /// A bead's `repo:` label, for the guards on text written to an existing bead.
    pub repo_of: &'a dyn Fn(&str) -> Option<String>,
    /// A dependency target as bd shows it, for the refusals on a `blocks` edge.
    pub target_of: &'a dyn Fn(&str) -> Option<bead::ShowRow>,
    /// `SPIRA_INCIDENT_LABEL`, this installation's, resolved only by a `blocks` edge.
    pub incident_label: &'a dyn Fn() -> Result<String, String>,
    pub now: i64,
    /// A forced edit the guard admitted, for [`acknowledge`] once bd has applied it.
    pub forced: std::cell::RefCell<Option<Forced>>,
    /// The registered repo root a `gate discover` runs bd in, for the write to take.
    pub cwd: std::cell::RefCell<Option<std::path::PathBuf>>,
}

/// A claimed description edit admitted under `--force-claimed`.
pub struct Forced {
    pub id: String,
    pub claim: bead::claimdesc::LiveClaim,
    pub reason: String,
}

/// bdq's write guards on a content write (`bead::guard::write`), in bdq's order: the
/// create-time fences, the ask-label write fence, the state-verb fence, the czar fence, the
/// claimed-description check. The lifecycle row comes from the machine `m`; `body` is the
/// request body a create judges as its description. A refusal is bdq's: exit 1 and its text,
/// so a write through this door is refused whenever the same write through bdq would be.
/// `Ok` carries the lines to log (an overridden state write).
pub fn guard(args: &mut Vec<String>, body: &mut Option<String>, g: &Guards, m: &mut dyn crate::callers::Machine) -> Result<String, Answer> {
    let rows = std::cell::RefCell::new(m);
    let row = |id: &str| match rows.borrow_mut().call(&["show".to_string(), id.to_string()]) {
        (0, out) => spira_store::lc_state::parse_show(&out).ok().flatten(),
        _ => None,
    };
    let vars = |k: &str| g.caller.get(k).cloned();
    let ctx = bead::guard::Ctx { who: "spira-lc content", vars: &vars, rows: &row, now: g.now, ask_label: g.ask_label, registry: g.registry };
    blocks_edge(args, g)?;
    own_parent_wait(args, g);
    repo_cwd(args, g)?;
    let mut log = match bead::guard::write(args, body.as_deref(), &ctx) {
        Ok(bead::guard::Admit { claim: bead::guard::Claim::Free, log }) => {
            if args.first().map(String::as_str) == Some("update") {
                let rest = bead::claimdesc::parse_update_args(&args[1..]).passthrough;
                args.truncate(1);
                args.extend(rest);
            }
            log
        }
        Ok(bead::guard::Admit { claim: bead::guard::Claim::Forced { id, claim, reason }, log }) => {
            let rest = bead::claimdesc::parse_update_args(&args[1..]).passthrough;
            args.truncate(1);
            args.extend(rest);
            *g.forced.borrow_mut() = Some(Forced { id, claim, reason });
            log
        }
        Err(r) => return Err(Answer { code: r.code, stderr: r.text, ..Default::default() }),
    };
    if args.first().map(String::as_str) == Some("remember") {
        args.extend(body.take());
    }
    if !filed_with_contract(args) {
        return Ok(log);
    }
    let policy = (g.policy)().map_err(|e| Answer { code: bead::guard::REFUSED, stderr: format!("spira-lc content: the filing policy cannot be read: {}\n", e.trim_end()), ..Default::default() })?;
    let shown = if bead::bdq::is_create(args) {
        bead::filing::create(args, body, &policy, &vars)
    } else if args.first().map(String::as_str) == Some("label") {
        bead::filing::label_write(args, &policy).map(|()| String::new())
    } else {
        bead::filing::amend_text(args, body, g.repo_of, &policy, &vars)
    }
    .map_err(|r| Answer { code: r.code, stderr: r.text, ..Default::default() })?;
    log.push_str(&shown);
    Ok(log)
}

/// `gate discover … --repo <name>`: the name must be registered. The write runs in that root and
/// bd is not given the flag, so the caller never names a path.
fn repo_cwd(args: &mut Vec<String>, g: &Guards) -> Result<(), Answer> {
    let [gate, discover, _, _, repo, name] = args.as_slice() else { return Ok(()) };
    if (gate.as_str(), discover.as_str(), repo.as_str()) != ("gate", "discover", "--repo") {
        return Ok(());
    }
    let Some(root) = (g.registry)().root(name) else {
        return Err(Answer { code: bead::guard::REFUSED, stderr: format!("spira-lc content: gate discover --repo {name}: no such registered repo\n"), ..Default::default() });
    };
    *g.cwd.borrow_mut() = Some(root.into());
    args.truncate(4);
    Ok(())
}

/// The refusals `bead.sh dep add` gives a `blocks` edge: onto an epic, or onto an incident.
fn blocks_edge(args: &[String], g: &Guards) -> Result<(), Answer> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (id, dep, t) = match a.as_slice() {
        ["dep", "add", id, dep] => (id, dep, None),
        ["dep", "add", id, dep, "--type", t] => (id, dep, Some(*t)),
        _ => return Ok(()),
    };
    if !bead::is_blocks_type(t) {
        return Ok(());
    }
    let refuse = |text: String| Answer { code: bead::guard::REFUSED, stderr: format!("{text}\n"), ..Default::default() };
    let incident = (g.incident_label)().map_err(|e| refuse(format!("spira-lc content: the incident label cannot be read: {}", e.trim_end())))?;
    let Some(target) = (g.target_of)(dep) else { return Ok(()) };
    if target.issue_type == "epic" {
        return Err(refuse(bead::epic_blocks_refusal(id, dep)));
    }
    if target.labels.contains(&incident) {
        return Err(refuse(bead::incident_blocks_refusal(id, dep, &incident)));
    }
    Ok(())
}

/// A `blocks` edge onto the item's own parent is stored as the `split-child` label, which the
/// claim rule reads and which leaves the parent link in place; `dep remove` of it takes the label off.
fn own_parent_wait(args: &mut Vec<String>, g: &Guards) {
    let label = spira_config::nonwork::SPLIT_LABEL;
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (add, id, dep) = match a.as_slice() {
        ["dep", "add", id, dep] if bead::is_blocks_type(None) => (true, *id, *dep),
        ["dep", "add", id, dep, "--type", t] if bead::is_blocks_type(Some(t)) => (true, *id, *dep),
        ["dep", "remove", id, dep] => (false, *id, *dep),
        _ => return,
    };
    let Some(child) = (g.target_of)(id) else {
        return;
    };
    if child.parent.as_deref() != Some(dep) || (!add && !child.labels.iter().any(|l| l == label)) {
        return;
    }
    let verb = if add { "add" } else { "remove" };
    *args = ["label", verb, id, label].map(String::from).to_vec();
}

/// Whether the filing contract has anything to say about `args`: a create, a partition-label
/// write, or text written onto an existing bead.
fn filed_with_contract(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("create") | Some("label") | Some("note") => true,
        Some("update") => args.iter().any(|a| a == "--body-file"),
        _ => false,
    }
}

/// `create-bead` is idempotent, so a transient failure is retried before it is reported, as
/// bdq's `ensure_row_retrying` does.
const ROW_ATTEMPTS: u32 = 3;

/// What a write bd applied owes the lifecycle machine (`bead::guard::after`), written here in
/// the same request: a row for every bead a create's `stdout` names, a terminal row for a
/// closed bead. bd's write stands whatever happens; a lifecycle write that fails is reported
/// (the returned stderr lines), never swallowed and never a reason to repeat the bd write.
pub fn after(args: &[String], stdout: &str, m: &mut dyn crate::callers::Machine, bd: &mut dyn crate::callers::Bd, backoff_ms: u64) -> String {
    let (mut report, rowed) = rows_after(args, stdout, m, bd, backoff_ms);
    report.push_str(&mirror_after(args, m));
    let ids: Vec<String> = spira_store::planning::claim_inputs_changed(args, stdout).into_iter().filter(|i| rowed.as_ref().is_none_or(|ok| ok.contains(i))).collect();
    if !ids.is_empty() {
        let ans = crate::settle::sync_claims(&ids, false, m, bd);
        if ans.code != 0 {
            report.push_str(&format!("spira-lc content: LIFECYCLE: {}; claim inputs not written; repair: spira-lc sync-claim-inputs {}\n", ans.stderr.trim(), ids.join(" ")));
        }
    }
    report
}

const NOT_MIRRORED: &str = "is in bd but was not mirrored to the lifecycle store";

/// A `dep add`/`dep remove` bd applied is mirrored into the lifecycle store, as `bead.sh` does.
fn mirror_after(args: &[String], m: &mut dyn crate::callers::Machine) -> String {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (verb, id, dep, t) = match a.as_slice() {
        ["dep", "add", id, dep] => ("dep-add", id, dep, None),
        ["dep", "add", id, dep, "--type", t] => ("dep-add", id, dep, Some(*t)),
        ["dep", "remove", id, dep] => ("dep-remove", id, dep, None),
        _ => return String::new(),
    };
    match m.call(&bead::lc::dep_argv(verb, id, dep, t)) {
        (0, _) => String::new(),
        (_, _) if state_store::phases::deadline_passed() && crate::settle::owe_edge(verb, id, dep, t) => String::new(),
        (code, out) => format!("spira-lc content: LIFECYCLE: {verb} {id} -> {dep} {NOT_MIRRORED} (exit {code}: {}); re-run the `dep` verb to mirror it\n", out.trim()),
    }
}

/// A forced edit bd applied: bdq's acknowledgment, through the door's own reads and writes.
pub fn acknowledge(forced: Forced, read: &dyn Fn(&[String]) -> Result<String, crate::composed::ReadError>, write: Write<'_>, run: &str, mail: &str) {
    let show = |id: &str| read(&["show".to_string(), id.to_string(), "--json".to_string()]).unwrap_or_default();
    let mut put = |a: &[&str]| {
        let argv: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        let _ = write(&argv, None);
    };
    let actor = ["BEADS_ACTOR", "USER"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_else(|| "unknown".into());
    bead::claimdesc::acknowledge_forced_edit(&show, &mut put, &actor, &forced.id, &forced.claim, &forced.reason, (run, mail));
}

/// The lifecycle half of [`after`]: its report, and, for a create, the ids whose row now exists.
fn rows_after(args: &[String], stdout: &str, m: &mut dyn crate::callers::Machine, bd: &mut dyn crate::callers::Bd, backoff_ms: u64) -> (String, Option<Vec<String>>) {
    match bead::guard::after(args) {
        bead::guard::After::Nothing if bead::bdq::creates_beads(args) => (String::new(), Some(Vec::new())),
        bead::guard::After::Nothing => (String::new(), None),
        bead::guard::After::Rows => {
            let ids = spira_store::lifecycle_row::created_ids(stdout);
            if ids.is_empty() {
                return (format!("spira-lc content: LIFECYCLE: cannot read any created bead id from {stdout:?}; the new beads have no lifecycle row and cannot be claimed\n"), Some(Vec::new()));
            }
            let (mut errs, mut rowed) = (Vec::new(), Vec::new());
            for id in &ids {
                let repo = bead::bdq::created_repo(args);
                let argv = crate::settle::row_argv(id, repo.as_deref());
                let mut last = m.call(&argv);
                for n in 1..ROW_ATTEMPTS {
                    if last.0 == 0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms * u64::from(n)));
                    last = m.call(&argv);
                }
                if last.0 != 0 && state_store::phases::deadline_passed() && crate::settle::owe_row(id, repo.as_deref()) {
                    continue;
                }
                if last.0 != 0 {
                    errs.push(format!("create-bead {id} --ready exited {}: {}", last.0, last.1.trim()));
                } else {
                    rowed.push(id.clone());
                }
            }
            let report = if errs.is_empty() {
                String::new()
            } else {
                format!("spira-lc content: LIFECYCLE: some created beads have NO lifecycle row and cannot be claimed: {}\n", errs.join("; "))
            };
            (report, Some(rowed))
        }
        bead::guard::After::Terminal(id) => {
            let ans = crate::callers::reconcile_closed(&["--apply".to_string(), id.clone()], m, bd);
            let report = if ans.code == 0 {
                String::new()
            } else {
                format!("spira-lc content: LIFECYCLE: {id} was closed but its lifecycle row is not terminal and may read as claimable: {}\n", format!("{}{}", ans.stdout, ans.stderr).trim())
            };
            (report, None)
        }
    }
}

/// A content write's guards: `Ok` is the lines to log, `Err` the refusal answered instead of
/// the write. Given the write's argv and its request body.
pub type Guard<'a> = &'a mut dyn FnMut(&mut Vec<String>, &mut Option<String>) -> Result<String, Answer>;
/// What a write bd applied owes the lifecycle machine: the lines reporting any that failed.
pub type After<'a> = &'a mut dyn FnMut(&[String], &str) -> String;

/// One content call. A read goes to `read`. A write reads its request body when the sub-verb
/// takes one (refused, before bd runs, when none was sent), passes `guard` (bdq's write
/// guards), goes to `write`, and once bd has applied it, owes the lifecycle machine `after`.
pub fn run(
    args: &[String],
    stdin: &mut dyn FnMut() -> Result<String, String>,
    guard: Guard<'_>,
    after: After<'_>,
    read: &dyn Fn(&[String]) -> Result<String, crate::composed::ReadError>,
    write: Write<'_>,
) -> Answer {
    if args.is_empty() {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content <list|show|ref|search|dep list|label list|events|comments|gate list|memories|state|config get|comments add|label add|label remove|note|close|update|create|dep add|dep remove|dep relate|set-state|duplicate|recompute-blocked|gate discover|gate check|gate resolve|kv get|kv set|kv clear|remember|forget|recall> …\n".into(), ..Default::default() };
    }
    let kind = match check(args) {
        Ok(k) => k,
        Err(e) => return Answer { code: REFUSED, stderr: format!("spira-lc content: {e}\n"), ..Default::default() },
    };
    let mut body = match kind {
        Kind::Read => {
            return match read(args) {
                Err(e) if e.cannot_tell => Answer { code: CANNOT_TELL, stderr: format!("spira-lc content: cannot tell: {}\n", e.text.trim_end()), ..Default::default() },
                res => answer_of(res.map_err(|e| e.text), String::new()),
            }
        }
        Kind::Write { body: false } => None,
        Kind::Write { body: true } => match stdin() {
            Ok(b) => Some(b),
            Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc content: stdin: {e}\n"), ..Default::default() },
        },
    };
    let mut args = args.to_vec();
    let log = match guard(&mut args, &mut body) {
        Ok(log) => log,
        Err(refused) => return refused,
    };
    let res = write(&args, body.as_deref());
    let report = match &res {
        Ok(out) => after(&args, out),
        Err(_) => String::new(),
    };
    let mut ans = answer_of(res, format!("{log}{report}"));
    if ans.code == 0 && report.contains(NOT_MIRRORED) {
        ans.code = CANNOT_TELL;
    }
    ans
}

fn answer_of(res: Result<String, String>, stderr: String) -> Answer {
    match res {
        Ok(out) => Answer { stdout: out, stderr, ..Default::default() },
        Err(e) => Answer { code: 1, stderr: format!("{stderr}{}\n", e.trim_end()), ..Default::default() },
    }
}

/// The labels a bd alert item carried before alerts became control-plane conditions: the alert
/// itself and the watchdog's old write-probe item.
pub const ALERT_LABELS: &[&str] = &["alert", "heimdall:probe"];
const DELETE_CHUNK: usize = 100;

/// `content purge-alerts [--apply]`: every bd item carrying an [`ALERT_LABELS`] label. A dry
/// run reports the count; `--apply` writes each item as one JSONL line to a fresh file under
/// `export_dir` and only then deletes them through `delete`. With none left, nothing is written,
/// so a second run changes nothing.
pub fn purge_alerts(
    args: &[String],
    bd: &dyn Fn(&[String]) -> Result<String, String>,
    delete: &dyn Fn(&[String]) -> Result<(), String>,
    export_dir: &std::path::Path,
    now: i64,
) -> Answer {
    let apply = match args {
        [] => false,
        [a] if a == "--apply" => true,
        _ => return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc content purge-alerts [--apply]\n".into(), ..Default::default() },
    };
    let cannot = |why: String| Answer { code: CANNOT_TELL, stderr: format!("spira-lc content purge-alerts: {why}\n"), ..Default::default() };
    let mut items: Vec<serde_json::Value> = Vec::new();
    for label in ALERT_LABELS {
        let argv: Vec<String> = ["list", "--all", "--limit", "0", "--label", label, "--json"].iter().map(|s| s.to_string()).collect();
        let out = match bd(&argv) {
            Ok(o) => o,
            Err(e) => return cannot(format!("bd list --label {label} failed: {}", e.trim())),
        };
        let text = out.find(['[', '{']).map_or("[]", |i| &out[i..]);
        let rows = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(o @ serde_json::Value::Object(_)) => vec![o],
            _ => return cannot(format!("bd list --label {label} printed no JSON list")),
        };
        for r in rows {
            if r.get("id").and_then(|i| i.as_str()).is_some() && !items.iter().any(|x| x["id"] == r["id"]) {
                items.push(r);
            }
        }
    }
    let ids: Vec<String> = items.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    if !apply || ids.is_empty() {
        let note = if apply { "" } else { " (dry run; --apply to export and delete)" };
        return Answer { stdout: format!("purge-alerts: {} bd alert item(s){note}", ids.len()), ..Default::default() };
    }
    let file = export_dir.join(format!("purge-alerts-{now}.jsonl"));
    let lines: String = items.iter().map(|r| format!("{r}\n")).collect();
    let fs = crate::disk::fs();
    let written = fs.create_dir(export_dir).and_then(|()| fs.create_new(&file, lines.as_bytes())).and_then(|()| fs.sync_file(&file));
    if let Err(e) = written {
        return cannot(format!("{}: {e} — nothing deleted", file.display()));
    }
    for chunk in ids.chunks(DELETE_CHUNK) {
        if let Err(e) = delete(chunk) {
            return cannot(format!("delete failed after exporting to {}: {} — re-run to finish", file.display(), e.trim()));
        }
    }
    Answer { stdout: format!("purge-alerts: exported {} bd alert item(s) to {} and deleted them", ids.len(), file.display()), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test_id(tgs1bm7)]
    #[test]
    fn a_filing_under_a_parent_or_of_a_closed_insight_is_served() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(check(&a(&["create", "T", "-l", "x", "--parent", "sp-p", "--no-inherit-labels", "--silent"])), Ok(Kind::Write { body: false }));
        assert_eq!(check(&a(&["create", "T", "--type", "insight", "--status", "closed", "--body-file", "-"])), Ok(Kind::Write { body: true }));
        assert!(check(&a(&["create", "T", "--status"])).is_err(), "a status flag needs its value");
    }

    /// A bd holding items by label; `delete` removes them.
    struct Bd(RefCell<BTreeMap<String, (String, serde_json::Value)>>);

    impl Bd {
        fn new(items: &[(&str, &str)]) -> Self {
            Bd(RefCell::new(items.iter().map(|(id, l)| (id.to_string(), (l.to_string(), serde_json::json!({"id": id, "labels": [l], "title": format!("{id} fired")})))).collect()))
        }
        fn list(&self, a: &[String]) -> Result<String, String> {
            let label = a.iter().position(|x| x == "--label").map(|i| a[i + 1].clone()).unwrap();
            let rows: Vec<serde_json::Value> = self.0.borrow().values().filter(|(l, _)| *l == label).map(|(_, r)| r.clone()).collect();
            Ok(format!("note: bd says hello\n{}", serde_json::Value::Array(rows)))
        }
        fn delete(&self, ids: &[String]) -> Result<(), String> {
            for i in ids {
                self.0.borrow_mut().remove(i);
            }
            Ok(())
        }
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(toq6vwd)]
    #[test]
    fn a_tick_driven_caller_is_refused_the_whole_history_with_exit_3_named() {
        let whole = argv(&["list", "--all", "--limit", "0", "--json"]);
        for caller in ["spira-runtime", "spira-runtime/list", "cockpit-collect/list", "panel"] {
            let r = refuse_whole_history(&whole, caller).unwrap_or_else(|| panic!("{caller} was served the whole history"));
            assert_eq!(r.code, REFUSED);
            assert!(r.stderr.contains("exit 3") && r.stderr.contains(caller), "{}", r.stderr);
        }
    }

    #[test_id(thccm0f)]
    #[test]
    fn other_callers_and_bounded_reads_are_not_refused() {
        let whole = argv(&["list", "--all", "--limit", "0", "--json"]);
        assert!(refuse_whole_history(&whole, "spira-lc").is_none(), "an operator's own read stands");
        for bounded in [&["list", "--limit", "0", "--json"][..], &["list", "--all", "--limit", "0", "--id", "sp-a"], &["list", "--all", "--limit", "0", "--parent", "sp-e"], &["list", "--all", "--limit", "0", "--closed-after", "2026-11-01"], &["list", "--all", "--json"]] {
            assert!(refuse_whole_history(&argv(bounded), "spira-runtime/list").is_none(), "{bounded:?}");
        }
        assert!(refuse_whole_history(&argv(&["list", "--all", "--limit", "0", "--label-any", "x"]), "spira-runtime").is_some(), "a label can match every bead");
    }

    #[test_id(tdz7i4e)]
    #[test]
    fn purge_alerts_reports_dry_then_exports_before_deleting_and_a_second_run_changes_nothing() {
        let dir = testkit::TempDir::new("purge-alerts");
        let bd = Bd::new(&[("sp-a1", "alert"), ("sp-a2", "alert"), ("sp-p", "heimdall:probe"), ("sp-w", "plan")]);
        let deleted = RefCell::new(Vec::new());
        let del = |ids: &[String]| {
            assert!(std::fs::read_dir(dir.path()).unwrap().count() == 1, "exported before any delete");
            deleted.borrow_mut().extend(ids.iter().cloned());
            bd.delete(ids)
        };
        let dry = purge_alerts(&[], &|a| bd.list(a), &|_| panic!("a dry run deletes nothing"), dir.path(), 7);
        assert_eq!((dry.code, dry.stdout.contains("3 bd alert item(s) (dry run")), (0, true), "{dry:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "a dry run writes nothing");

        let applied = purge_alerts(&v(&["--apply"]), &|a| bd.list(a), &del, dir.path(), 7);
        assert_eq!(applied.code, 0, "{applied:?}");
        let export = std::fs::read_to_string(dir.join("purge-alerts-7.jsonl")).unwrap();
        let ids: Vec<String> = export.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["sp-a1", "sp-a2", "sp-p"]);
        assert_eq!(*deleted.borrow(), ids);
        assert!(bd.0.borrow().contains_key("sp-w"), "a work bead is never touched");

        let again = purge_alerts(&v(&["--apply"]), &|a| bd.list(a), &|_| panic!("nothing left to delete"), dir.path(), 8);
        assert!(again.stdout.contains("0 bd alert item(s)"), "{again:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "a second run writes nothing");
    }

    #[test_id(t5fkjs8)]
    #[test]
    fn purge_alerts_deletes_nothing_when_its_export_file_already_exists() {
        let dir = testkit::TempDir::new("purge-alerts-exists");
        std::fs::write(dir.join("purge-alerts-7.jsonl"), "earlier export\n").unwrap();
        let bd = Bd::new(&[("sp-a1", "alert")]);
        let a = purge_alerts(&v(&["--apply"]), &|a| bd.list(a), &|_| panic!("must not delete"), dir.path(), 7);
        assert_eq!(a.code, CANNOT_TELL, "{a:?}");
        assert!(a.stderr.contains("nothing deleted") || a.stdout.contains("nothing deleted"), "{a:?}");
        assert_eq!(std::fs::read_to_string(dir.join("purge-alerts-7.jsonl")).unwrap(), "earlier export\n", "the earlier export is not overwritten");
    }

    #[test_id(tsnm452)]
    #[test]
    fn purge_alerts_deletes_nothing_when_bd_cannot_be_read() {
        let dir = testkit::TempDir::new("purge-alerts-down");
        let a = purge_alerts(&v(&["--apply"]), &|_| Err("down".into()), &|_| panic!("must not delete"), dir.path(), 1);
        assert_eq!(a.code, CANNOT_TELL);
        assert_eq!(purge_alerts(&v(&["--force"]), &|_| panic!(), &|_| panic!(), dir.path(), 1).code, CANNOT_TELL);
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test_id(tjwb0yg)]
    #[test]
    fn reads_and_the_task_writes_pass() {
        for (ok, kind) in [
            (&["list", "--all", "--json"][..], Kind::Read),
            (&["show", "sp-a", "--json"], Kind::Read),
            (&["ref", "github:o/r#1", "--json"], Kind::Read),
            (&["comments", "sp-a", "--json"], Kind::Read),
            (&["gate", "list", "--json"], Kind::Read),
            (&["state", "sp-a", "rune"], Kind::Read),
            (&["memories", "--json"], Kind::Read),
            (&["search", "merge queue", "--json"], Kind::Read),
            (&["dep", "list", "sp-a", "--direction=up", "--json"], Kind::Read),
            (&["label", "list", "sp-a", "--json"], Kind::Read),
            (&["events", "--id", "sp-a,sp-b"], Kind::Read),
            (&["comments", "add", "sp-a", "text"], Kind::Write { body: false }),
            (&["update", "sp-a", "--add-label", "x", "--remove-label", "y"], Kind::Write { body: false }),
            (&["update", "sp-a", "--set-metadata", "k=v", "--add-label", "x"], Kind::Write { body: false }),
            (&["label", "add", "sp-a", "x"], Kind::Write { body: false }),
            (&["label", "remove", "sp-a", "x"], Kind::Write { body: false }),
            (&["note", "sp-a", "text"], Kind::Write { body: false }),
            (&["note", "sp-a", "--stdin"], Kind::Write { body: true }),
            (&["close", "sp-a"], Kind::Write { body: false }),
            (&["close", "sp-a", "--reason", "r"], Kind::Write { body: false }),
            (&["dep", "add", "sp-a", "sp-b"], Kind::Write { body: false }),
            (&["dep", "add", "sp-a", "sp-b", "--type", "blocks"], Kind::Write { body: false }),
            (&["dep", "remove", "sp-a", "sp-b"], Kind::Write { body: false }),
            (&["dep", "relate", "sp-a", "sp-b"], Kind::Write { body: false }),
            (&["set-state", "sp-a", "branch=spira/sp-a"], Kind::Write { body: false }),
            (&["set-state", "sp-a", "lane=builder"], Kind::Write { body: false }),
            (&["update", "sp-a", "--priority", "1"], Kind::Write { body: false }),
            (&["update", "sp-a", "-p", "2"], Kind::Write { body: false }),
            (&["update", "sp-a", "--body-file", "-", "--force-claimed", "why"], Kind::Write { body: true }),
            (&["update", "sp-a", "--assignee", ""], Kind::Write { body: false }),
            (&["update", "sp-a", "--assignee", "dwarf-1", "--force"], Kind::Write { body: false }),
            (&["duplicate", "sp-a", "--of", "sp-b"], Kind::Write { body: false }),
            (&["recompute-blocked"], Kind::Write { body: false }),
            (&["gate", "discover", "--branch", "b", "--repo", "spira"], Kind::Write { body: false }),
            (&["gate", "check", "--type", "gh:run"], Kind::Write { body: false }),
            (&["gate", "check", "--type=gh:run"], Kind::Write { body: false }),
            (&["gate", "resolve", "sp-g"], Kind::Write { body: false }),
            (&["kv", "get", "spira.watch.sp-e"], Kind::Read),
            (&["kv", "get", "spira.watch.sp-e", "--json"], Kind::Read),
            (&["kv", "set", "spira.watch.sp-e", "a@b"], Kind::Write { body: false }),
            (&["kv", "clear", "spira.watch.sp-e"], Kind::Write { body: false }),
            (&["remember", "--key", "sop-x"], Kind::Write { body: true }),
            (&["forget", "sop-x"], Kind::Write { body: false }),
            (&["recall", "sop-x"], Kind::Read),
            (&["create", "t"], Kind::Write { body: false }),
            (&["create", "T", "-t", "bug", "-p", "2", "--labels", "a,b", "--external-ref", "gh-1", "--body-file", "-", "--silent"], Kind::Write { body: true }),
            (&["create", "--parent", "sp-e", "--silent", "--title", "T"], Kind::Write { body: false }),
        ] {
            assert_eq!(check(&v(ok)), Ok(kind), "{ok:?}");
        }
    }

    #[test_id(tbi3kzl)]
    #[test]
    fn a_read_the_door_does_not_serve_is_refused_naming_its_exit() {
        for (args, exit) in [
            (&["ready"][..], "spira-lc claimable"),
            (&["blocked"], "blocked_by"),
            (&["history", "sp-a"], "fact list"),
            (&["children", "sp-a"], "--parent"),
            (&["sql", "select 1"], "typed read"),
        ] {
            let e = check(&v(args)).unwrap_err();
            assert!(e.contains(exit), "{args:?}: {e}");
        }
    }

    #[test_id(t2pqr1d)]
    #[test]
    fn every_other_write_is_refused() {
        for bad in [
            &["close", "sp-a", "--reason", "r", "--force"][..],
            &["close", "sp-a", "--reason", " "],
            &["close", "--force"],
            &["update", "sp-a", "--status", "open"],
            &["update", "sp-a", "--add-label"],
            &["update", "sp-a", "--set-metadata", "novalue"],
            &["update", "sp-a", "--set-metadata", "=v"],
            &["update", "sp-a"],
            &["label", "add", "sp-a"],
            &["label", "list", "sp-a", "x"],
            &["note", "sp-a", "--file", "x"],
            &["note", "sp-a", " "],
            &["create"],
            &["create", "--type", "task"],
            &["create", "T", "--body-file", system_paths::ETC_PASSWD],
            &["create", "T", "--force"],
            &["comments", "add", "sp-a"],
            &["delete", "sp-a"],
            &["reopen", "sp-a"],
            &["set-state", "sp-a", "rune=x"],
            &["dep", "add", "sp-a"],
            &["dep", "add", "sp-a", "sp-b", "--type", "bogus"],
            &["update", "sp-a", "--priority", "9"],
            &["update", "sp-a", "--force-claimed", "why"],
            &["set-state", "sp-a", "branch="],
            &["update", "sp-a", "-p", "5"],
            &["update", "sp-a", "--force"],
            &["duplicate", "sp-a"],
            &["gate", "discover", "--branch", "b"],
            &["kv", "set", crate::plan::KV_KEY, "1"],
            &["kv", "clear", crate::plan::KV_KEY],
            &["remember", "--key", "-x"],
            &["update", "sp-a", "--body-file", "-", "--force-claimed", " "],
            &["dep", "add", "sp-a", "sp-b", "--type"],
            &[][..],
            &["gate", "resolve"],
            &["gate", "check"],
            &["gate", "resolve", "-x"],
        ] {
            assert!(check(&v(bad)).is_err(), "{bad:?}");
        }
    }

    /// The lifecycle machine as the guard reads it: `show <id>` answers `row`, or exit 1.
    struct Rows(Option<serde_json::Value>);

    impl crate::callers::Machine for Rows {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            assert_eq!(args.first().map(String::as_str), Some("show"), "the guard only reads a row: {args:?}");
            match &self.0 {
                Some(r) => (0, serde_json::json!({"bead": r, "delivery": null}).to_string()),
                None => (1, String::new()),
            }
        }
    }

    const NOW: i64 = 1_800_000_000;

    fn caller(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A write's verdict: admitted, or refused with an exit code and its text.
    type Verdict = Result<(), (i32, String)>;

    // literal-ok: test fixture label
    const ASK: &str = "needs-ryan";

    fn incident() -> Result<String, String> {
        Ok(INCIDENT.to_string())
    }

    // literal-ok: test fixture label
    const INCIDENT: &str = "alarm-x";

    fn no_policy() -> Result<bead::filing::Policy, String> {
        Ok(bead::filing::Policy::none())
    }

    fn registry() -> spira_config::repos::Registry {
        let map = "# name|root|base|gate\nspira|r/spira|main|gate.sh\ndeck|r/deck|main|gate.sh\nkiln|r/kiln|main|gate.sh\nmist|r/mist|main|gate.sh\n";
        spira_config::repos::Registry::new(Some(map), &BTreeMap::new(), std::path::Path::new("r/spira"))
    }

    fn row_of(row: Option<&serde_json::Value>) -> Option<spira_store::lc_state::Row> {
        row.and_then(|r| spira_store::lc_state::parse_show(&serde_json::json!({"bead": r}).to_string()).ok().flatten())
    }

    /// bdq's guards as `src/bin/bdq.rs` calls them, each at its own point and in its order: the
    /// create-time fences, the ask-label write fence and the state-verb fence; then the czar
    /// fence; then the claimed-description check — on the process environment and the row.
    fn through_bdq(args: &[String], body: Option<&str>, env: &BTreeMap<String, String>, row: Option<&serde_json::Value>) -> Verdict {
        let vars = |k: &str| env.get(k).cloned();
        let rows = |_: &str| row_of(row);
        let ask = || Ok::<_, String>(ASK.to_string());
        let ctx = bead::guard::Ctx { who: "bdq", vars: &vars, rows: &|_| None, now: 0, ask_label: &ask, registry: &registry };
        let err = |r: bead::guard::Refusal| (r.code, r.text);
        bead::guard::create(args, body, &ctx).map_err(err)?;
        bead::guard::ask_label_write(args, &ctx).map_err(err)?;
        bead::guard::state_verb(args, &vars, "bdq").map_err(err)?;
        bead::guard::czar(args, &vars).map_err(err)?;
        bead::guard::claim(args, &rows, NOW).map(|_| ()).map_err(err)
    }

    /// The content door's guard, as the server runs it: the caller's variables off the request,
    /// the row off the machine.
    fn through_content(args: &[String], body: Option<&str>, env: &BTreeMap<String, String>, row: Option<&serde_json::Value>) -> Verdict {
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        guard(&mut args.to_vec(), &mut body.map(str::to_string), &g, &mut Rows(row.cloned())).map(|_| ()).map_err(|a| (a.code, a.stderr))
    }

    fn czar(stage: Option<&str>) -> BTreeMap<String, String> {
        let mut c = caller(&[("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "loop-stalled"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-t")]);
        if let Some(s) = stage {
            c.insert("SPIRA_CZAR_STAGE_LOOP_STALLED".into(), s.into());
        }
        c
    }

    const WHOLE_ASK: &str = "## Question\nWhich?\nDefault: a\nClass: policy\n";

    #[test_id(t6kbgrx)]
    #[test]
    fn the_content_guard_reaches_bdqs_verdict_on_every_write() {
        let with_override = |mut c: BTreeMap<String, String>| {
            c.insert("SPIRA_BDQ_STATE_WRITE".into(), "landing".into());
            c
        };
        let contexts = [
            ("no rune", caller(&[])),
            ("state override", with_override(caller(&[]))),
            ("builder", caller(&[("SPIRA_RUNE", "builder"), ("SPIRA_CZAR_CLASS", "loop-stalled")])),
            ("czar, class unset", caller(&[("SPIRA_RUNE", "czar")])),
            ("czar shadow (unset)", czar(None)),
            ("czar shadow, state override", with_override(czar(Some("shadow")))),
            ("czar act, state override", with_override(czar(Some("act")))),
            ("czar unknown stage", czar(Some("loud"))),
        ];
        let held = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW + 600});
        let lapsed = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW - 1});
        let ready = serde_json::json!({"bead_id": "sp-a", "state": "READY"});
        let rows = [None, Some(&held), Some(&lapsed), Some(&ready)];
        let writes: &[(&[&str], Option<&str>)] = &[
            (&["close", "sp-a"], None),
            (&["close", "sp-a", "--reason", "r"], None),
            (&["close", "sp-t"], None),
            (&["update", "sp-a", "--add-label", "x"], None),
            (&["update", "sp-a", "--add-label", ASK], None),
            (&["update", "sp-a", "--remove-label", ASK], None),
            (&["update", "sp-a", "--set-metadata", "k=v"], None),
            (&["update", "sp-t", "--set-metadata", "k=v"], None),
            (&["update", "sp-a", "--title", "T"], None),
            (&["update", "sp-a", "--body-file", "f"], None),
            (&["update", "sp-t", "--title", "T"], None),
            (&["update", "sp-a", "--status", "open"], None),
            (&["label", "add", "sp-a", "x"], None),
            (&["label", "add", "sp-a", ASK], None),
            (&["label", "remove", "sp-a", ASK], None),
            (&["note", "sp-a", "t"], None),
            (&["note", "sp-a", "--stdin"], Some("world.sh stop")),
            (&["comments", "add", "sp-a", "t"], None),
            (&["create", "T", "--type", "task"], None),
            (&["create", "T", "--labels", "repo:deck"], None),
            (&["create", "T", "--labels", "repo:nowhere"], None),
            (&["create", "Stop it", "--description", "run world.sh stop first"], None),
            (&["create", "Stop it", "--labels", ASK, "--description", "run world.sh stop first"], None),
            (&["create", "Pick", "--labels", ASK], None),
            (&["create", "Pick", "--labels", ASK, "--body-file", "-"], Some(WHOLE_ASK)),
            (&["create", "Pick", "--labels", ASK, "--description", "x", "--body-file", "-"], Some(WHOLE_ASK)),
            (&["create", "Body", "--body-file", "-"], Some("run world.sh stop first")),
            (&["create", "Fix", "--description", "DELETE FROM schema_migrations"], None),
            (&["reopen", "sp-a"], None),
        ];
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (name, env) in &contexts {
            for row in rows {
                for (w, body) in writes {
                    let args = v(w);
                    let (bdq, content) = (through_bdq(&args, *body, env, row), through_content(&args, *body, env, row));
                    assert_eq!(content, bdq, "{name}, row {row:?}: {w:?} body {body:?}");
                    let kind = match &bdq {
                        Ok(()) => "admitted",
                        Err((1, t)) if t.starts_with("czar-fence: ") => "czar",
                        Err((1, t)) if t.contains("is claimed by dwarf-sp-a") => "claim",
                        Err((1, t)) if t.starts_with("spira: bdq refuses") => "state",
                        Err((1, t)) if t.starts_with("spira: refusing to add") => "ask-label",
                        Err((1, t)) if t.contains("is not in the repo map") => "repo-label",
                        Err((1, t)) if t.contains("DELETE FROM schema_migrations") => "schema-delete",
                        Err((1, t)) if t.contains(ASK) => "ask-shape",
                        Err(other) => panic!("a refusal no guard gives: {other:?}"),
                    };
                    *seen.entry(kind).or_default() += 1;
                }
            }
        }
        let kinds: Vec<&str> = seen.keys().copied().collect();
        assert_eq!(kinds, ["admitted", "ask-label", "ask-shape", "claim", "czar", "repo-label", "schema-delete", "state"], "every guard refuses somewhere in the table: {seen:?}");
        assert_eq!(
            through_content(&v(&["update", "sp-a", "--add-label", "x"]), None, &czar(None), None),
            Err((1, "czar-fence: loop-stalled is shadow — mutation refused (SPIRA_CZAR_STAGE_LOOP_STALLED=act to enable)\n".into()))
        );
        assert_eq!(through_content(&v(&["update", "sp-t", "--add-label", "x"]), None, &czar(None), None), Ok(()), "the trigger bead itself passes");
    }

    use bead::filing::{CheckRun, Contract, Persona, Policy, Statutes};

    // literal-ok: test fixture vocabulary, not the configured names
    const MIMIR: &str = "mimir-sweep";

    fn filing_policy() -> Policy {
        let persona = |name: &str, labels: &[&str], spec: bool, unresolved: Option<&str>| Persona {
            name: name.into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            requires_build_spec: spec,
            unresolved: unresolved.map(str::to_string),
        };
        Policy {
            personas: vec![
                persona("builder", &["builder-lane"], true, None),
                persona("scout", &["scout-lane", MIMIR], false, None),
                persona("broken", &["broken-lane"], false, Some("broken.rune: RUNE_LABELS references $X unguarded, and it resolved empty — refusing")),
            ],
            repos: bead::repos_by_name("spira | /r/spira | push | origin/main | | true | self\ndeck | /r/deck | push | origin/main | | true | develop\nkiln | /r/kiln | push | origin/main | | true | develop\nmist | /r/mist | push | origin/main | | true | develop\n"),
            identity: bead::identity::parse("spira | spira-only\ndeck | pokedex\n"),
            statutes: Statutes::Registry { path: "statutes".into(), text: "law-x | refuse | forbidden plan\nlaw-y | show | allocator\n".into() },
            contract: Contract::new("spira", |repo| match repo {
                "kiln" => Ok(CheckRun { code: Some(1), stdout: "met environment: ok\nnot met submit-checks: .spira/checks.toml does not exist\n".into(), stderr: String::new() }),
                "mist" => Ok(CheckRun { code: Some(2), stdout: String::new(), stderr: "no such repo\n".into() }),
                _ => Ok(CheckRun { code: Some(0), ..Default::default() }),
            }),
        }
    }

    fn filing_guard(args: &[String], body: Option<&str>, env: &BTreeMap<String, String>) -> (Verdict, Vec<String>, Option<String>) {
        let ask = || Ok::<_, String>(ASK.to_string());
        let policy = || Ok(filing_policy());
        let g = Guards { caller: env, ask_label: &ask, registry: &registry, policy: &policy, repo_of: &|id| (id == "sp-a").then(|| "spira".to_string()), target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let (mut args, mut body) = (args.to_vec(), body.map(str::to_string));
        let verdict = guard(&mut args, &mut body, &g, &mut Rows(None)).map(|_| ()).map_err(|a| (a.code, a.stderr));
        (verdict, args, body)
    }

    /// A filing as `bead.sh file --for <persona> --repo <repo>` judges it, in `cmd_file`'s order:
    /// identity, statute, the repo it requires, then the persona's build spec and lane.
    fn through_beadsh(persona: &str, repo: Option<&str>, title: &str, body: &str, env: &BTreeMap<String, String>, fix: bool) -> Verdict {
        let policy = filing_policy();
        let vars = |k: &str| env.get(k).cloned();
        let err = |r: bead::guard::Refusal| (r.code, r.text);
        let persona = policy.personas.iter().find(|p| p.name == persona).unwrap();
        if let Some(repo) = repo {
            bead::filing::identity_guard(&policy.identity, repo, &format!("{title}\n{body}"), &vars).map_err(err)?;
        }
        bead::filing::statute_guard(&policy.statutes, &format!("{title}\n{body}"), &vars).map_err(err)?;
        if let Some(why) = &persona.unresolved {
            return Err((2, format!("bead: {why}\n")));
        }
        let Some(repo) = repo else { return Err((2, "bead: --repo <name> required\n".into())) };
        bead::filing::persona_guards(persona, repo, body, &policy.repos, false).map_err(err)?;
        let labels = bead::with_contract_fix(format!("repo:{repo}"), fix).split(',').map(str::to_string).collect::<Vec<_>>();
        bead::filing::contract_guard(repo, &labels, &policy.contract, &vars).map_err(err)?;
        Ok(())
    }

    fn through_the_door(persona: &str, repo: Option<&str>, title: &str, body: &str, env: &BTreeMap<String, String>, fix: bool) -> Verdict {
        let policy = filing_policy();
        let mut labels = policy.personas.iter().find(|p| p.name == persona).unwrap().labels.clone();
        labels.extend(repo.map(|r| format!("repo:{r}")));
        labels.extend(fix.then(|| spira_config::scoped_stop::CONTRACT_FIX_LABEL.to_string()));
        filing_guard(&v(&["create", title, "--labels", &labels.join(","), "--body-file", "-"]), Some(body), env).0
    }

    /// A case: its name, persona, repo, body and the caller's variables.
    type Filing<'a> = (&'a str, &'a str, Option<&'a str>, String, BTreeMap<String, String>);

    #[test_id(t3hfwau)]
    #[test]
    fn a_filing_is_refused_through_the_socket_with_the_text_bead_sh_refuses_it_with() {
        const SPEC: &str = "## Build\nthe artifact, and where\n";
        let over = |k: &str, v: &str| caller(&[(k, v)]);
        let cases: Vec<Filing> = vec![
            ("no build spec", "builder", Some("deck"), "just an outcome".into(), caller(&[])),
            ("empty build section", "builder", Some("deck"), "## Build\n\n## Notes\nx".into(), caller(&[])),
            ("build spec", "builder", Some("deck"), SPEC.into(), caller(&[])),
            ("lane not admitted", "scout", Some("deck"), "x".into(), caller(&[])),
            ("lane not admitted, override set", "scout", Some("deck"), "x".into(), over("SPIRA_BEAD_LANE_OVERRIDE", "1")),
            ("lane admitted", "scout", Some("spira"), "x".into(), caller(&[])),
            ("no repo", "builder", None, SPEC.into(), caller(&[])),
            ("foreign identity", "builder", Some("spira"), format!("{SPEC}uses pokedex"), caller(&[])),
            ("foreign identity, overridden", "builder", Some("spira"), format!("{SPEC}uses pokedex"), over("BEAD_CROSS_REPO_CONSIDERED", "shared kit")),
            ("own identity", "builder", Some("deck"), format!("{SPEC}uses pokedex"), caller(&[])),
            ("contradicted statute", "builder", Some("deck"), format!("{SPEC}forbidden plan"), caller(&[])),
            ("contradicted statute, overridden", "builder", Some("deck"), format!("{SPEC}forbidden plan"), over("BEAD_STATUTE_CONSIDERED", "it does not apply")),
            ("touched statute", "builder", Some("deck"), format!("{SPEC}the allocator"), caller(&[])),
            ("identity before statute", "builder", Some("spira"), format!("{SPEC}pokedex forbidden plan"), caller(&[])),
            ("statute before build spec", "builder", Some("deck"), "forbidden plan".into(), caller(&[])),
            ("unresolved persona", "broken", Some("deck"), "x".into(), caller(&[])),
            ("contract not met", "builder", Some("kiln"), SPEC.into(), caller(&[])),
            ("contract not met, contract-fix", "builder", Some("kiln"), SPEC.into(), caller(&[])),
            ("contract cannot tell", "builder", Some("mist"), SPEC.into(), caller(&[])),
        ];
        let mut refused = 0;
        for (name, persona, repo, body, env) in &cases {
            let fix = name.ends_with("contract-fix");
            let (beadsh, door) = (through_beadsh(persona, *repo, "T", body, env, fix), through_the_door(persona, *repo, "T", body, env, fix));
            assert_eq!(door, beadsh, "{name}");
            refused += usize::from(beadsh.is_err());
        }
        assert_eq!(refused, 12, "the table refuses where it should: an unrefused filing proves nothing");
        let refusal = |name: &str| through_the_door(cases.iter().find(|c| c.0 == name).unwrap().1, cases.iter().find(|c| c.0 == name).unwrap().2, "T", &cases.iter().find(|c| c.0 == name).unwrap().3, &cases.iter().find(|c| c.0 == name).unwrap().4, false).unwrap_err();
        assert_eq!(refusal("no repo"), (2, "bead: --repo <name> required\n".to_string()));
        assert!(refusal("no build spec").1.starts_with("bead: refusing: a bead for builder must name what to build under a `## Build` heading"));
        // literal-ok: the override's name is bead.sh's refusal text
        assert!(refusal("lane not admitted").1.contains("bead: repo:deck does not admit lane mimir-sweep — refused by its admitted lanes (plan,incident,groom,spike)\nbead: override: SPIRA_BEAD_LANE_OVERRIDE=1"));
        assert!(refusal("foreign identity").1.contains("\"pokedex\" belongs to deck"));
        assert!(refusal("contradicted statute").1.contains("law-x"));
        assert!(refusal("contract not met").1.contains("not met submit-checks: .spira/checks.toml does not exist"));
        assert!(refusal("contract cannot tell").1.contains("cannot tell"));
    }

    #[test_id(tcrkbg3)]
    #[test]
    fn an_admitted_override_is_recorded_ahead_of_the_filings_body() {
        let env = caller(&[("BEAD_CROSS_REPO_CONSIDERED", "shared kit"), ("BEAD_STATUTE_CONSIDERED", "n/a")]);
        let args = v(&["create", "T", "--labels", "builder-lane,repo:spira", "--body-file", "-"]);
        let (verdict, _, body) = filing_guard(&args, Some("## Build\nx\npokedex forbidden plan"), &env);
        assert_eq!(verdict, Ok(()));
        assert_eq!(body.as_deref(), Some("Statute check: law-x (override: n/a)\n\nCross-repo considered: shared kit\n\n## Build\nx\npokedex forbidden plan"), "statute line first, as bead.sh puts it");
        let args = v(&["create", "T", "--labels", "builder-lane,repo:spira", "--description", "## Build\nx\nthe allocator"]);
        let (verdict, args, body) = filing_guard(&args, None, &caller(&[]));
        assert_eq!((verdict, body), (Ok(()), None));
        assert!(args.last().unwrap().starts_with("Statute check: law-y\n\n## Build"), "{args:?}");
    }

    #[test_id(t2j14eb)]
    #[test]
    fn a_create_that_names_no_personas_partition_is_not_a_filing() {
        let env = caller(&[]);
        for labels in ["repo:spira", "other,repo:deck"] {
            let args = v(&["create", "T", "--labels", labels, "--body-file", "-"]);
            let (verdict, _, body) = filing_guard(&args, Some("pokedex forbidden plan"), &env);
            assert_eq!((verdict, body.as_deref()), (Ok(()), Some("pokedex forbidden plan")), "{labels}");
        }
    }

    #[test_id(tdrw90v)]
    #[test]
    fn a_partition_label_is_refused_through_the_socket_with_the_text_bead_sh_refuses_it_with() {
        let parts = filing_policy().partitions();
        assert_eq!(parts, ["builder-lane", "scout-lane", MIMIR, "broken-lane"]);
        let mut refused = 0;
        for (verb, label) in [("add", "builder-lane"), ("remove", "builder-lane"), ("add", "rune:builder"), ("add", MIMIR), ("add", "kit-bump"), ("remove", "kit-bump")] {
            let through_beadsh = bead::filing::partition_label(verb, label, &parts).map_err(|r| (r.code, r.text));
            let through_door = filing_guard(&v(&["label", verb, "sp-a", label]), None, &caller(&[])).0;
            assert_eq!(through_door, through_beadsh, "{verb} {label}");
            refused += usize::from(through_beadsh.is_err());
        }
        assert_eq!(refused, 4);
        assert_eq!(
            filing_guard(&v(&["label", "add", "sp-a", "builder-lane"]), None, &caller(&[])).0,
            Err((1, "bead: label add: refusing — builder-lane is a partition label; `bead.sh file --for <persona>` derives those from the persona's own predicate, so a hand-set one can disagree with it\n".into()))
        );
    }

    #[test_id(txchrd4)]
    #[test]
    fn text_written_onto_a_bead_meets_its_repos_identity_and_the_statutes() {
        let none = caller(&[]);
        let refused = |args: &[&str], body: Option<&str>| filing_guard(&v(args), body, &none).0.unwrap_err().1;
        assert!(refused(&["note", "sp-a", "see pokedex"], None).contains("\"pokedex\" belongs to deck"));
        assert!(refused(&["note", "sp-a", "--stdin"], Some("see pokedex")).contains("\"pokedex\" belongs to deck"));
        assert!(refused(&["update", "sp-a", "--body-file", "-"], Some("pokedex")).contains("belongs to deck"));
        assert!(refused(&["note", "sp-a", "forbidden plan"], None).contains("law-x"));
        assert_eq!(filing_guard(&v(&["note", "sp-a", "spira-only is fine"]), None, &none).0, Ok(()));
        let admitted = filing_guard(&v(&["note", "sp-a", "the allocator"]), None, &none);
        assert_eq!((admitted.0, admitted.1[2].as_str()), (Ok(()), "Statute check: law-y\n\nthe allocator"));
        let overridden = filing_guard(&v(&["update", "sp-a", "--body-file", "-"]), Some("pokedex"), &caller(&[("BEAD_CROSS_REPO_CONSIDERED", "kit")]));
        assert_eq!((overridden.0, overridden.2.as_deref()), (Ok(()), Some("Cross-repo considered: kit\n\npokedex")));
    }

    #[test_id(t6bhfjc)]
    #[test]
    fn the_door_serves_a_description_replacement_only_from_the_request_body() {
        assert_eq!(check(&v(&["update", "sp-a", "--body-file", "-"])), Ok(Kind::Write { body: true }));
        assert!(check(&v(&["update", "sp-a", "--body-file", system_paths::ETC_PASSWD])).is_err());
        assert!(takes_body(&v(&["update", "sp-a", "--body-file", "-"])));
    }

    #[test_id(tdss33k)]
    #[test]
    fn a_claimed_bead_refuses_a_description_replacement_through_the_door() {
        let held = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW + 600});
        let verdict = through_content(&v(&["update", "sp-a", "--body-file", "-"]), Some("x"), &caller(&[]), Some(&held));
        assert!(verdict.unwrap_err().1.contains("is claimed by dwarf-sp-a"));
    }

    fn blocks_edge_guard(args: &[&str], target: serde_json::Value, bd_ran: &std::cell::Cell<bool>) -> Answer {
        let env = caller(&[]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let shown = |_: &str| bead::parse_show_row(&target.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &shown, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(None));
        let ran = |_: &[String], _: Option<&str>| -> Result<String, String> {
            bd_ran.set(true);
            Ok(String::new())
        };
        let mut no_after = |_: &[String], _: &str| String::new();
        let mut body = || Ok::<String, String>(String::new());
        run(&v(args), &mut body, &mut gd, &mut no_after, &|_| Ok(String::new()), &ran)
    }

    fn parent_wait_guard(
        args: &[&str],
        child_labels: &[&str],
        parent: serde_json::Value,
    ) -> (Answer, Vec<Vec<String>>) {
        let env = caller(&[]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let child = serde_json::json!({"id": "sp-c", "issue_type": "task", "labels": child_labels, "parent": "sp-p"});
        let shown = |id: &str| {
            bead::parse_show_row(
                &if id == "sp-c" {
                    child.clone()
                } else {
                    parent.clone()
                }
                .to_string(),
            )
        };
        let g = Guards {
            caller: &env,
            ask_label: &ask,
            registry: &registry,
            policy: &no_policy,
            repo_of: &|_| None,
            target_of: &shown,
            incident_label: &incident,
            now: NOW,
            forced: Default::default(),
            cwd: Default::default(),
        };
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(None));
        let ran_args = std::cell::RefCell::new(Vec::new());
        let ran = |a: &[String], _: Option<&str>| -> Result<String, String> {
            ran_args.borrow_mut().push(a.to_vec());
            Ok(String::new())
        };
        let mut no_after = |_: &[String], _: &str| String::new();
        let mut body = || Ok::<String, String>(String::new());
        let ans = run(
            &v(args),
            &mut body,
            &mut gd,
            &mut no_after,
            &|_| Ok(String::new()),
            &ran,
        );
        (ans, ran_args.into_inner())
    }

    #[test_id(tpg3b7w)]
    #[test]
    fn a_blocks_edge_onto_the_items_own_parent_is_written_as_the_split_label_and_removed_with_it() {
        let label = spira_config::nonwork::SPLIT_LABEL;
        let task = serde_json::json!({"id": "sp-p", "issue_type": "task", "labels": []});
        for args in [
            &["dep", "add", "sp-c", "sp-p"][..],
            &["dep", "add", "sp-c", "sp-p", "--type", "blocks"],
        ] {
            let (ans, ran) = parent_wait_guard(args, &[], task.clone());
            assert_eq!(
                (ans.code, ran),
                (0, vec![v(&["label", "add", "sp-c", label])]),
                "{args:?}"
            );
        }
        let (ans, ran) =
            parent_wait_guard(&["dep", "remove", "sp-c", "sp-p"], &[label], task.clone());
        assert_eq!(
            (ans.code, ran),
            (0, vec![v(&["label", "remove", "sp-c", label])])
        );
    }

    #[test_id(t1o7j7d)]
    #[test]
    fn an_edge_that_is_not_a_wait_on_the_own_parent_passes_through_unrewritten() {
        let task = serde_json::json!({"id": "sp-p", "issue_type": "task", "labels": []});
        for (args, labels) in [
            (&["dep", "add", "sp-c", "sp-other"][..], &[][..]),
            (&["dep", "add", "sp-c", "sp-p", "--type", "relates-to"], &[]),
            (&["dep", "remove", "sp-c", "sp-p"], &[]),
        ] {
            let (_, ran) = parent_wait_guard(args, labels, task.clone());
            assert_eq!(ran, vec![v(args)], "{args:?}");
        }
    }

    #[test_id(t4txzum)]
    #[test]
    fn an_epic_or_incident_parent_is_refused_before_the_rewrite() {
        let epic = serde_json::json!({"id": "sp-p", "issue_type": "epic", "labels": []});
        let alarm = serde_json::json!({"id": "sp-p", "issue_type": "task", "labels": [INCIDENT]});
        let (ans, ran) = parent_wait_guard(&["dep", "add", "sp-c", "sp-p"], &[], epic);
        assert_eq!(
            (ans.code, ans.stderr, ran.len()),
            (
                1,
                format!("{}\n", bead::epic_blocks_refusal("sp-c", "sp-p")),
                0
            )
        );
        let (ans, ran) = parent_wait_guard(&["dep", "add", "sp-c", "sp-p"], &[], alarm);
        assert_eq!(
            (ans.code, ans.stderr, ran.len()),
            (
                1,
                format!(
                    "{}\n",
                    bead::incident_blocks_refusal("sp-c", "sp-p", INCIDENT)
                ),
                0
            )
        );
    }

    #[test_id(t112293)]
    #[test]
    fn a_blocks_edge_onto_an_epic_or_an_incident_is_refused_through_the_door() {
        let ran = std::cell::Cell::new(false);
        let epic = serde_json::json!({"id": "sp-b", "issue_type": "epic", "labels": []});
        let alarm = serde_json::json!({"id": "sp-b", "issue_type": "task", "labels": [INCIDENT]});
        for args in [&["dep", "add", "sp-a", "sp-b"][..], &["dep", "add", "sp-a", "sp-b", "--type", "blocks"]] {
            let ans = blocks_edge_guard(args, epic.clone(), &ran);
            assert_eq!((ans.code, ans.stderr), (1, format!("{}\n", bead::epic_blocks_refusal("sp-a", "sp-b"))), "{args:?}");
            let ans = blocks_edge_guard(args, alarm.clone(), &ran);
            assert_eq!((ans.code, ans.stderr), (1, format!("{}\n", bead::incident_blocks_refusal("sp-a", "sp-b", INCIDENT))), "{args:?}");
        }
        for args in [&["dep", "add", "sp-a", "sp-b", "--type", "parent-child"][..], &["dep", "add", "sp-a", "sp-b", "--type", "relates-to"]] {
            assert_eq!(blocks_edge_guard(args, epic.clone(), &ran).code, 0, "{args:?}: only a blocks edge is refused");
        }
        assert!(ran.get(), "the non-blocks edges reached bd");
        ran.set(false);
        blocks_edge_guard(&["dep", "add", "sp-a", "sp-b"], epic, &ran);
        assert!(!ran.get(), "a refused edge never reaches bd");
    }

    #[test_id(tko20w0)]
    #[test]
    fn a_dep_add_through_the_door_is_mirrored_to_the_lifecycle_store() {
        let mut m = Recorder(Vec::new());
        assert_eq!(after(&v(&["dep", "add", "sp-a", "sp-b", "--type", "blocks"]), "", &mut m, &mut NoBd, 0), "");
        assert_eq!(m.0[0], v(&["dep-add", "sp-a", "sp-b", "--type", "blocks"]));
        let mut m = Recorder(Vec::new());
        after(&v(&["dep", "remove", "sp-a", "sp-b"]), "", &mut m, &mut NoBd, 0);
        assert_eq!(m.0[0], v(&["dep-remove", "sp-a", "sp-b"]));
        let mut m = Recorder(Vec::new());
        after(&v(&["dep", "relate", "sp-a", "sp-b"]), "", &mut m, &mut NoBd, 0);
        assert!(m.0.iter().all(|c| !c[0].starts_with("dep-")), "a relate is no lifecycle edge: {:?}", m.0);
    }

    #[test_id(ti183ag)]
    #[test]
    fn a_dep_mirror_the_machine_refuses_is_reported_and_bds_write_stands() {
        struct Down;
        impl crate::callers::Machine for Down {
            fn call(&mut self, _: &[String]) -> (i32, String) {
                (1, "store down".into())
            }
        }
        let report = after(&v(&["dep", "add", "sp-a", "sp-b"]), "", &mut Down, &mut NoBd, 0);
        assert!(report.contains("dep-add sp-a -> sp-b is in bd but was not mirrored") && report.contains("store down"), "{report}");
        let ans = run(&v(&["dep", "add", "sp-a", "sp-b"]), &mut || Ok(String::new()), &mut |_, _| Ok(String::new()), &mut |a, _| after(a, "", &mut Down, &mut NoBd, 0), &|_| Ok(String::new()), &|_, _| Ok("Added".into()));
        assert_eq!(ans.code, CANNOT_TELL, "{ans:?}");
    }

    #[test_id(ttdn3pq)]
    #[test]
    fn a_forced_edit_through_the_door_is_acknowledged() {
        let held = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW + 600});
        let env = caller(&[]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let wrote = std::cell::RefCell::new(Vec::<String>::new());
        let write = |a: &[String], body: Option<&str>| -> Result<String, String> {
            wrote.borrow_mut().push(format!("{} <<{}", a.join(" "), body.unwrap_or("")));
            Ok(String::new())
        };
        let read = |_: &[String]| -> Result<String, crate::composed::ReadError> { Ok("[{\"description\": \"new\"}]".into()) };
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(Some(held.clone())));
        let mut after = |_: &[String], _: &str| {
            acknowledge(g.forced.take().expect("the guard admitted a forced edit"), &read, &write, "", "");
            String::new()
        };
        let mut body = || Ok::<String, String>("new".into());
        let ans = run(&v(&["update", "sp-a", "--body-file", "-", "--force-claimed", "why"]), &mut body, &mut gd, &mut after, &read, &write);
        assert_eq!(ans.code, 0, "{ans:?}");
        let wrote = wrote.into_inner();
        assert_eq!(wrote[0], "update sp-a --body-file - <<new", "bd never sees spira's own flag");
        let hash = bead::claimdesc::desc_hash("[{\"description\": \"new\"}]").unwrap();
        assert_eq!(wrote[1], format!("update sp-a --set-metadata {}={hash} <<", bead::claimdesc::HASH_KEY));
        assert!(wrote[2].starts_with("note sp-a ") && wrote[2].contains("why"), "{}", wrote[2]);
        assert_eq!(wrote.len(), 3);
    }

    #[test_id(t2v185t)]
    #[test]
    fn a_force_flag_on_an_unheld_bead_never_reaches_bd() {
        let env = caller(&[]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let mut args = v(&["update", "sp-a", "--body-file", "-", "--force-claimed", "why"]);
        let mut body = Some("new".to_string());
        guard(&mut args, &mut body, &g, &mut Rows(None)).expect("an unheld bead admits the edit");
        assert_eq!(args, v(&["update", "sp-a", "--body-file", "-"]));
        assert!(g.forced.take().is_none());
    }

    #[test_id(t9naqf3)]
    #[test]
    fn every_variable_the_filing_contract_reads_reaches_the_service() {
        for var in bead::filing::VARS {
            assert!(spira_store::lc_call::GUARD_VARS.contains(var), "{var} is read by the contract but never sent");
        }
    }

    #[test_id(to43vbg)]
    #[test]
    fn an_overridden_state_write_is_admitted_and_logged() {
        let env = caller(&[("SPIRA_BDQ_STATE_WRITE", "landing")]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        assert_eq!(
            guard(&mut v(&["close", "sp-a"]), &mut None, &g, &mut Rows(None)).map_err(|a| a.stderr),
            Ok("spira-lc content: STATE WRITE OVERRIDE (close sp-a) — bd's status moved outside the lifecycle machine: landing\n".into())
        );
    }

    #[test_id(tz0xzkq)]
    #[test]
    fn a_guard_refusal_is_the_answer_and_bd_never_runs() {
        let never_w = |_: &[String], _: Option<&str>| -> Result<String, String> { panic!("bd must not run") };
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("bd must not run") };
        let env = caller(&[("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "deadlock")]);
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &env, ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let mut machine = Rows(None);
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut machine);
        let mut no_after = |_: &[String], _: &str| -> String { panic!("nothing was written") };
        let mut body = || Ok::<String, String>("never read".into());
        let ans = run(&v(&["update", "sp-a", "--add-label", "x"]), &mut body, &mut gd, &mut no_after, &never, &never_w);
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "czar-fence: deadlock is shadow — mutation refused (SPIRA_CZAR_STAGE_DEADLOCK=act to enable)\n"));
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(None));
        let mut ask_body = || Ok::<String, String>("no shape at all".into());
        let ans = run(&v(&["create", "Pick", "--labels", ASK, "--body-file", "-"]), &mut ask_body, &mut gd, &mut no_after, &never, &never_w);
        assert_eq!(ans.code, 1, "the request body is judged as the ask's description: {ans:?}");
        let mut refuse_all = |_: &mut Vec<String>, _: &mut Option<String>| Err(Answer { code: 1, stderr: "no\n".into(), ..Default::default() });
        let ans = run(&v(&["show", "sp-a"]), &mut body, &mut refuse_all, &mut no_after, &|_| Ok("read".into()), &never_w);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "read"), "a read is never guarded");
    }

    #[test_id(t4j1yzx)]
    #[test]
    fn a_claimed_beads_assignee_clear_is_judged_as_bdq_judges_it() {
        let held = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holder": "dwarf-sp-a", "lease_until": NOW + 600});
        for env in [caller(&[]), czar(None), czar(Some("act")), caller(&[("SPIRA_RUNE", "builder")])] {
            for args in [&["update", "sp-a", "--assignee", ""][..], &["update", "sp-a", "--assignee", "", "--force"]] {
                for row in [None, Some(&held)] {
                    assert_eq!(through_content(&v(args), None, &env, row), through_bdq(&v(args), None, &env, row), "{args:?} {env:?}");
                }
            }
        }
    }

    #[test_id(tspf3vb)]
    #[test]
    fn a_gate_discover_runs_bd_in_the_registered_repo_root() {
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &caller(&[]), ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let ran: RefCell<Vec<(Vec<String>, Option<std::path::PathBuf>)>> = RefCell::new(Vec::new());
        let write = |a: &[String], _: Option<&str>| -> Result<String, String> {
            ran.borrow_mut().push((a.to_vec(), g.cwd.take()));
            Ok(String::new())
        };
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("not a read") };
        let mut no_after = |_: &[String], _: &str| String::new();
        let mut body = || Ok::<String, String>(String::new());
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(None));
        let ans = run(&v(&["gate", "discover", "--branch", "b", "--repo", "spira"]), &mut body, &mut gd, &mut no_after, &never, &write);
        assert_eq!(ans.code, 0, "{ans:?}");
        let root = registry().root("spira").map(std::path::PathBuf::from);
        assert_eq!(ran.borrow().as_slice(), &[(v(&["gate", "discover", "--branch", "b"]), root)]);
        let ans = run(&v(&["gate", "discover", "--branch", "b", "--repo", "nowhere"]), &mut body, &mut gd, &mut no_after, &never, &write);
        assert_eq!(ans.code, 1, "{ans:?}");
        assert_eq!(ran.borrow().len(), 1, "an unregistered name writes nothing");
    }

    #[test_id(tt1dz7p)]
    #[test]
    fn a_remembered_text_is_bds_last_argument_and_the_door_refuses_to_write_the_watermark() {
        let mut args = v(&["remember", "--key", "k"]);
        let mut body = Some("the text".to_string());
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &caller(&[]), ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        guard(&mut args, &mut body, &g, &mut Rows(None)).unwrap();
        assert_eq!((args, body), (v(&["remember", "--key", "k", "the text"]), None));
        assert!(check(&v(&["kv", "set", crate::plan::KV_KEY, "1"])).unwrap_err().contains("planning watermark"));
    }

    #[test_id(tot8gfj)]
    #[test]
    fn the_new_writes_reach_bd_with_bdqs_argv_and_a_remembered_body_last() {
        let ask = || Ok::<_, String>(ASK.to_string());
        let g = Guards { caller: &caller(&[]), ask_label: &ask, registry: &registry, policy: &no_policy, repo_of: &|_| None, target_of: &|_| None, incident_label: &incident, now: NOW, forced: Default::default(), cwd: Default::default() };
        let ran: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let write = |a: &[String], _: Option<&str>| -> Result<String, String> {
            ran.borrow_mut().push(a.to_vec());
            Ok(String::new())
        };
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("not a read") };
        let mut no_after = |_: &[String], _: &str| String::new();
        let mut body = || Ok::<String, String>("the text".into());
        let mut gd = |a: &mut Vec<String>, b: &mut Option<String>| guard(a, b, &g, &mut Rows(None));
        for args in [&["remember", "--key", "k"][..], &["forget", "k"], &["kv", "set", "k", "1"], &["recompute-blocked"], &["duplicate", "sp-a", "--of", "sp-b"], &["update", "sp-a", "--assignee", ""]] {
            assert_eq!(run(&v(args), &mut body, &mut gd, &mut no_after, &never, &write).code, 0, "{args:?}");
        }
        assert_eq!(ran.borrow()[0], v(&["remember", "--key", "k", "the text"]));
        assert_eq!(ran.borrow()[1..].to_vec(), [v(&["forget", "k"]), v(&["kv", "set", "k", "1"]), v(&["recompute-blocked"]), v(&["duplicate", "sp-a", "--of", "sp-b"]), v(&["update", "sp-a", "--assignee", ""])]);
    }

    struct Recorder(Vec<Vec<String>>);

    impl crate::callers::Machine for Recorder {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            self.0.push(args.to_vec());
            (0, String::new())
        }
    }

    struct NoBd;

    impl crate::callers::Bd for NoBd {
        fn issue_type(&mut self, _: &str) -> Result<String, String> {
            unreachable!("a create owes no bd read")
        }
        fn close(&mut self, _: &str, _: &str) -> Result<(), String> {
            unreachable!("a create owes no bd read")
        }
        fn closed(&mut self, _: &[String]) -> Result<Vec<(String, String)>, String> {
            unreachable!("a create owes no bd read")
        }
        fn known(&mut self, _: &[String]) -> Result<Vec<String>, String> {
            unreachable!("a create owes no bd read")
        }
        fn open_among(&mut self, _: &[String]) -> Result<Vec<String>, String> {
            unreachable!("a create owes no bd read")
        }
        fn reopen(&mut self, _: &str) -> Result<(), String> {
            unreachable!("a create owes no bd read")
        }
        fn work_beads(&mut self, _: &str) -> Result<Vec<String>, String> {
            unreachable!("a create owes no bd read")
        }
        fn external(&mut self, _: &str) -> Result<bool, String> {
            unreachable!("a create owes no bd read")
        }
        fn blocking_dependents(&mut self, _: &str) -> Result<Vec<String>, String> {
            unreachable!("a create owes no bd read")
        }
        fn planning_watermark(&mut self) -> Result<Option<u64>, String> {
            unreachable!("a create owes no bd read")
        }
        fn claim_read(&mut self, _: &str) -> Result<Option<crate::claim_sync::ClaimRead>, String> {
            Ok(Some(crate::claim_sync::ClaimRead { parent: None, labels: vec![], edges: vec![] }))
        }
    }

    fn rows_written_after(args: &[&str]) -> Vec<Vec<String>> {
        let mut m = Recorder(vec![]);
        assert_eq!(after(&v(args), "sp-new\n", &mut m, &mut NoBd, 0), "");
        m.0.into_iter().filter(|a| a[0] == "create-bead").collect()
    }

    #[test_id(tlojpvj)]
    #[test]
    fn a_created_beads_row_carries_the_repo_its_labels_name() {
        let named = rows_written_after(&["create", "T", "--labels", "plan,repo:deck,x"]);
        assert_eq!(named, [v(&["create-bead", "sp-new", "--ready", "--repo", "deck"])]);
        let bare = rows_written_after(&["create", "T", "--labels", "plan"]);
        assert_eq!(bare, [v(&["create-bead", "sp-new", "--ready"])], "no repo label, no repo");
    }

    #[test_id(t1jb2wl)]
    #[test]
    fn a_lifecycle_write_that_fails_after_bd_applied_is_reported_with_bds_answer() {
        let w = |_: &[String], _: Option<&str>| Ok::<_, String>("sp-new\n".to_string());
        let mut admit = |_: &mut Vec<String>, _: &mut Option<String>| Ok(String::new());
        let mut failed = |_: &[String], out: &str| format!("LIFECYCLE: {} has no row\n", out.trim());
        let mut body = || Err::<String, String>("none".into());
        let ans = run(&v(&["create", "T", "--silent"]), &mut body, &mut admit, &mut failed, &|_| panic!(), &w);
        assert_eq!((ans.code, ans.stdout.as_str(), ans.stderr.as_str()), (0, "sp-new\n", "LIFECYCLE: sp-new has no row\n"), "bd's write stands; the failure is on stderr");
        let bad = |_: &[String], _: Option<&str>| Err::<String, String>("bd: down".into());
        let mut never_after = |_: &[String], _: &str| -> String { panic!("bd did not apply it") };
        let ans = run(&v(&["create", "T", "--silent"]), &mut body, &mut admit, &mut never_after, &|_| panic!(), &bad);
        assert_eq!((ans.code, ans.stderr.as_str()), (1, "bd: down\n"));
    }

    #[test_id(t6zwirf)]
    #[test]
    fn run_refuses_before_calling_bd() {
        let never = |_: &[String]| -> Result<String, crate::composed::ReadError> { panic!("bd must not run") };
        let never_w = |_: &[String], _: Option<&str>| -> Result<String, String> { panic!("bd must not run") };
        let mut no_stdin = || Err::<String, String>("nothing was sent".into());
        let mut admit = |_: &mut Vec<String>, _: &mut Option<String>| Ok(String::new());
        let mut no_after = |_: &[String], _: &str| String::new();
        let ans = run(&v(&["delete", "sp-a"]), &mut no_stdin, &mut admit, &mut no_after, &never, &never_w);
        assert_eq!(ans.code, REFUSED);
        let ans = run(&v(&["note", "sp-a", "--stdin"]), &mut no_stdin, &mut admit, &mut no_after, &never, &never_w);
        assert_eq!((ans.code, ans.stderr.as_str()), (CANNOT_TELL, "spira-lc content: stdin: nothing was sent\n"));
        let ans = run(&v(&["show", "sp-a"]), &mut no_stdin, &mut admit, &mut no_after, &|a| Ok(format!("ran {}", a.join(" "))), &never_w);
        assert_eq!((ans.code, ans.stdout.as_str()), (0, "ran show sp-a"));
    }
}
