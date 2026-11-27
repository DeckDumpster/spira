//! `bead` — see DESIGN.md. `spira/bead.sh` is the shim every caller names; it `exec`s into
//! this binary by bare name on the release PATH, passing its own directory as `--home`
//! (the same convention `gate.sh` established).

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use spira_config::amendment;
use bead::claimdesc::notify_live_dwarf;
use bead::{
    runes_partitions, epic_blocks_refusal, incident_blocks_refusal, is_blocks_type,
    stack_after_refusal, lint_judge, parse_created_id, non_work_labels, parse_blocks_targets, parse_list_ids, parse_show_row,
    persona_line, repos_by_name, repos_section, work_labels,
};

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut home = ".".to_string();
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home = it.next().unwrap_or_else(|| ".".to_string());
        } else {
            rest.push(a);
        }
    }

    let mut args = rest.into_iter();
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-bead", &cmd);
    let rc = match cmd.as_str() {
        "file" => cmd_file(&home, &rest),
        "pilgrimage" => cmd_pilgrimage(&home, &rest),
        "amend" => cmd_amend(&home, &rest),
        "label" => cmd_label(&home, &rest),
        "drop" => cmd_drop(&rest),
        "contract" => cmd_contract(&home),
        "ideas" => cmd_ideas(&rest),
        "lint" => cmd_lint(&home, &rest),
        "judge-create" => cmd_judge_create(&home, &rest),
        "event" => cmd_event(&rest),
        // `czar-fence.sh <class>`'s answer: 0 act, 1 shadow (the default), 2 otherwise, read
        // from this process's environment as the script read it (bead::guard::czar_stage).
        "czar-fence" => match bead::guard::czar_stage(rest.first().map(String::as_str).unwrap_or(""), &|k| env::var(k).ok()) {
            Ok(()) => 0,
            Err(r) => {
                eprint!("{}", r.text);
                r.code
            }
        },
        "dep" => match rest.first().map(String::as_str) {
            Some("add") => cmd_dep_add(&rest[1..]),
            Some("remove") => cmd_dep_remove(&rest[1..]),
            Some("epic-edges") => cmd_dep_epic_edges(),
            Some("convert") => cmd_dep_convert(&rest[1..]),
            _ => {
                eprintln!("usage: bead.sh dep add <id> <depends-on-id> [--type <type>]\n       bead.sh dep remove <id> <depends-on-id>");
                2
            }
        },
        _ => {
            eprintln!(
                "usage: bead.sh file \"<title>\" --for <persona> --repo <name> [--priority N] [--body-file F] [--express] [--submitted --tip <sha>] [--contract-fix] [--depends-on <id>]... [--json]\n       bead.sh file \"<title>\" --kind <kind> [--repo <name>] [--part N (proposal)] [--priority N] [--body-file F] [--express] [--submitted --tip <sha>] [--json]\n       --submitted --tip <sha> files the item SUBMITTED at <sha> and creates refs/heads/spira/<id> there in the repo checkout\n       bead.sh pilgrimage <design-file> --repo <name> --after <build-id>... [--priority N]\n       bead.sh amend <id> [--note \"<text>\"] [--priority N] [--body-file F --replace | --append-body F] [--express]\n       bead.sh amend <id> --live --note \"<text>\" | --append-body F [--ack-within <dur>]\n       bead.sh amend <id> --ack <amend-id> --accept | --decline \"<why>\"\n       bead.sh amend --enforce <amend-id>\n       bead.sh label add|remove <id> <label>\n       bead.sh drop <id> --reason <text>\n       bead.sh dep add <id> <depends-on-id> [--type <type>]\n       bead.sh dep remove <id> <depends-on-id>\n       bead.sh lint [--all|<id>...]\n       bead.sh ideas [--json]\n       bead.sh contract\n       bead event <kind> <target|-> <title> [detail]"
            );
            2
        }
    };
    spira_otel::oneshot::exit(rc);
}

// =========================================================================================
// The content door — every read and write of a bead goes to `spira-lc content`.
// =========================================================================================

/// The bead type `--kind` names when it is not given: a work bead.
const DEFAULT_KIND: &str = "work";

/// What a call exits when the door's client cannot be built.
const DOOR_UNREACHABLE: i32 = 90;

#[cfg(test)]
thread_local! {
    static TEST_DOOR: std::cell::RefCell<Option<spira_store::lc_call::LcClient>> = const { std::cell::RefCell::new(None) };
}

/// The store's client: its socket in production, the declared stand-in in a fixture.
fn lc() -> Result<spira_store::lc_call::LcClient, String> {
    #[cfg(test)]
    if let Some(c) = TEST_DOOR.with(|d| d.borrow().clone()) {
        return Ok(c);
    }
    spira_store::lc_call::LcClient::configured()
}

#[cfg(test)]
thread_local! {
    /// Every git call a test's filing made, and the (subcommand, exit, output) it is answered with.
    static TEST_GIT: std::cell::RefCell<(Vec<Vec<String>>, Vec<(&'static str, i32, &'static str)>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    static TEST_SAID: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The git a submitted filing runs: exit, with stdout on success and stderr otherwise.
fn git(args: &[&str]) -> (i32, String) {
    #[cfg(test)]
    {
        TEST_GIT.with(|g| {
            let mut g = g.borrow_mut();
            g.0.push(args.iter().map(|a| a.to_string()).collect());
            g.1.iter().find(|(verb, _, _)| args.get(2) == Some(verb)).map_or((0, String::new()), |(_, code, out)| (*code, out.to_string()))
        })
    }
    #[cfg(not(test))]
    match spira_config::bounded::bounded("git").args(args).output() {
        Ok(o) => {
            let said = if o.status.success() { &o.stdout } else { &o.stderr };
            (o.status.code().unwrap_or(-1), String::from_utf8_lossy(said).into_owned())
        }
        Err(e) => (-1, format!("cannot run git: {e}")),
    }
}

fn complain(line: &str) {
    #[cfg(test)]
    TEST_SAID.with(|s| s.borrow_mut().push(line.to_string()));
    eprintln!("{line}");
}

/// A read through the content door.
fn door(args: &[String]) -> (i32, String) {
    match lc() {
        Ok(c) => bead::door::read(&c, args),
        Err(e) => {
            eprintln!("bead: {e}");
            (DOOR_UNREACHABLE, String::new())
        }
    }
}

/// The row `id` shows through the door, or `None` when there is no such bead.
fn shown_row(id: &str) -> Option<bead::ShowRow> {
    let (_, out) = door(&s(&["show", id, "--json"]));
    parse_show_row(&out)
}

/// A write through the content door, its refusal text on stderr as the caller saw it from bd:
/// the exit code and the stdout, left for the caller to print. A door that cannot say whether
/// the write committed exits [`CANNOT_TELL`].
fn put_capture(args: &[&str], body: Option<&str>) -> (i32, String) {
    put_capture_under(args, body, None)
}

/// [`put_capture`] sent under `request` and sent again under it while the store cannot tell: the
/// store runs the write once and answers every send with that run's result.
fn put_capture_under(args: &[&str], body: Option<&str>, request: Option<&str>) -> (i32, String) {
    let client = match lc() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bead: {e}");
            return (DOOR_UNREACHABLE, String::new());
        }
    };
    let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
    let sent = match request {
        None => match body {
            Some(b) => client.call_stdin(&argv, b),
            None => client.call(&argv),
        }
        .map_err(|e| e.to_string()),
        Some(request) => resend_under(&client, &argv, body, request),
    };
    match sent {
        Ok(r) => {
            eprint!("{}", r.err);
            (r.code, r.out)
        }
        Err(e) => {
            eprintln!("bead: {e}");
            (CANNOT_TELL, String::new())
        }
    }
}

fn resend_under(client: &spira_store::lc_call::LcClient, argv: &[&str], body: Option<&str>, request: &str) -> Result<spira_store::lc_call::Reply, String> {
    let retry = spira_store::lifecycle_row::Retry::configured()?;
    let mut said = false;
    let mut note = || {
        if !std::mem::replace(&mut said, true) {
            eprintln!("bead: file: the store cut the create at its deadline; re-sent under request {request}");
        }
    };
    spira_store::lc_call::as_request(request, || spira_store::lifecycle_row::call_idempotent_noting(client, argv, body, &retry, &mut note))
}

/// [`put_capture`] with the door's stdout passed through.
fn put(args: &[&str], body: Option<&str>) -> i32 {
    let (code, out) = put_capture(args, body);
    print!("{out}");
    code
}

fn put_note(id: &str, text: &str) -> i32 {
    put(&["note", id, "--stdin"], Some(text))
}

/// The labels of `id`; empty when it cannot be read. A bd parent cannot be blocked on its own
/// child, so a remedy filed under an incident is wired as a plain blocks edge instead.
fn parent_labels(id: &str) -> Vec<String> {
    let (_, out) = door(&s(&["show", id, "--json"]));
    parse_show_row(&out).map(|r| r.labels).unwrap_or_default()
}

/// A create through the door: stdout is passed through unchanged, and when `incident` is
/// set that incident is wired to block on the new bead in the same step
/// (law-a-bug-with-a-fix-in-flight-depends-on-it). Failing to wire it fails the filing.
#[allow(clippy::too_many_arguments)]
fn door_create(out: &mut dyn std::io::Write, args: &[String], body: Option<&str>, incident: Option<(&str, &[String])>, blockers: &[String], title: Option<&str>, lookup_title: Option<&str>, express: bool, submitted: Option<(&str, &str)>, repo: Option<&str>, drafted_by: Option<&str>) -> i32 {
    if incident.is_none() && blockers.is_empty() && lookup_title.is_none() && !express {
        return put(&args.iter().map(String::as_str).collect::<Vec<_>>(), body);
    }
    let mut id_text = None;
    let rc = door_create_writes(&mut id_text, args, body, incident, blockers, title, lookup_title, express, submitted, repo, drafted_by);
    if let Some(text) = id_text {
        let _ = write!(out, "{text}");
        let _ = out.flush();
    }
    rc
}

/// Every write of the filing; the id text goes to `id_text` for the caller to print once they are all sent.
#[allow(clippy::too_many_arguments)]
fn door_create_writes(id_text: &mut Option<String>, args: &[String], body: Option<&str>, incident: Option<(&str, &[String])>, blockers: &[String], title: Option<&str>, lookup_title: Option<&str>, express: bool, submitted: Option<(&str, &str)>, repo: Option<&str>, drafted_by: Option<&str>) -> i32 {
    let submitted_tip = submitted.map(|(tip, _)| tip);
    let argv_owned = args.to_vec();
    let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
    let window = match spira_config::process::cfg_parse::<u64>(FILE_WINDOW_KEY) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("bead: file: {e}");
            return 1;
        }
    };
    let since = bead::bdq::utc_stamp_nanos(std::time::SystemTime::now() - std::time::Duration::from_secs(window));
    let created_at = std::cell::Cell::new(std::time::Instant::now());
    let request = bead::filing::create_request_id(&std::env::var("BEADS_ACTOR").unwrap_or_default(), &argv_owned, body);
    let mut create = || {
        let sent = put_capture_under(&argv, body, Some(&request));
        created_at.set(std::time::Instant::now());
        sent
    };
    let mut read = || {
        let (list_code, listing) = door(&s(&["list", "--limit", "0", "--json"]));
        bead::readback(list_code, &listing, lookup_title.unwrap_or_default(), &since[..19], drafted_by)
    };
    let mut wait = || {
        if created_at.get().elapsed() >= spira_store::lc_call::DEADLINE {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(500)); // waits-on: the store's listing; cap lc_call::DEADLINE
        true
    };
    let utc = || bead::bdq::utc_stamp_nanos(std::time::SystemTime::now())[..19].to_string() + "Z";
    let mut say = |t: &str| eprintln!("{t}");
    let (code, stdout) = bead::filing::create_or_find(lookup_title, &mut bead::filing::CreateDoor { create: &mut create, read: &mut read, wait: &mut wait, utc: &utc, say: &mut say, json: argv.contains(&"--json") });
    let by_hand = unrecovered(None, express, incident.map(|(i, _)| i));
    if code == CANNOT_TELL && parse_created_id(&stdout).is_none() && !by_hand.is_empty() {
        eprintln!("bead: file: bead-sweep cannot restore these if the bead turns up{by_hand}");
    }
    if submitted_tip.is_none() {
        *id_text = Some(stdout.clone());
    }
    if code != 0 {
        // bd may have written the content bead before it errored; without a row the loop cannot see it.
        if let (Some(id), Some(_)) = (parse_created_id(&stdout), title) {
            if submitted_tip.is_none() && create_row_retrying(&id, None, repo).is_ok() {
                eprintln!("bead: file: bd create exited {code} after creating {id}; its lifecycle row was created");
            } else {
                eprintln!("bead: file: bd create exited {code} after creating {id}; repair: spira-lc create-bead {id} --ready{}", unrecovered(Some(&id), express, incident.map(|(i, _)| i)));
            }
        }
        return code;
    }
    let Some(new_id) = parse_created_id(&stdout) else {
        if submitted_tip.is_some() {
            eprintln!("bead: file: created, but could not read its id to create its SUBMITTED lifecycle row; it is not claimable until one exists: spira-lc create-bead <id> --submitted-tip <sha>");
            return 1;
        }
        if let Some((incident, _)) = incident {
            eprintln!("bead: file: created a remedy for incident {incident} but could not read its id; add the edge: bd dep add {incident} <id>");
            return 1;
        }
        eprintln!("bead: file: created, but could not read its id to create its lifecycle row or mark it express; `spira-lc create-bead <id> --ready` creates the row, `bead.sh amend <id> --express` marks it");
        return 0;
    };
    let mut rc = 0;
    if title.is_some() {
        match submitted {
            Some((tip, root)) => {
                if let Err(e) = create_row_retrying(&new_id, Some(tip), repo) {
                    eprintln!("bead: file: {new_id} filed but its lifecycle row was not created SUBMITTED: {e}; no id is printed. Once the lifecycle store answers: spira-lc create-bead {new_id} --submitted-tip {tip}");
                    return 1;
                }
                if let Err(e) = bead::filing::SubmittedBranch::new(&mut git).create(root, &new_id, tip) {
                    complain(&format!("bead: file: {new_id} filed SUBMITTED at {tip} but refs/heads/spira/{new_id} was not created: {e}; no id is printed. Create it: git -C {root} update-ref refs/heads/spira/{new_id} {tip} \"\""));
                    return 1;
                }
                *id_text = Some(stdout.clone());
            }
            None => {
                if let Err(e) = create_row_retrying(&new_id, None, repo) {
                    eprintln!("bead: file: {new_id} filed but its lifecycle row was not created: {e}; the loop cannot see it and a retry would duplicate it. Repair: spira-lc create-bead {new_id} --ready");
                    return 1;
                }
            }
        }
    }
    if express {
        if let Err(e) = spira_store::lifecycle_row::set_express(&new_id, true) {
            eprintln!("bead: file: {new_id} filed but not marked express: {e}; mark it: bead.sh amend {new_id} --express");
            rc = 1;
        }
    }
    if let Some((incident, labels)) = incident {
        if let Some((from, to)) = bead::remedy_edge(labels, &cfg_label("SPIRA_INCIDENT_LABEL"), &new_id, incident) {
            let (r, _) = put_capture(&["dep", "add", &from, &to, "--type", "blocks"], None);
            if r != 0 {
                eprintln!("bead: file: {new_id} filed but the blocks edge {from} -> {to} failed; add it: bd dep add {from} {to}");
            }
            rc |= r;
        }
    }
    let promote_row = title.is_some() && submitted_tip.is_none();
    let mut promote = || {
        if let Err(e) = spira_store::lifecycle_row::promote(&new_id) {
            eprintln!("bead: file: {new_id} filed but its row was not promoted from OPEN: {e}; the OPEN sweep promotes it, or: spira-lc promote-filed {new_id}");
        }
    };
    rc |= bead::filing::edges_then_promote(
        &new_id,
        blockers,
        &mut |from, to| blocks_edge_present_ok(from, to),
        promote_row.then_some(&mut promote as &mut dyn FnMut()),
    );
    rc
}

use bead::filing::CANNOT_TELL;

/// An edge that rode the create is already in the content store, which is not a failure, but the
/// lifecycle side is mirrored here because the add that would have mirrored it was refused.
fn blocks_edge_present_ok(from: &str, to: &str) -> i32 {
    let (r, _) = put_capture(&["dep", "add", from, to, "--type", "blocks"], None);
    if r == 0 {
        return 0;
    }
    let (_, listed) = door(&s(&["dep", "list", from, "--type", "blocks", "--json"])); // BDQ_READ_CONSIDERED="through the content door, as every read here"
    if !bead::parse_blocks_targets(&listed).iter().any(|t| t == to) {
        return r;
    }
    match lc().map(|c| c.call(&bead::lc::dep_argv("dep-add", from, to, Some("blocks")))) {
        Ok(Ok(reply)) if reply.code == 0 => 0,
        _ => r,
    }
}

/// The registered window in which a bead of the same title is the one a retried filing wants.
const FILE_WINDOW_KEY: &str = "SPIRA_BEAD_FILE_RETRY_WINDOW_SECS";

/// The repairs of the parts bead-sweep cannot recover from a bead it finds: the express mark and the incident edge.
fn unrecovered(id: Option<&str>, express: bool, incident: Option<&str>) -> String {
    let id = id.unwrap_or("<id>");
    let mut parts = Vec::new();
    if express {
        parts.push(format!("mark it express: bead.sh amend {id} --express"));
    }
    if let Some(i) = incident {
        parts.push(format!("wire the incident edge: bd dep add {i} {id}"));
    }
    if parts.is_empty() { String::new() } else { format!("; {}", parts.join("; ")) }
}

/// Filing is two writes; the second is retried through a store restart, so it never strands the first.
fn create_row_retrying(id: &str, tip: Option<&str>, repo: Option<&str>) -> Result<(), String> {
    bead::lc::create_row_retrying(&lc()?, id, tip, repo, &spira_store::lifecycle_row::Retry::configured()?)
}

fn s(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

/// `RUNE_LABELS`, plainly evaluated — delegates to `spira_config::runes::rune_get`
/// (sp-xsnid retired this crate's OWN hand-rolled bash-bridge, which carried an
/// independently-drifted three-key label overlay — `SPIRA_CZAR_LABEL`/
/// `SPIRA_GROOMER_LABEL`/`SPIRA_MIMIR_LABEL` only, never `SPIRA_INCIDENT_LABEL`/
/// `SPIRA_SPIKE_LABEL`/`SPIRA_PLAN_LABEL`/`SPIRA_SCOPE_LABEL`/`SPIRA_ASK_LABEL`/
/// `SPIRA_CI_LABEL` — a SECOND hand-curated list with its own gaps, on top of
/// `spira-config`'s own five-key one). For REPORTING call sites only (`bead contract`,
/// `bead lint`): an empty result here just means the report shows an empty partition for
/// that persona, never a claim decision, so it never needs [`rune_predicate`]'s
/// fail-closed refusal. `home` is [`runes_home`]'s own `$SPIRA_HOME`-override
/// precedence, matching every other runes read in this file.
fn rune_label(home: &str, rune: &str) -> String {
    spira_config::runes::rune_get(Path::new(&runes_home(home)), rune, "RUNE_LABELS", "")
}

const REQUIRES_BUILD_SPEC_KEY: &str = "RUNE_REQUIRES_BUILD_SPEC";
const REQUIRES_SKULD_KEY: &str = "RUNE_REQUIRES_SKULD";

fn requires_skuld(home: &str, rune: &str) -> bool {
    spira_config::runes::rune_get(Path::new(&runes_home(home)), rune, REQUIRES_SKULD_KEY, "") == "1"
}

fn is_concierge() -> bool {
    env::var("SPIRA_CONCIERGE").as_deref() == Ok("1")
}

/// Refuses (exit 2) a Concierge body no Skuld request holds verbatim; `Ok` carries the request id.
fn skuld_gate(op: &str, title: &str, body: &str, drafted_by: Option<&str>) -> Result<String, i32> {
    let refuse = |why: String| {
        spira_otel::oneshot::event("spira_skuld_refusals", 1, &[("op", op)]);
        eprintln!("bead: refusing: {why}");
        Err(2)
    };
    let Some(id) = drafted_by else {
        return refuse(bead::SKULD_NEEDS_DRAFT.to_string());
    };
    let (_, shown) = door(&s(&["show", id, "--json"]));
    let Some(row) = parse_show_row(&shown) else {
        return refuse(format!("--drafted-by {id}: no such bead; get a draft or review: bead.sh file \"<title>\" --for skuld --repo <r> --body-file <intent or draft>"));
    };
    match bead::skuld_drafted(title, body, &cfg_label("SPIRA_SKULD_LABEL"), id, &row.labels, &row.notes) {
        Ok(()) => Ok(id.to_string()),
        Err(why) => refuse(why),
    }
}

fn requires_build_spec(home: &str, rune: &str) -> bool {
    spira_config::runes::rune_get(Path::new(&runes_home(home)), rune, REQUIRES_BUILD_SPEC_KEY, "") == "1"
}

/// `RUNE_LABELS`/`RUNE_EXCLUDE_LABELS`, fully resolved and FAIL-CLOSED — delegates to
/// `spira_config::runes::rune_predicate`, the one shared evaluator every
/// ready-counting/claiming/FILING caller now goes through (sp-xsnid). For the one call
/// site that decides what a bead gets FILED as (`bead file --for`): a predicate that
/// would otherwise widen to the whole ready queue must refuse the filing outright, never
/// hand back an empty label silently.
fn rune_predicate(home: &str, rune: &str) -> Result<spira_config::runes::Predicate, spira_config::runes::Refusal> {
    spira_config::runes::rune_predicate(Path::new(&runes_home(home)), rune)
}

/// `SPIRA_HOME`, preferring an explicit override from THIS process's own environment over
/// the `--home` argument (`dirname "$0"`, `bead.sh`'s own location). This is the one place
/// `runes_dir` ever differed from `home`: several suites (`test-bead-contract.sh`,
/// notably) pin a scratch runes by exporting `SPIRA_HOME` themselves, distinct from
/// wherever the real `bead.sh` lives on PATH — a fact that survives `exec` on its own
/// (an already-exported var keeps its export attribute across reassignment, and across
/// `exec`, with no help from `bead.sh`), so it needed no re-export even before wave 4.9.
/// `home_arg` is the correct answer only for the UNEXPORTED, no-override, production case
/// — exactly conf.sh's own `SPIRA_HOME="${SPIRA_HOME:-$_spira_conf_here}"` derived default,
/// which is this same `--home` value.
fn runes_home(home_arg: &str) -> String {
    env::var("SPIRA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| home_arg.to_string())
}

fn runes_dir(home: &str) -> String {
    spira_config::runes::runes_dir(Path::new(&runes_home(home))).to_string_lossy().into_owned()
}

/// `rune_names`: every `*.rune` basename under `<home>/runes`, sorted — bash's own
/// glob expansion sorts lexicographically, so this matches it with `Vec::sort`.
fn rune_names(home: &str) -> Vec<String> {
    let mut names = match std::fs::read_dir(runes_dir(home)) {
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

// FORK-EDGE: script schema.sh (291 lines of shell; over the in-crate port bound) — its three
// callers below (type-of, kinds, name) are this one edge.
fn schema_sh(home: &str) -> String {
    format!("{home}/schema.sh")
}

/// `schema.sh type-of <kind>` — `Ok(bd_type)` or `Err(())` (stderr discarded: `bead.sh`
/// prints its own "unknown kind" message, never schema.sh's).
fn schema_type_of(home: &str, kind: &str) -> Result<String, ()> {
    // schema.sh sources an installed release's conf.sh, which a unit test has none of.
    if cfg!(test) && kind == DEFAULT_KIND {
        return Ok("task".to_string());
    }
    let out = spira_config::bounded::bounded(schema_sh(home))
        .arg("type-of")
        .arg(kind)
        .output()
        .map_err(|_| ())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(())
    }
}

/// `schema.sh kinds` — passed straight through to stdout by `contract` (bash's own
/// behaviour: the call sits inline between two of `_bead_contract`'s own `printf`s).
fn schema_kinds_passthrough(home: &str) -> String {
    spira_config::bounded::bounded(schema_sh(home))
        .arg("kinds")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `schema.sh name <key>` — used for the two names (`scope`, `insight`) `bead.sh file`'s
/// non-work path reads from the live schema rather than an inline bash default, so this
/// bridges rather than duplicating `schema_name`'s own default table.
fn schema_name(home: &str, key: &str) -> String {
    spira_config::bounded::bounded(schema_sh(home))
        .arg("name")
        .arg(key)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// =========================================================================================
// Repository map / env helpers
// =========================================================================================

/// `SPIRA_REPO_MAP` through `cfg` (per Ryan 2026-10-05: one source of config) — the config
/// file `cfg` reads is the same whatever `--home` says; `--home` only ever selects
/// runes/registry PATHS (`runes_home`/`runes_dir`/`rune_names`), never which
/// config file is in force.
fn load_repos() -> std::collections::BTreeMap<String, spira_config::RepoSection> {
    let path = cfg_label("SPIRA_REPO_MAP");
    if path.is_empty() {
        return std::collections::BTreeMap::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => repos_by_name(&content),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

fn load_identity() -> std::collections::BTreeMap<String, Vec<String>> {
    let path = cfg_label("SPIRA_REPO_IDENTITY");
    if path.is_empty() {
        return Default::default();
    }
    std::fs::read_to_string(&path).map(|t| bead::identity::parse(&t)).unwrap_or_default()
}

/// `Err` is the exit code of a refusal; `Ok(Some(line))` is the line to record on the bead
/// when the named override admitted text that carried another repo's terms.
fn identity_guard(repo: &str, text: &str) -> Result<Option<String>, i32> {
    bead::filing::identity_guard(&load_identity(), repo, text, &|k| env::var(k).ok()).map_err(refused)
}

fn refused(r: bead::guard::Refusal) -> i32 {
    eprint!("{}", r.text);
    r.code
}

/// The statute check on a decision's own text. `Ok(Some(line))` is the line to record on the
/// bead; an unreadable or malformed registry refuses, and only an empty key is inert.
fn statute_guard(text: &str) -> Result<Option<String>, i32> {
    let statutes = bead::filing::Statutes::at(&cfg_label("SPIRA_STATUTE_GUARDS"));
    let note = bead::filing::statute_guard(&statutes, text, &|k| env::var(k).ok()).map_err(refused)?;
    eprint!("{}", note.shown);
    Ok(note.line)
}

/// One source of config (per Ryan 2026-10-05): a registered key's value, through
/// `spira_config::process::cfg`, resolved from `$SPIRA_TOML`. Several of these keys treat
/// a resolved empty string as a legitimate, documented value (SPIRA_SCOPE_LABEL's "no
/// scope exclusion", in particular) — that passes straight through unchanged. Only an
/// actual resolution failure refuses, naming the key; it is never silently substituted.
fn cfg_label(key: &str) -> String {
    match spira_config::process::cfg(key) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bead: {e}");
            spira_otel::oneshot::exit(1);
        }
    }
}

// =========================================================================================
// file
// =========================================================================================

/// A body file carrying the `Target:` line ahead of the caller's body, so the target is part of the bead.
fn with_target_line(target: &str, body_file: Option<&str>) -> Result<String, String> {
    let body = match body_file {
        Some(f) => std::fs::read_to_string(f).map_err(|e| format!("reading {f}: {e}"))?,
        None => String::new(),
    };
    let path = env::temp_dir().join(format!("bead-target-{}.md", std::process::id()));
    std::fs::write(&path, format!("{}{target}\n\n{body}", spira_config::target::TARGET_PREFIX)).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

fn cmd_file(home: &str, args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("bead: title required");
        return 2;
    }
    let title = args[0].clone();

    let mut for_rune: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut body_file: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut express = false;
    let mut submitted = false;
    let mut contract_fix = false;
    let mut tip: Option<String> = None;
    let mut json = false;
    let mut parent: Option<String> = None;
    let mut target: Option<String> = None;
    let mut stack_after: Option<String> = None;
    let mut part: Option<String> = None;
    let mut drafted_by: Option<String> = None;
    let mut depends_on: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drafted-by" => {
                i += 1;
                drafted_by = args.get(i).cloned();
            }
            "--target" => {
                i += 1;
                target = args.get(i).cloned();
            }
            "--part" => {
                i += 1;
                part = args.get(i).cloned();
            }
            "--stack-after" => {
                i += 1;
                stack_after = args.get(i).cloned();
            }
            "--depends-on" => {
                i += 1;
                match args.get(i) {
                    Some(id) => depends_on.push(id.clone()),
                    None => {
                        eprintln!("bead: --depends-on needs a bead id");
                        return 2;
                    }
                }
            }
            "--for" => {
                i += 1;
                for_rune = args.get(i).cloned();
            }
            "--repo" => {
                i += 1;
                repo = args.get(i).cloned();
            }
            "--priority" | "-p" => {
                i += 1;
                priority = args.get(i).cloned();
            }
            "--body-file" => {
                i += 1;
                body_file = args.get(i).cloned();
            }
            "--kind" => {
                i += 1;
                kind = args.get(i).cloned();
            }
            "--parent" => {
                i += 1;
                parent = args.get(i).cloned();
            }
            "--express" => express = true,
            "--submitted" => submitted = true,
            "--contract-fix" => contract_fix = true,
            "--tip" => {
                i += 1;
                tip = args.get(i).cloned();
            }
            "--json" => json = true,
            "--harness-title" => {}
            other => {
                eprintln!("bead: unknown option: {other}");
                return 2;
            }
        }
        i += 1;
    }

    if body_file.as_deref() == Some("-") {
        let mut text = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
            eprintln!("bead: reading the body from stdin: {e}");
            return 2;
        }
        let path = env::temp_dir().join(format!("bead-stdin-{}.md", std::process::id()));
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("bead: writing {}: {e}", path.display());
            return 2;
        }
        body_file = Some(path.to_string_lossy().into_owned());
    }

    let parent_cmdline = std::fs::read(format!("{}/{}/cmdline", system_paths::PROC, std::os::unix::process::parent_id())).unwrap_or_default();
    let mut request_id: Option<String> = None;
    if bead::skuld_fence_binds(is_concierge(), for_rune.as_deref().is_some_and(|r| requires_skuld(home, r)), &parent_cmdline) {
        let body = body_file.as_deref().map(|f| std::fs::read_to_string(f).unwrap_or_default()).unwrap_or_default();
        let id = match skuld_gate("file", &title, &body, drafted_by.as_deref()) {
            Ok(id) => id,
            Err(rc) => return rc,
        };
        request_id = Some(id.clone());
        if let Some(why) = bead::depends_refusal(&body, &depends_on) {
            eprintln!("bead: refusing: {why}");
            return 2;
        }
        let path = env::temp_dir().join(format!("bead-drafted-{}.md", std::process::id()));
        if let Err(e) = std::fs::write(&path, format!("Drafted by Skuld: {id}\n\n{body}")) {
            eprintln!("bead: writing {}: {e}", path.display());
            return 1;
        }
        body_file = Some(path.to_string_lossy().into_owned());
    } else if drafted_by.is_some() {
        eprintln!("bead: --drafted-by applies only to the Concierge's builder filings");
        return 2;
    }

    if let Some(t) = &target {
        if let Err(e) = spira_config::target::valid_target(t) {
            eprintln!("bead: {e}");
            return 2;
        }
        if kind.as_deref().is_some_and(|k| k != "work") {
            eprintln!("bead: --target applies only to work beads");
            return 2;
        }
        match with_target_line(t, body_file.as_deref()) {
            Ok(p) => body_file = Some(p),
            Err(e) => {
                eprintln!("bead: {e}");
                return 2;
            }
        }
    }

    let incident_label = cfg_label("SPIRA_INCIDENT_LABEL");
    let parent_labels = parent.as_deref().map(parent_labels).unwrap_or_default();
    let incident_parent: Option<(String, Vec<String>)> = match &parent {
        Some(_) if bead::is_incident(&parent_labels, &incident_label) => parent.take().map(|p| (p, parent_labels)),
        _ => None,
    };
    let incident_parent = incident_parent.as_ref().map(|(p, l)| (p.as_str(), l.as_slice()));

    let repos = load_repos();
    if let Some(r) = &repo {
        if !repos.contains_key(r) {
            let valid = repos.keys().cloned().collect::<Vec<_>>().join(" ");
            let valid = if valid.is_empty() {
                "<map not found>".to_string()
            } else {
                valid
            };
            eprintln!("bead: repo:{r} is not in the repo map; valid keys: {valid}");
            return 2;
        }
    }

    if let Some(r) = &repo {
        let body = body_file.as_deref().map(|f| std::fs::read_to_string(f).unwrap_or_default()).unwrap_or_default();
        match identity_guard(r, &format!("{title}\n{body}")) {
            Err(rc) => return rc,
            Ok(Some(why)) => {
                let path = env::temp_dir().join(format!("bead-crossrepo-{}.md", std::process::id()));
                if let Err(e) = std::fs::write(&path, format!("{why}\n\n{body}")) {
                    eprintln!("bead: writing {}: {e}", path.display());
                    return 1;
                }
                body_file = Some(path.to_string_lossy().into_owned());
            }
            Ok(None) => {}
        }
    }

    {
        let body = body_file.as_deref().map(|f| std::fs::read_to_string(f).unwrap_or_default()).unwrap_or_default();
        match statute_guard(&format!("{title}\n{body}")) {
            Err(rc) => return rc,
            Ok(Some(line)) => {
                let path = env::temp_dir().join(format!("bead-statute-{}.md", std::process::id()));
                if let Err(e) = std::fs::write(&path, format!("{line}\n\n{body}")) {
                    eprintln!("bead: writing {}: {e}", path.display());
                    return 1;
                }
                body_file = Some(path.to_string_lossy().into_owned());
            }
            Ok(None) => {}
        }
    }

    if kind.is_some() && for_rune.is_some() {
        eprintln!("bead: --kind and --for are mutually exclusive");
        return 2;
    }

    let kind = kind.unwrap_or_else(|| DEFAULT_KIND.to_string());
    if kind == "gate" {
        eprintln!("bead: kind=gate is a bd-internal type; use bd gate directly");
        return 2;
    }

    let mut proposal_part: Option<u32> = None;
    if kind == "proposal" {
        match part.as_deref().map(str::parse::<u32>) {
            Some(Ok(n)) if n > 0 => proposal_part = Some(n),
            _ => {
                eprintln!("bead: refusing: a law proposal is filed into a Part of the code. Pass --part <n>; the Parts are:");
                eprint!("{}", law_contents());
                return 2;
            }
        }
    } else if part.is_some() {
        eprintln!("bead: --part applies only to --kind proposal");
        return 2;
    }

    let bd_type = match schema_type_of(home, &kind) {
        Ok(t) => t,
        Err(()) => {
            eprintln!("bead: unknown kind: {kind}");
            return 2;
        }
    };

    if submitted && kind != "work" {
        eprintln!("bead: --submitted applies only to work beads");
        return 2;
    }

    if contract_fix && (kind != "work" || for_rune.is_none()) {
        eprintln!("bead: --contract-fix applies only with --for <persona>");
        return 2;
    }

    if submitted && tip.as_deref().is_none_or(str::is_empty) {
        eprintln!("bead: --submitted requires --tip <sha>: the lifecycle row is created SUBMITTED at that tip, so no claimable READY window exists");
        return 2;
    }
    if tip.is_some() && !submitted {
        eprintln!("bead: --tip applies only with --submitted");
        return 2;
    }

    if kind == "work" {
        let for_rune = match for_rune {
            Some(f) => f,
            None => {
                eprintln!("bead: --for <persona> required");
                return 2;
            }
        };
        let repo = match repo {
            Some(r) => r,
            None => {
                eprintln!("bead: --repo <name> required");
                return 2;
            }
        };
        let mut checkout = None;
        if submitted {
            let Some(raw) = tip.take() else { return 2 };
            let registry = spira_config::repos::Registry::from_env_checkout(env::vars().collect(), Path::new(home));
            let Some(root) = registry.root(&repo) else {
                complain(&format!("bead: --submitted --tip {raw}: repo {repo} maps to no checkout; nothing filed"));
                return 2;
            };
            let mut run = git;
            let mut branch = bead::filing::SubmittedBranch::new(&mut run);
            if let Err(e) = branch.check(&root, &raw) {
                complain(&format!("bead: --submitted --tip {raw}: {e} (repo {repo}); nothing filed"));
                return 2;
            }
            match branch.resolve(&root, &raw) {
                Ok(full) => tip = Some(full),
                Err(e) => {
                    complain(&format!("bead: --tip {raw}: {e}"));
                    return 2;
                }
            }
            checkout = Some(root);
        }
        let fpath = format!("{}/{}.rune", runes_dir(home), for_rune);
        if !Path::new(&fpath).is_file() {
            eprintln!("bead: no such persona: {for_rune}");
            return 2;
        }
        let rune_labels = match rune_predicate(home, &for_rune) {
            Ok(p) => p.labels,
            Err(e) => {
                eprintln!("bead: {e}");
                return 2;
            }
        };
        if rune_labels.is_empty() {
            eprintln!("bead: persona {for_rune} has no partition labels");
            return 2;
        }

        let lane_override = env::var("SPIRA_BEAD_LANE_OVERRIDE").map(|v| !v.is_empty()).unwrap_or(false);
        let persona = bead::filing::Persona { name: for_rune.clone(), labels: rune_labels.split(',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(), requires_build_spec: requires_build_spec(home, &for_rune), unresolved: None };
        let body = body_file.as_deref().map(|f| std::fs::read_to_string(f).unwrap_or_default()).unwrap_or_default();
        if let Err(r) = bead::filing::persona_guards(&persona, &repo, &body, &repos, lane_override) {
            return refused(r);
        }

        let mut labels = bead::with_contract_fix(work_labels(&rune_labels, &repo), contract_fix);
        let contract = bead::filing::Contract::real(cfg_label("SPIRA_HOME_REPO"));
        let filed_labels: Vec<String> = labels.split(',').map(str::to_string).collect();
        match bead::filing::contract_guard(&repo, &filed_labels, &contract, &|k| env::var(k).ok()) {
            Ok(Some(line)) => {
                let path = env::temp_dir().join(format!("bead-contract-{}-{}.md", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
                if let Err(e) = std::fs::write(&path, format!("{line}\n\n{body}")) {
                    eprintln!("bead: writing {}: {e}", path.display());
                    return 1;
                }
                body_file = Some(path.to_string_lossy().into_owned());
            }
            Ok(None) => {}
            Err(r) => return refused(r),
        }

        for dep in &depends_on {
            if shown_row(dep).is_none() {
                eprintln!("bead: --depends-on {dep}: no such bead");
                return 2;
            }
        }

        if let Some(base) = &stack_after {
            let shown = shown_row(base);
            let terminal = bead::lc_done(&bead::door::DoorSource::lifecycle_only(vec![base.clone()], lifecycle_rows), &mut None, base);
            if let Some(why) = stack_after_refusal(base, shown.as_ref(), terminal, &repo) {
                eprintln!("bead: {why}");
                return 2;
            }
        }

        if submitted {
            labels.push(',');
            labels.push_str(&cfg_label("SPIRA_SUBMITTED_LABEL"));
        }
        let title_for_row = title.clone();
        let mut bd_args = s(&["create"]);
        bd_args.push(title);
        bd_args.push("-l".into());
        bd_args.push(labels);
        if let Some(p) = &priority {
            bd_args.push("-p".into());
            bd_args.push(p.clone());
        }
        let create_body = match body_file.as_deref().map(std::fs::read_to_string).transpose() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("bead: file: cannot read the body file: {e}");
                return 1;
            }
        };
        if create_body.is_some() {
            bd_args.push("--body-file".into());
            bd_args.push("-".into());
        }
        if json {
            bd_args.push("--json".into());
        } else {
            bd_args.push("--silent".into());
        }
        if let Some(p) = &parent {
            bd_args.push("--parent".into());
            bd_args.push(p.clone());
            bd_args.push("--no-inherit-labels".into());
        }
        let rc = door_create(&mut std::io::stdout(), &bd_args, create_body.as_deref(), incident_parent, &bead::filing::blockers(stack_after.as_deref(), &depends_on), Some(&title_for_row), Some(&title_for_row), express, tip.as_deref().zip(checkout.as_deref()), Some(repo.as_str()), request_id.as_deref());
        if rc == 0 {
            spira_otel::oneshot::event("spira_beads_filed", 1, &[("persona", &for_rune), ("repo", &repo), ("kind", "work")]);
        }
        rc
    } else {
        if stack_after.is_some() || !depends_on.is_empty() {
            eprintln!("bead: --stack-after and --depends-on apply only to work beads");
            return 2;
        }
        let scope_label = schema_name(home, "scope");
        let insight_label = if kind == "insight" || kind == "idea" {
            Some(schema_name(home, &kind))
        } else {
            None
        };
        let mut labels = non_work_labels(&scope_label, insight_label.as_deref(), repo.as_deref());
        if let Some(n) = proposal_part {
            labels.push_str(&format!(",part:{n}"));
        }

        let title_for_lookup = title.clone();
        let mut bd_args = s(&["create"]);
        bd_args.push(title);
        bd_args.push("-l".into());
        bd_args.push(labels);
        bd_args.push("--type".into());
        bd_args.push(bd_type);
        let mut priority = priority;
        if kind == "insight" {
            bd_args.push("--status".into());
            bd_args.push("closed".into());
            if priority.is_none() {
                priority = Some("4".to_string());
            }
        }
        if let Some(p) = &priority {
            bd_args.push("-p".into());
            bd_args.push(p.clone());
        }
        let create_body = match body_file.as_deref().map(std::fs::read_to_string).transpose() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("bead: file: cannot read the body file: {e}");
                return 1;
            }
        };
        if create_body.is_some() {
            bd_args.push("--body-file".into());
            bd_args.push("-".into());
        }
        if json {
            bd_args.push("--json".into());
        } else {
            bd_args.push("--silent".into());
        }
        if let Some(p) = &parent {
            bd_args.push("--parent".into());
            bd_args.push(p.clone());
            bd_args.push("--no-inherit-labels".into());
        }
        let rc = door_create(&mut std::io::stdout(), &bd_args, create_body.as_deref(), incident_parent, &[], None, Some(&title_for_lookup), express, None, None, None);
        if rc == 0 {
            spira_otel::oneshot::event("spira_beads_filed", 1, &[("persona", ""), ("repo", repo.as_deref().unwrap_or("")), ("kind", &kind)]);
        }
        rc
    }
}

/// `work law`'s table of contents, read from the statute book in the wiki checkout.
fn law_contents() -> String {
    let key = |k: &str| spira_config::process::cfg(k).ok().filter(|v| !v.is_empty());
    let path = key("SPIRA_WIKI").zip(key("SPIRA_LAW_BOOK")).map(|(wiki, book)| Path::new(&wiki).join(book).join("index.md"));
    path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| "(the statute book's index.md not found — run `work law`)\n".to_string())
}

/// Files the design's pilgrimage bead for the pilgrim, blocked by every build bead named with `--after`.
fn cmd_pilgrimage(home: &str, args: &[String]) -> i32 {
    let bead::PilgrimageArgs { design, repo, priority, after } = match bead::pilgrimage_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if after.is_empty() {
        eprintln!("bead: pilgrimage: --after <build-id> required: a pilgrimage with no build bead to wait for is claimable before the feature exists");
        return 2;
    }
    let text = match std::fs::read_to_string(&design) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bead: pilgrimage: reading {design}: {e}");
            return 2;
        }
    };
    let (title, body) = match bead::pilgrimage_spec(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bead: pilgrimage: refusing {design}: {e}");
            return 2;
        }
    };
    let body_path = env::temp_dir().join(format!("bead-pilgrimage-{}.md", std::process::id()));
    if let Err(e) = std::fs::write(&body_path, body) {
        eprintln!("bead: writing {}: {e}", body_path.display());
        return 1;
    }
    let exe = match env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("bead: pilgrimage: {e}");
            return 1;
        }
    };
    let mut file = vec!["--home".to_string(), home.to_string(), "file".into(), title, "--for".into(), "pilgrim".into(), "--repo".into(), repo, "--body-file".into(), body_path.to_string_lossy().into_owned()];
    if let Some(p) = priority {
        file.push("--priority".into());
        file.push(p);
    }
    // batch-job: re-enters bead file, which bounds its own store calls
    // FORK-EDGE: machine leg (catalogue phase) — the self re-exec becomes a task state
    let out = std::process::Command::new(exe).args(&file).stderr(Stdio::inherit()).output();
    let _ = std::fs::remove_file(&body_path);
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            eprintln!("bead: pilgrimage: {e}");
            return 1;
        }
    };
    if !out.status.success() {
        return out.status.code().unwrap_or(1);
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() || id.contains(char::is_whitespace) {
        eprintln!("bead: pilgrimage: filing printed no bead id");
        return 1;
    }
    for dep in &after {
        let rc = cmd_dep_add(&[id.clone(), dep.clone()]);
        if rc != 0 {
            eprintln!("bead: pilgrimage: {id} is filed but not blocked by {dep}; wire it with `bead.sh dep add {id} {dep}`");
            return rc;
        }
    }
    println!("{id}");
    0
}

// =========================================================================================
// amend
// =========================================================================================

fn cmd_amend(home: &str, args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("bead: amend: id required");
        return 2;
    }
    if args[0] == "--enforce" {
        return match args.get(1) {
            Some(aid) if args.len() == 2 => amend_enforce(aid),
            _ => {
                eprintln!("bead: amend: usage: bead.sh amend --enforce <amend-id>");
                2
            }
        };
    }
    if args[0] == "--settle" {
        return match args.get(1) {
            Some(id) if args.len() == 2 => amend_settle(id),
            _ => {
                eprintln!("bead: amend: usage: bead.sh amend --settle <bead-id>");
                2
            }
        };
    }
    let id = args[0].clone();
    let mut live = false;
    let mut ack_within: Option<String> = None;
    let mut ack: Option<String> = None;
    let mut accept = false;
    let mut decline: Option<String> = None;
    let mut note: Option<String> = None;
    let mut body_file: Option<String> = None;
    let mut append_file: Option<String> = None;
    let mut replace = false;
    let mut express = false;
    let mut priority: Option<i64> = None;
    let mut drafted_by: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drafted-by" => {
                i += 1;
                drafted_by = args.get(i).cloned();
            }
            "--note" => {
                i += 1;
                note = args.get(i).cloned();
            }
            "--body-file" => {
                i += 1;
                body_file = args.get(i).cloned();
            }
            "--append-body" => {
                i += 1;
                append_file = args.get(i).cloned();
            }
            "--replace" => replace = true,
            "--live" => live = true,
            "--ack-within" => {
                i += 1;
                ack_within = args.get(i).cloned();
            }
            "--ack" => {
                i += 1;
                ack = args.get(i).cloned();
            }
            "--accept" => accept = true,
            "--decline" => {
                i += 1;
                decline = args.get(i).cloned();
            }
            "--express" => express = true,
            "--priority" | "-p" => {
                i += 1;
                let Some(p) = args.get(i).and_then(|a| bead::parse_priority_arg(a)) else {
                    eprintln!("bead: amend: --priority takes 0..4 (or P0..P4)");
                    return 2;
                };
                priority = Some(p);
            }
            other => {
                eprintln!("bead: amend: unknown option: {other}");
                return 2;
            }
        }
        i += 1;
    }
    if let Some(aid) = &ack {
        let answer = match (accept, &decline) {
            (true, None) => amendment::State::Accepted,
            (false, Some(why)) if !why.trim().is_empty() => amendment::State::Declined(why.clone()),
            _ => {
                eprintln!("bead: amend: --ack <amend-id> takes exactly one of --accept or --decline \"<why>\"");
                return 2;
            }
        };
        return amend_ack(&id, aid, answer);
    }
    if accept || decline.is_some() {
        eprintln!("bead: amend: --accept and --decline qualify --ack <amend-id>");
        return 2;
    }
    if ack_within.is_some() && !live {
        eprintln!("bead: amend: --ack-within qualifies --live");
        return 2;
    }
    if live && (body_file.is_some() || replace || express || priority.is_some() || (note.is_none() && append_file.is_none())) {
        eprintln!("bead: amend: --live takes --note and/or --append-body only: the text is merged into the description when it is accepted");
        return 2;
    }
    if note.is_none() && body_file.is_none() && append_file.is_none() && !express && priority.is_none() {
        eprintln!("bead: amend: --note, --body-file, --append-body, --express, or --priority required");
        return 2;
    }
    if body_file.is_some() && append_file.is_some() {
        eprintln!("bead: amend: --body-file and --append-body are exclusive");
        return 2;
    }
    if body_file.is_some() && !replace {
        eprintln!("bead: amend: --body-file REPLACES the description of {id}; pass --replace to do that, or --append-body F to keep the old text and add F under a dated separator");
        return 2;
    }
    if replace && body_file.is_none() {
        eprintln!("bead: amend: --replace only qualifies --body-file");
        return 2;
    }

    if replace && is_concierge() {
        let (_, shown) = door(&s(&["show", &id, "--json"]));
        let labels = parse_show_row(&shown).map(|r| r.labels).unwrap_or_default();
        let skuld_bound = rune_names(home).iter().any(|r| {
            let own: Vec<String> = rune_label(home, r).split(',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            requires_skuld(home, r) && bead::is_builder_bead(&labels, &own)
        });
        if skuld_bound {
            let body = body_file.as_deref().map(|f| std::fs::read_to_string(f).unwrap_or_default()).unwrap_or_default();
            let title = bead::parse_title(&shown).unwrap_or_default();
            if let Err(rc) = skuld_gate("amend", &title, &body, drafted_by.as_deref()) {
                return rc;
            }
        }
    } else if drafted_by.is_some() {
        eprintln!("bead: amend: --drafted-by applies only to the Concierge's --replace of a builder bead");
        return 2;
    }

    let mut override_note: Option<String> = None;
    if note.is_some() || body_file.is_some() || append_file.is_some() {
        let (_, shown) = door(&s(&["show", &id, "--json"]));
        let repo = bead::parse_show_row(&shown)
            .and_then(|r| r.labels.iter().find_map(|l| l.strip_prefix("repo:").map(str::to_string)));
        if let Some(repo) = repo {
            let mut text = note.clone().unwrap_or_default();
            for f in [&body_file, &append_file].into_iter().flatten() {
                text.push('\n');
                text.push_str(&std::fs::read_to_string(f).unwrap_or_default());
            }
            match identity_guard(&repo, &text) {
                Err(rc) => return rc,
                Ok(line) => override_note = line,
            }
        }
    }

    let mut statute_note: Option<String> = None;
    if note.is_some() || body_file.is_some() || append_file.is_some() {
        let mut text = note.clone().unwrap_or_default();
        for f in [&body_file, &append_file].into_iter().flatten() {
            text.push('\n');
            text.push_str(&std::fs::read_to_string(f).unwrap_or_default());
        }
        match statute_guard(&text) {
            Err(rc) => return rc,
            Ok(line) => statute_note = line,
        }
    }

    if live {
        let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
        let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
        if bead::claimdesc::live_dwarf(&id, &run, &mail) {
            return amend_live(&id, &mail, note.as_deref(), append_file.as_deref(), ack_within.as_deref());
        }
        eprintln!("bead: amend: no live dwarf on {id}; recording the amendment without an acknowledgement");
    }

    let mut changed = String::new();
    let mut applied: Vec<String> = Vec::new();
    let mut rc = 0;
    if let Some(line) = &override_note {
        rc |= put_note(&id, line);
    }
    if let Some(line) = &statute_note {
        rc |= put_note(&id, line);
    }
    if let Some(line) = &statute_note {
        rc |= put_note(&id, line);
    }
    if let Some(new) = priority {
        let (_, out) = door(&s(&["show", &id, "--json"]));
        let Some(old) = bead::parse_priority(&out) else {
            eprintln!("bead: amend: cannot read the current priority of {id}; refusing");
            return 1;
        };
        let actor = std::env::var("BEADS_ACTOR")
            .ok()
            .filter(|a| !a.is_empty())
            .or_else(|| std::env::var("USER").ok().filter(|a| !a.is_empty()))
            .unwrap_or_else(|| "unknown".to_string());
        let upd = put(&["update", &id, "--priority", &new.to_string()], None);
        rc |= upd;
        if upd == 0 {
            let text = bead::priority_note(&actor, Some(old), new, note.as_deref());
            let wrote = put_note(&id, &text);
            rc |= wrote;
            if wrote == 0 {
                bead::claimdesc::append_change(&mut changed, &text);
                applied.push(format!("priority P{old} → P{new}"));
            }
            note = None;
        }
    }
    if express {
        match spira_store::lifecycle_row::set_express(&id, true) {
            Ok(()) => {
                bead::claimdesc::append_change(&mut changed, "Marked express.");
                applied.push("marked express".into());
            }
            Err(e) => {
                eprintln!("bead: amend: {id} not marked express: {e}");
                rc |= 1;
            }
        }
    }
    if let Some(n) = &note {
        let wrote = put_note(&id, n);
        rc |= wrote;
        if wrote == 0 {
            bead::claimdesc::append_change(&mut changed, n);
            applied.push("note added".into());
        }
    }
    if body_file.is_some() || append_file.is_some() {
        let (_, out) = door(&s(&["show", &id, "--json"]));
        let Some(old) = bead::parse_description(&out) else {
            eprintln!("bead: amend: cannot read the current description of {id}; refusing");
            return 1;
        };
        let new_body = if let Some(af) = &append_file {
            let addition = match std::fs::read_to_string(af) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bead: amend: cannot read {af}: {e}");
                    return 1;
                }
            };
            bead::appended_body(&old, &addition, &spira_otel::utc_date(std::time::SystemTime::now()))
        } else {
            eprintln!("bead: amend: {id}: {}", bead::replaced_body_summary(&old));
            match body_file.as_deref().map(std::fs::read_to_string).transpose() {
                Ok(b) => b.unwrap_or_default(),
                Err(e) => {
                    eprintln!("bead: amend: cannot read the body file: {e}");
                    return 1;
                }
            }
        };
        rc |= put(&["update", &id, "--body-file", "-", bead::claimdesc::FORCE_FLAG, "bead.sh amend"], Some(&new_body));
        let (text, plain) = if append_file.is_some() { ("Description appended.", "description appended") } else { ("Description updated.", "description updated") };
        bead::claimdesc::append_change(&mut changed, text);
        applied.push(plain.into());
    }

    // Best-effort notification, same as bdq's own `acknowledge_forced_edit`: the real
    // amend above has already landed, so a config-resolution failure here must not abort
    // and lose `rc` — `notify_live_dwarf` already treats an empty SPIRA_RUN/SPIRA_MAIL as
    // "nothing to notify".
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
    let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
    for line in bead::claimdesc::amend_report(&id, &applied, &notify_live_dwarf(&id, &changed, &run, &mail)) {
        println!("bead: amend: {line}");
    }
    rc
}

fn now_epoch() -> i64 {
    env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    })
}

fn amender() -> String {
    ["BEADS_ACTOR", "USER"].iter().find_map(|k| env::var(k).ok().filter(|a| !a.is_empty())).unwrap_or_else(|| "concierge".into())
}

fn amend_mail_cfg() -> Option<String> {
    spira_config::process::cfg("SPIRA_MAIL").ok().filter(|m| !m.is_empty())
}

fn merge_into_body(id: &str, addition: &str) -> i32 {
    let (_, out) = door(&s(&["show", id, "--json"]));
    let Some(old) = bead::parse_description(&out) else {
        eprintln!("bead: amend: cannot read the current description of {id}; refusing");
        return 1;
    };
    let date = spira_otel::utc_date(std::time::SystemTime::now());
    let body = bead::appended_body(&old, addition, &date);
    put(&["update", id, "--body-file", "-", bead::claimdesc::FORCE_FLAG, "bead.sh amend"], Some(&body))
}

fn amend_live(id: &str, mail: &str, note: Option<&str>, append_file: Option<&str>, ack_within: Option<&str>) -> i32 {
    let secs = match ack_within {
        None => amendment::DEFAULT_ACK_SECS,
        Some(d) => match amendment::parse_duration(d) {
            Some(n) => n,
            None => {
                eprintln!("bead: amend: --ack-within takes a duration such as 90s, 10m or 2h");
                return 2;
            }
        },
    };
    let mut text = note.unwrap_or_default().to_string();
    if let Some(f) = append_file {
        match std::fs::read_to_string(f) {
            Ok(t) => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&t);
            }
            Err(e) => {
                eprintln!("bead: amend: cannot read {f}: {e}");
                return 1;
            }
        }
    }
    let rec = match amendment::create(mail, id, &amender(), &text, now_epoch(), secs) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bead: amend: cannot record the amendment: {e}");
            return 1;
        }
    };
    let rc = put_note(id, &format!("Live amendment {} by {} awaiting acknowledgement (within {secs}s):\n\n{}", rec.id, rec.amender, rec.text.trim()));
    bead::claimdesc::mail_to(
        &format!("dwarf-{id}"),
        "amend <amend@spira>",
        "Amendment to answer",
        &amendment::block_text(&rec),
    );
    println!("{}", rec.id);
    eprintln!("bead: amend: {} recorded on {id}; the dwarf is blocked until it runs `work ack {} --accept|--decline`; after {secs}s or a decline: bead.sh amend --enforce {}", rec.id, rec.id, rec.id);
    rc
}

fn amend_ack(id: &str, aid: &str, answer: amendment::State) -> i32 {
    let Some(mail) = amend_mail_cfg() else {
        eprintln!("bead: amend: SPIRA_MAIL is not configured");
        return 1;
    };
    let rec = match amendment::answer(&mail, id, aid, answer.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bead: amend: {e}");
            return 1;
        }
    };
    let mut rc = 0;
    let (verdict, tell) = match &answer {
        amendment::State::Accepted => {
            rc |= merge_into_body(id, &rec.text);
            ("accepted".to_string(), format!("Amendment {aid} on {id} was accepted by the dwarf and is merged into the description."))
        }
        amendment::State::Declined(why) => (
            format!("declined: {why}"),
            format!("Amendment {aid} on {id} was DECLINED by the dwarf: {why}\n\nTo slay the session and requeue with the amendment merged: bead.sh amend --enforce {aid}"),
        ),
        _ => unreachable!(),
    };
    rc |= put_note(id, &format!("Amendment {aid} {verdict}"));
    bead::claimdesc::mail_to(&rec.amender, "amend <amend@spira>", &format!("Amendment {aid} answered"), &tell);
    rc
}

/// A released bead's unanswered amendments are merged into its body, so the next claim sees them.
fn amend_settle(id: &str) -> i32 {
    let Some(mail) = amend_mail_cfg() else {
        eprintln!("bead: amend: SPIRA_MAIL is not configured");
        return 1;
    };
    let mut rc = 0;
    for rec in amendment::unanswered(&mail, id) {
        if merge_into_body(id, &rec.text) != 0 {
            eprintln!("bead: amend: could not merge {} into {id}; left pending", rec.id);
            rc = 1;
            continue;
        }
        if let Err(e) = amendment::mark_enforced(&mail, &rec) {
            eprintln!("bead: amend: {e}");
            rc = 1;
            continue;
        }
        rc |= put_note(id, &format!("Amendment {} merged into the body: its session ended before it was answered", rec.id));
    }
    rc
}

fn amend_enforce(aid: &str) -> i32 {
    let Some(mail) = amend_mail_cfg() else {
        eprintln!("bead: amend: SPIRA_MAIL is not configured");
        return 1;
    };
    let Some(rec) = amendment::find(&mail, aid) else {
        eprintln!("bead: amend: no amendment {aid}");
        return 1;
    };
    let why = match amendment::enforceable(&rec, now_epoch()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("bead: amend: {e}");
            return 1;
        }
    };
    let id = rec.bead.clone();
    let mut rc = merge_into_body(&id, &rec.text);
    if rc != 0 {
        eprintln!("bead: amend: could not merge {aid} into {id}; not slaying");
        return rc;
    }
    if let Err(e) = amendment::mark_enforced(&mail, &rec) {
        eprintln!("bead: amend: {e}");
        return 1;
    }
    let reason = format!("amendment {aid} {why}; slain and requeued with the amendment merged into the body");
    rc |= put_note(&id, &format!("Enforced {aid}: {reason}"));
    // batch-job: slay waits out the holder's shutdown and salvages its work
    // FORK-EDGE: needs a hel/Units library for slay (stop the holder, salvage, reopen); slay is another crate's binary
    let slay = Command::new(match spira_config::release_env::tool("slay") { Ok(v) => v, Err(e) => { eprintln!("bead: {e}"); return 1; } })
        .args(["--bead", &id, "--why", &reason, "--reopen"])
        .envs(match spira_config::release_env::child_path_env_for_process() { Ok(v) => v, Err(e) => { eprintln!("bead: {e}"); return 1; } })
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127);
    if slay != 0 {
        eprintln!("bead: amend: slay --bead {id} failed ({slay}); the amendment is merged and marked enforced");
    }
    rc | slay
}

// =========================================================================================
// ideas
// =========================================================================================

fn cmd_ideas(args: &[String]) -> i32 {
    let json = match args {
        [] => false,
        [a] if a == "--json" => true,
        _ => {
            eprintln!("usage: bead.sh ideas [--json]");
            return 2;
        }
    };
    let label = bead::door::IDEA_LABEL;
    let client = match spira_store::lc_call::LcClient::configured() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bead: {e}");
            return DOOR_UNREACHABLE;
        }
    };
    let rows = match bead::door::ideas(&client, label) {
        Ok(rows) => rows,
        Err(code) => {
            eprintln!("bead: ideas: content list exited {code}");
            return 1;
        }
    };
    if json {
        println!("{}", serde_json::Value::Array(rows.iter().map(|r| serde_json::json!({"id": r.id, "title": r.title, "created_at": r.created_at})).collect()));
    } else {
        for r in &rows {
            println!("{}\t{}", r.id, r.title);
        }
    }
    0
}

// =========================================================================================
// contract
// =========================================================================================

fn cmd_contract(home: &str) -> i32 {
    println!("PERSONAS");
    for f in rune_names(home) {
        let labels = rune_label(home, &f);
        println!("{}", persona_line(&f, &labels));
    }
    println!();
    println!("KINDS");
    print!("{}", schema_kinds_passthrough(home));
    println!();
    println!("REPOS");
    print!("{}", repos_section(&load_repos()));
    0
}

// =========================================================================================
// event — sp-ogu8x, wave 4.24, family Z. `spira/lib.sh`'s `spira_event` is now a one-line
// shim onto this subcommand (see DESIGN.md "event"); every other caller (the dwarf/gate-check/
// landing-pass/queue/sentinel seams) still types the bash function name unchanged.
// =========================================================================================

fn cmd_event(args: &[String]) -> i32 {
    // Mirrors the bash's own positional defaults: `target="${2:--}"`, so a caller that
    // omits it gets "-" (a plan-level event), not an empty string.
    let kind = args.first().cloned().unwrap_or_default();
    let target = args.get(1).cloned().unwrap_or_else(|| "-".to_string());
    let title = args.get(2).cloned().unwrap_or_default();
    let detail = args.get(3).cloned().unwrap_or_default();

    // `dir="$SPIRA_RUN/events"` in the bash: an unset SPIRA_RUN concatenates to a literal
    // leading slash, which every real caller's conf.sh-derived environment avoids by
    // always setting SPIRA_RUN. Matched here as a plain string join, not `Path::join` on an
    // empty base (which would silently go relative instead) — see DESIGN.md "event".
    // Unset or empty SPIRA_RUN is refused by name (sp-0c1wz): the old join made a relative
    // path and the event vanished with rc 0. SPIRA_RUN is a registered key, so this now
    // reads it through `cfg` rather than the process environment directly.
    let run_dir = match spira_config::process::cfg("SPIRA_RUN") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        Ok(_) => {
            eprintln!("spira_event: SPIRA_RUN resolved empty — refusing rather than drop the event");
            return 2;
        }
        Err(e) => {
            eprintln!("spira_event: {e}");
            return 2;
        }
    };
    // SPIRA_EVENT_COOLDOWN / SPIRA_NOW are not registered config keys (spira/conf.d has no
    // entry for either) — left as plain env reads with their existing defaults.
    let cooldown: i64 = env::var("SPIRA_EVENT_COOLDOWN").ok().and_then(|v| v.parse().ok()).unwrap_or(3600);
    let now: i64 = env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });

    match bead::event::emit(&run_dir, cooldown, now, &kind, &target, &title, &detail) {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

// =========================================================================================
// lint
// =========================================================================================

/// `bead judge-create <labels-csv> [type]` — the lint judge applied to a bead about to be
/// created open. Exit 1 with the partition list on stderr when the bead would be unclaimable;
/// the dwarf fence calls this so a raw `bd create` is refused by the same rule `file` obeys.
fn cmd_judge_create(home: &str, args: &[String]) -> i32 {
    let labels_csv = args.first().cloned().unwrap_or_default();
    let ty = args.get(1).map(String::as_str).unwrap_or("task");
    let labels = labels_csv.replace(',', " ");
    let scope = cfg_label("SPIRA_SCOPE_LABEL");
    if !scope.is_empty() && !labels.split_whitespace().any(|l| l == scope) {
        return 0;
    }
    let personas: Vec<(String, String)> = rune_names(home)
        .into_iter()
        .map(|f| {
            let l = rune_label(home, &f);
            (f, l)
        })
        .collect();
    let partitions = runes_partitions(&personas, &scope);
    let no_loop = cfg_label("SPIRA_NO_LOOP_LABEL");
    // A bead being filed is about to wait for dispatch.
    let (_, out) = lint_judge(&labels, true, ty, &partitions.join(" "), &no_loop);
    if out.iter().any(|l| l.starts_with("no partition label")) {
        eprintln!(
            "{}; add one of: {}",
            out.iter().find(|l| l.starts_with("no partition label")).unwrap(),
            partitions.join(", ")
        );
        return 1;
    }
    0
}

fn lifecycle_rows(ids: &[String]) -> bead::door::LifecycleRows {
    spira_store::lc_state::list_ids(ids).map(spira_store::lc_state::index)
}

fn cmd_lint(home: &str, args: &[String]) -> i32 {
    let set = if args.is_empty() || args[0] == "--all" {
        bead::door::Set::All
    } else {
        bead::door::Set::Ids(args.to_vec())
    };

    let personas: Vec<(String, String)> = rune_names(home)
        .into_iter()
        .map(|f| {
            let labels = rune_label(home, &f);
            (f, labels)
        })
        .collect();
    // The scope exclusion here defaults to EMPTY, not "spira": `file` reads its own default.
    let partitions = runes_partitions(&personas, &cfg_label("SPIRA_SCOPE_LABEL"));
    let spec_partitions = rune_names(home)
        .into_iter()
        .filter(|f| requires_build_spec(home, f))
        .map(|f| rune_label(home, &f).split(',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .collect();
    let cfg = bead::LintCfg {
        partitions,
        spec_partitions,
        no_loop_label: cfg_label("SPIRA_NO_LOOP_LABEL"),
        ask_label: cfg_label("SPIRA_ASK_LABEL"),
        incident_label: cfg_label("SPIRA_INCIDENT_LABEL"),
    };

    let client = match spira_store::lc_call::LcClient::configured() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bead: {e}");
            return DOOR_UNREACHABLE;
        }
    };
    let src = match bead::door::DoorSource::load(&client, set, lifecycle_rows) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bead: lint: {e}");
            return DOOR_UNREACHABLE;
        }
    };
    let n = bead::LintSource::ids(&src).len();
    let (rc, lines) = bead::lint(&src, &cfg);
    for l in &lines {
        eprintln!("{l}");
    }
    if lines.is_empty() {
        println!("bead: {n} bead(s) checked, ok");
    }
    rc
}

// =========================================================================================
// dep add
// =========================================================================================

fn cmd_dep_add(args: &[String]) -> i32 {
    let mut id: Option<String> = None;
    let mut depid: Option<String> = None;
    let mut dep_type: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut pos_n = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--type" | "-t" => {
                i += 1;
                dep_type = args.get(i).cloned();
            }
            "--blocked-by" => {
                i += 1;
                depid = args.get(i).cloned();
            }
            "--depends-on" => {
                i += 1;
                depid = args.get(i).cloned();
            }
            "--file" => {
                eprintln!("bead: dep add: --file bulk wiring is not wrapped; add each edge with `bead.sh dep add`; the content door takes no bulk file");
                return 2;
            }
            a if a.starts_with('-') => rest.push(a.to_string()),
            a => match pos_n {
                0 => {
                    id = Some(a.to_string());
                    pos_n = 1;
                }
                1 => {
                    if depid.is_none() {
                        depid = Some(a.to_string());
                    }
                    pos_n = 2;
                }
                _ => rest.push(a.to_string()),
            },
        }
        i += 1;
    }

    let (id, depid) = match (id, depid) {
        (Some(i), Some(d)) => (i, d),
        _ => {
            eprintln!("usage: bead.sh dep add <id> <depends-on-id> [--type <type>]");
            return 2;
        }
    };

    if is_blocks_type(dep_type.as_deref()) {
        let incident_label = cfg_label("SPIRA_INCIDENT_LABEL");
        let (_, out) = door(&s(&["show", &depid, "--json"]));
        let target = parse_show_row(&out);
        if target.as_ref().is_some_and(|r| r.issue_type == "epic") {
            eprintln!("{}", epic_blocks_refusal(&id, &depid));
            return 1;
        }
        let target_labels = target.map(|r| r.labels).unwrap_or_default();
        if target_labels.iter().any(|l| l == &incident_label) {
            eprintln!("{}", incident_blocks_refusal(&id, &depid, &incident_label));
            return 1;
        }
    }

    let mut call = s(&["dep", "add"]);
    call.push(id.clone());
    call.push(depid.clone());
    if let Some(t) = &dep_type {
        call.push("--type".into());
        call.push(dep_type_name(t).into());
    }
    call.extend(rest);
    let code = put(&call.iter().map(String::as_str).collect::<Vec<_>>(), None);
    if code != 0 || !is_blocks_type(dep_type.as_deref()) {
        return code;
    }
    let Some(row) = shown_row(&id) else {
        eprintln!("bead: dep add: the edge {id} -> {depid} is in, but {id} could not be read, so the registry rows naming it did not move to {depid}");
        return 1;
    };
    if !row.labels.iter().any(|l| l == &cfg_label("SPIRA_INCIDENT_LABEL")) {
        return 0;
    }
    match repoint_registry(&id, &depid) {
        Ok(moved) => {
            if let Some(line) = repointed_line(&moved, &depid) {
                eprintln!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("{}", repoint_failed_line(&id, &depid, &e));
            1
        }
    }
}

fn repointed_line(moved: &[String], to: &str) -> Option<String> {
    (!moved.is_empty()).then(|| format!("bead: dep add: registry row(s) {} now name {to}", moved.join(" ")))
}

fn repoint_failed_line(from: &str, to: &str, why: &str) -> String {
    format!("bead: dep add: the edge {from} -> {to} is in, but the registry rows naming {from} did not move to {to}: {why}")
}

fn repoint_argv(from: &str, to: &str) -> Vec<String> {
    s(&["repoint", "--from", from, "--to", to, "--actor", "bead"])
}

fn moved_ids(stdout: &str) -> Vec<String> {
    stdout.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
type RepointSeam = (Vec<(String, String)>, Result<Vec<String>, String>);

#[cfg(test)]
thread_local! {
    static TEST_REPOINT: std::cell::RefCell<RepointSeam> = const { std::cell::RefCell::new((Vec::new(), Ok(Vec::new()))) };
}

/// The seam: moves the test-registry rows naming `from` to `to`; returns the moved test ids.
fn repoint_registry(from: &str, to: &str) -> Result<Vec<String>, String> {
    #[cfg(test)]
    return TEST_REPOINT.with(|r| {
        let mut r = r.borrow_mut();
        r.0.push((from.to_string(), to.to_string()));
        r.1.clone()
    });
    #[cfg(not(test))]
    {
        let bin = spira_config::release_env::tool("test-registry")?;
        let out = spira_config::bounded::bounded(bin)
            .args(repoint_argv(from, to))
            .output()
            .map_err(|e| format!("test-registry: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(moved_ids(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// bd's spellings of a `blocks` edge, normalised to the name the lifecycle store records.
fn dep_type_name(t: &str) -> &str {
    if is_blocks_type(Some(t)) {
        "blocks"
    } else {
        t
    }
}

// =========================================================================================
// dep epic-edges / dep convert — the reconciler's observation and remedy for a blocks edge
// onto an open epic.
// =========================================================================================

fn cmd_dep_epic_edges() -> i32 {
    let (code, out) = door(&s(&["list", "--type", "epic", "--brief", "--json", "--limit", "0"]));
    if code != 0 {
        eprintln!("bead: dep epic-edges: bd list exited {code}");
        return 1;
    }
    let epics = parse_list_ids(&out);
    if epics.is_empty() {
        return 0;
    }
    for epic in &epics {
        let (code, out) = door(&s(&["dep", "list", epic, "--direction", "up", "--type", "blocks", "--json"]));
        if code != 0 {
            eprintln!("bead: dep epic-edges: bd dep list {epic} exited {code}");
            return 1;
        }
        for child in parse_blocks_targets(&out) {
            println!("{child} {epic}");
        }
    }
    0
}

fn convert_left_edge(id: &str, epic: &str, rc: i32) -> String {
    format!("bead: dep convert: {id} -> {epic} blocks edge was removed but the parent-child edge was not added (exit {rc}); the edge is now absent: bead.sh dep add {id} {epic} --type parent-child")
}

fn cmd_dep_convert(args: &[String]) -> i32 {
    let (id, epic) = match args {
        [i, e] if !i.starts_with('-') && !e.starts_with('-') => (i.clone(), e.clone()),
        _ => {
            eprintln!("usage: bead.sh dep convert <id> <epic-id>");
            return 2;
        }
    };
    let (_, out) = door(&s(&["show", &epic, "--json"]));
    if parse_show_row(&out).is_none_or(|r| r.issue_type != "epic") {
        eprintln!("bead: dep convert: refusing — {epic} is not an epic");
        return 1;
    }
    let rc = put(&["dep", "remove", &id, &epic], None);
    if rc != 0 {
        eprintln!("bead: dep convert: the edge removal was refused (exit {rc}); the edge is unchanged");
        return rc;
    }
    let rc = put(&["dep", "add", &id, &epic, "--type", "parent-child"], None);
    if rc != 0 {
        eprintln!("{}", convert_left_edge(&id, &epic, rc));
        return rc;
    }
    println!("converted edge: {id} -> {epic} blocks => parent-child");
    0
}

// =========================================================================================
// dep remove
// =========================================================================================

fn cmd_label(home: &str, args: &[String]) -> i32 {
    let (verb, id, label) = match args {
        [v, i, l] if (v == "add" || v == "remove") && !i.starts_with('-') && !l.starts_with('-') => (v.as_str(), i.clone(), l.clone()),
        _ => {
            eprintln!("usage: bead.sh label add|remove <id> <label>");
            return 2;
        }
    };

    let personas: Vec<(String, String)> = rune_names(home)
        .into_iter()
        .map(|f| {
            let labels = rune_label(home, &f);
            (f, labels)
        })
        .collect();
    let partitions = runes_partitions(&personas, "");
    if let Err(r) = bead::filing::partition_label(verb, &label, &partitions) {
        eprint!("{}", r.text);
        return r.code;
    }

    let (_, out) = door(&s(&["show", &id, "--json"]));
    let Some(row) = parse_show_row(&out) else {
        eprintln!("bead: label {verb}: refusing — unknown bead {id}");
        return 1;
    };
    let has = row.labels.iter().any(|l| l == &label);
    if (verb == "add") == has {
        println!("{id}: label {label} already {}", if has { "present" } else { "absent" });
        return 0;
    }

    if let Some(repo) = label.strip_prefix("repo:") {
        if let Err(code) = mirror_repo(verb, &id, repo) {
            return code;
        }
    }

    let rc = put(&["label", verb, &id, &label], None);
    if rc == 0 {
        println!("{verb}ed label: {id} {label}");
    }
    rc
}

/// Point the lifecycle row at the repo a `repo:` label names, before the label moves: claims,
/// worktrees and rounds read the row, and a WORKING bead is refused by it.
fn mirror_repo(verb: &str, id: &str, repo: &str) -> Result<(), i32> {
    let client = match lc() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bead: {e}");
            return Err(1);
        }
    };
    match bead::lc::set_repo(&client, verb, id, repo) {
        bead::lc::SetRepo::Applied => Ok(()),
        bead::lc::SetRepo::Refused(why) => {
            eprint!("bead: label {verb}: refusing — {why}");
            Err(1)
        }
        bead::lc::SetRepo::Failed => {
            eprintln!("bead: label {verb}: the lifecycle row of {id} could not be updated; nothing changed. Retry, or `spira-lc set-repo {id} <repo>`");
            Err(1)
        }
    }
}

const DROPPED_LABEL: &str = "spira-dropped";

/// End a work bead as won't-do: the lifecycle machine does the work (refuse on open dependents,
/// leave every batch, DROPPED with the reason, close the store); this labels it afterwards.
fn cmd_drop(args: &[String]) -> i32 {
    let usage = "usage: bead.sh drop <id> --reason <text>";
    let (mut id, mut reason) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reason" => reason = it.next().cloned(),
            x if !x.starts_with('-') && id.is_none() => id = Some(x.to_string()),
            _ => {
                eprintln!("{usage}");
                return 2;
            }
        }
    }
    let (Some(id), Some(reason)) = (id, reason.filter(|r| !r.trim().is_empty())) else {
        eprintln!("{usage}");
        return 2;
    };

    let (_, out) = door(&s(&["show", &id, "--json"]));
    if parse_show_row(&out).is_none() {
        eprintln!("bead: drop: refusing — unknown bead {id}");
        return 1;
    }

    // batch-job: ejects from batches and closes the store, bounded by timeout(1)
    // FORK-EDGE: unserved verb drop-bead (the spira-lc CLI composes it; the socket does not serve it)
    let status = Command::new("timeout")
        .args(["120", match spira_store::lifecycle_row::lc_bin() { Ok(v) => v, Err(e) => { eprintln!("bead: {e}"); return 1; } }.as_str(), "drop-bead", &id, "--reason", &reason, "--actor", "bead"])
        .envs(match spira_config::release_env::child_path_env_for_process() { Ok(v) => v, Err(e) => { eprintln!("bead: {e}"); return 1; } })
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127);
    if status != 0 {
        return status;
    }
    let rc = label_dropped(&id);
    if rc != 0 {
        eprintln!("bead: drop: {id} is DROPPED but could not be labelled; add it: bead.sh label add {id} {DROPPED_LABEL}");
        return rc;
    }
    println!("dropped: {id} ({reason})");
    0
}

fn label_dropped(id: &str) -> i32 {
    put(&["label", "add", id, DROPPED_LABEL], None)
}

fn cmd_dep_remove(args: &[String]) -> i32 {
    let (id, depid) = match args {
        [i, d] if !i.starts_with('-') && !d.starts_with('-') => (i.clone(), d.clone()),
        _ => {
            eprintln!("usage: bead.sh dep remove <id> <depends-on-id>");
            return 2;
        }
    };

    for b in [&id, &depid] {
        let (_, out) = door(&s(&["show", b, "--json"]));
        if parse_show_row(&out).is_none() {
            eprintln!("bead: dep remove: refusing — unknown bead {b}");
            return 1;
        }
    }

    let (_, out) = door(&s(&["dep", "list", &id, "--json"]));
    if !parse_blocks_targets(&out).iter().any(|t| t == &depid) {
        eprintln!("bead: dep remove: refusing — {id} has no dependency edge on {depid}");
        return 1;
    }

    let rc = put(&["dep", "remove", &id, &depid], None);
    if rc == 0 {
        println!("removed edge: {id} -> {depid}");
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    type Sent = Vec<(Vec<String>, Option<String>)>;

    struct Door {
        sent: Mutex<Sent>,
        answers: Vec<(&'static str, i32, &'static str)>,
        first: Mutex<Vec<(&'static str, i32, &'static str)>>,
        request_ids: Mutex<Vec<Option<String>>>,
        log: Arc<Mutex<Vec<String>>>,
    }

    struct LogOut(Arc<Mutex<Vec<String>>>);

    impl std::io::Write for LogOut {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(format!("out:{}", String::from_utf8_lossy(b)));
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Transport for Door {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _: std::time::Duration) -> Result<Reply, String> {
            self.sent.lock().unwrap().push((argv.to_vec(), stdin.map(str::to_string)));
            let line = argv.join(" ");
            self.log.lock().unwrap().push(line.clone());
            self.request_ids.lock().unwrap().push(spira_store::lc_call::current_request_id());
            let once = {
                let mut first = self.first.lock().unwrap();
                first.iter().position(|(p, _, _)| line.starts_with(p)).map(|i| first.remove(i))
            };
            let (_, code, text) = once.or_else(|| self.answers.iter().find(|(p, _, _)| line.starts_with(p)).copied()).unwrap_or(("", 0, ""));
            Ok(Reply { code, out: if code == 0 { text.into() } else { String::new() }, err: if code == 0 { String::new() } else { text.into() }, read_ended: false })
        }
    }

    const BEAD: &str = r#"[{"id":"sp-a","issue_type":"task","priority":2,"description":"old","labels":["repo:r"]}]"#;

    fn install(answers: Vec<(&'static str, i32, &'static str)>) -> Arc<Door> {
        let d = Arc::new(Door { sent: Mutex::new(vec![]), answers, first: Mutex::new(vec![]), request_ids: Mutex::new(vec![]), log: Arc::default() });
        TEST_DOOR.with(|t| *t.borrow_mut() = Some(LcClient::over(d.clone(), true)));
        d
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    fn writes(d: &Door) -> Sent {
        const READS: [&str; 3] = ["show", "list", "ref"];
        d.sent.lock().unwrap().iter().filter(|(a, _)| !READS.contains(&a[1].as_str()) && !(a[1] == "dep" && a[2] == "list")).cloned().collect()
    }

    fn sent(d: &Door) -> Vec<Vec<String>> {
        writes(d).into_iter().map(|(a, _)| a).collect()
    }

    fn configured(dir: &std::path::Path) -> testkit::EnvGuard {
        let toml = spira_config::process::fixture_toml(dir, &[]);
        testkit::env(&[("SPIRA_TOML", Some(&toml.to_string_lossy())), ("SPIRA_HOME", Some(&testkit::manifest_dir().join("../spira").to_string_lossy()))])
    }

    #[test_id(t5abe2z)]
    #[test]
    fn every_bead_write_goes_to_the_content_door() {
        let dir = testkit::TempDir::new("bead-door-writes");
        let _env = configured(dir.path());
        let body = dir.path().join("body.md");
        std::fs::write(&body, "the new text").unwrap();
        let body = body.to_string_lossy().into_owned();
        let show = ("content show", 0, BEAD);

        let d = install(vec![show]);
        assert_eq!(cmd_amend(".", &argv(&["sp-a", "--note", "n"])), 0);
        assert_eq!(writes(&d), vec![(argv(&["content", "note", "sp-a", "--stdin"]), Some("n".into()))]);

        let d = install(vec![show]);
        assert_eq!(cmd_amend(".", &argv(&["sp-a", "-p", "1"])), 0);
        let w = writes(&d);
        assert_eq!(w[0].0, argv(&["content", "update", "sp-a", "--priority", "1"]));
        assert_eq!(w[1].0, argv(&["content", "note", "sp-a", "--stdin"]));
        assert!(w[1].1.as_deref().unwrap().contains("P2"), "{w:?}");

        let d = install(vec![show]);
        assert_eq!(cmd_amend(".", &argv(&["sp-a", "--body-file", &body, "--replace"])), 0);
        assert_eq!(writes(&d), vec![(argv(&["content", "update", "sp-a", "--body-file", "-", "--force-claimed", "bead.sh amend"]), Some("the new text".into()))]);

        let d = install(vec![show]);
        assert_eq!(cmd_dep_add(&argv(&["sp-a", "sp-b"])), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "add", "sp-a", "sp-b"])]);

        let d = install(vec![show, ("content dep list", 0, r#"[{"depends_on_id":"sp-a"}]"#)]);
        assert_eq!(cmd_dep_remove(&argv(&["sp-a", "sp-a"])), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "remove", "sp-a", "sp-a"])]);

        let d = install(vec![show]);
        assert_eq!(cmd_label(".", &argv(&["add", "sp-a", "tag"])), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "label", "add", "sp-a", "tag"])]);

        let d = install(vec![]);
        assert_eq!(label_dropped("sp-a"), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "label", "add", "sp-a", DROPPED_LABEL])]);

        let d = install(vec![("content create", 0, "sp-new\n")]);
        let args = argv(&["create", "T", "-l", "repo:r", "--silent", "--body-file", "-"]);
        assert_eq!(door_create(&mut std::io::sink(), &args, Some("the body"), None, &[], Some("T"), Some("T"), false, None, Some("r"), None), 0);
        let w = writes(&d);
        assert_eq!(w[0], (argv(&["content", "create", "T", "-l", "repo:r", "--silent", "--body-file", "-"]), Some("the body".into())));
        assert_eq!(w[1].0[..2], argv(&["create-bead", "sp-new"])[..], "the lifecycle row is the store's own verb on the same client");
    }

    fn repoint_calls() -> Vec<(String, String)> {
        TEST_REPOINT.with(|r| std::mem::take(&mut r.borrow_mut().0))
    }

    #[test_id(tt0ceft)]
    #[test]
    fn the_repoint_wiring_names_the_registry_verb_and_reads_its_stdout() {
        assert_eq!(repoint_argv("sp-i", "sp-f"), argv(&["repoint", "--from", "sp-i", "--to", "sp-f", "--actor", "bead"]));
        assert_eq!(moved_ids("suite:a.sh\nsuite:b.sh\n"), ["suite:a.sh", "suite:b.sh"]);
        assert!(moved_ids("").is_empty());
        assert_eq!(repointed_line(&["suite:a.sh".into(), "suite:b.sh".into()], "sp-f").unwrap(), "bead: dep add: registry row(s) suite:a.sh suite:b.sh now name sp-f");
        assert!(repointed_line(&[], "sp-f").is_none());
        let failed = repoint_failed_line("sp-i", "sp-f", "db locked");
        assert!(failed.contains("edge sp-i -> sp-f is in") && failed.contains("did not move") && failed.contains("db locked"), "{failed}");
    }

    #[test_id(t8wjeyk)]
    #[test]
    fn a_blocks_edge_from_an_incident_moves_its_registry_rows_to_the_fix() {
        let dir = testkit::TempDir::new("bead-dep-repoint");
        let _env = configured(dir.path());
        let label = cfg_label("SPIRA_INCIDENT_LABEL");
        let inc: &'static str = Box::leak(format!(r#"[{{"id":"sp-i","issue_type":"task","labels":["{label}"]}}]"#).into_boxed_str());
        repoint_calls();
        TEST_REPOINT.with(|r| r.borrow_mut().1 = Ok(vec!["suite:a.sh".into()]));

        let d = install(vec![("content show sp-i", 0, inc), ("content show sp-f", 0, BEAD)]);
        assert_eq!(cmd_dep_add(&argv(&["sp-i", "sp-f"])), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "add", "sp-i", "sp-f"])]);
        assert_eq!(repoint_calls(), vec![("sp-i".to_string(), "sp-f".to_string())]);

        install(vec![("content show sp-i", 0, inc), ("content show sp-f", 0, BEAD)]);
        assert_eq!(cmd_dep_add(&argv(&["sp-i", "sp-f", "--type", "related"])), 0);
        assert!(repoint_calls().is_empty(), "a related edge moves nothing");

        install(vec![("content show sp-a", 0, BEAD), ("content show sp-f", 0, BEAD)]);
        assert_eq!(cmd_dep_add(&argv(&["sp-a", "sp-f"])), 0);
        assert!(repoint_calls().is_empty(), "a non-incident dependent moves nothing");

        let d = install(vec![("content show sp-i", 1, "gone"), ("content show sp-f", 0, BEAD)]);
        assert_eq!(cmd_dep_add(&argv(&["sp-i", "sp-f"])), 1, "an unreadable dependent is not silently skipped");
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "add", "sp-i", "sp-f"])]);

        TEST_REPOINT.with(|r| r.borrow_mut().1 = Err("db locked".into()));
        let d = install(vec![("content show sp-i", 0, inc), ("content show sp-f", 0, BEAD)]);
        assert_eq!(cmd_dep_add(&argv(&["sp-i", "sp-f"])), 1);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "add", "sp-i", "sp-f"])], "the edge stays");
    }

    #[test_id(t9jvrsn)]
    #[test]
    fn a_filing_with_a_blocker_creates_without_deps_and_wires_the_new_bead_as_the_blocked_one() {
        let dir = testkit::TempDir::new("bead-door-deps");
        let _env = configured(dir.path());
        let d = install(vec![("content create", 0, "sp-new\n")]);
        let args = argv(&["create", "T", "--silent"]);
        assert_eq!(door_create(&mut std::io::sink(), &args, None, None, &["sp-a".to_string()], Some("T"), Some("T"), false, None, Some("r"), None), 0);
        let sent = writes(&d);
        assert_eq!(sent[0].0, argv(&["content", "create", "T", "--silent"]));
        assert!(sent.iter().any(|(a, _)| *a == argv(&["content", "dep", "add", "sp-new", "sp-a", "--type", "blocks"])), "{sent:?}");
    }

    #[test_id(tw49x1k)]
    #[test]
    fn a_create_the_store_cuts_is_sent_again_under_the_same_request_and_the_filing_completes() {
        let dir = testkit::TempDir::new("bead-door-resend");
        let _env = configured(dir.path());
        let d = install(vec![("content create", 0, "sp-new\n")]);
        *d.first.lock().unwrap() = vec![("content create", 2, "cannot tell: deadline")];
        let args = argv(&["create", "T", "--silent", "--body-file", "-"]);
        assert_eq!(door_create(&mut std::io::sink(), &args, Some("the body"), None, &[], Some("T"), Some("T"), false, None, Some("r"), None), 0);
        let w = writes(&d);
        let creates: Vec<_> = w.iter().filter(|(a, _)| a[1] == "create").collect();
        assert_eq!(creates.len(), 2, "{w:?}");
        assert_eq!(creates[0], creates[1], "same argv and body");
        let ids: Vec<_> = d.sent.lock().unwrap().iter().zip(d.request_ids.lock().unwrap().iter()).filter(|((a, _), _)| a[1] == "create").map(|(_, id)| id.clone()).collect();
        assert!(ids[0].is_some() && ids[0] == ids[1], "{ids:?}");
        assert!(w.iter().any(|(a, _)| a[..2] == argv(&["create-bead", "sp-new"])[..]), "{w:?}");
    }

    fn id_after_every_write(answers: Vec<(&'static str, i32, &'static str)>, first: Vec<(&'static str, i32, &'static str)>) -> (i32, Arc<Door>, Vec<String>) {
        let d = install(answers);
        *d.first.lock().unwrap() = first;
        let args = argv(&["create", "T", "--silent"]);
        let rc = door_create(&mut LogOut(d.log.clone()), &args, None, None, &["sp-a".to_string()], Some("T"), Some("T"), false, None, Some("r"), None);
        let log = d.log.lock().unwrap().clone();
        (rc, d, log)
    }

    #[test_id(twyrc7c)]
    #[test]
    fn a_filing_whose_row_write_the_store_cuts_prints_the_id_after_every_write() {
        let dir = testkit::TempDir::new("bead-door-id-last");
        let _env = configured(dir.path());
        let (rc, d, log) = id_after_every_write(vec![("content create", 0, "sp-new\n")], vec![("create-bead", 2, "cannot tell: deadline")]);
        assert_eq!(rc, 0);
        assert_eq!(sent(&d).iter().filter(|a| a[1] == "create").count(), 1);
        let pos = |p: &str| log.iter().rposition(|l| l.starts_with(p));
        assert_eq!(log.iter().filter(|l| l.starts_with("create-bead")).count(), 2, "{log:?}");
        let out = log.len() - 1;
        assert!(log[out].starts_with("out:") && log[out].trim_end().ends_with("sp-new"), "{log:?}");
        assert!(pos("out:") > pos("create-bead") && pos("out:") > pos("content dep add"), "{log:?}");
        if let Some(p) = pos("promote-filed") {
            assert!(pos("out:") > Some(p), "{log:?}");
        }
    }

    #[test_id(tigbm4o)]
    #[test]
    fn a_filing_prints_only_the_id_though_its_edge_write_answers_with_text() {
        let dir = testkit::TempDir::new("bead-door-id-only");
        let _env = configured(dir.path());
        let (rc, _, log) = id_after_every_write(vec![("content create", 0, "sp-new\n"), ("content dep add", 0, "added\n")], vec![]);
        assert_eq!(rc, 0);
        let outs: Vec<_> = log.iter().filter_map(|l| l.strip_prefix("out:")).collect();
        assert_eq!(outs.concat(), "sp-new\n", "{log:?}");
    }

    const REQUEST_AND_ITEM: &str = r#"[{"id":"sp-req","title":"T","issue_type":"task","created_at":"2999-01-01T00:00:00Z"},{"id":"sp-item","title":"T","issue_type":"task","created_at":"2999-01-01T00:00:00Z"}]"#;
    const LISTED_REQUEST: &str = r#"[{"id":"sp-req","title":"T","issue_type":"task","created_at":"2999-01-01T00:00:00Z"}]"#;

    #[test_id(txp1tk8)]
    #[test]
    fn a_draft_is_created_though_the_only_row_of_its_title_is_its_request() {
        let dir = testkit::TempDir::new("bead-door-draft");
        let _env = configured(dir.path());
        let d = install(vec![("content list", 0, LISTED_REQUEST), ("content create", 0, "sp-item\n")]);
        let args = argv(&["create", "T", "--silent"]);
        let mut out = Vec::new();
        assert_eq!(door_create(&mut out, &args, None, None, &[], Some("T"), Some("T"), false, None, Some("r"), Some("sp-req")), 0);
        assert_eq!(sent(&d).iter().filter(|a| a[1] == "create").count(), 1);
        assert_eq!(String::from_utf8(out).unwrap().trim(), "sp-item");
    }

    #[test_id(tb2xc2e)]
    #[test]
    fn a_retry_of_a_draft_already_filed_is_answered_with_the_item_not_the_request() {
        let dir = testkit::TempDir::new("bead-door-draft-retry");
        let _env = configured(dir.path());
        let d = install(vec![("content list", 0, REQUEST_AND_ITEM), ("content create", 0, "sp-dup\n")]);
        let args = argv(&["create", "T", "--silent"]);
        let mut out = Vec::new();
        assert_eq!(door_create(&mut out, &args, None, None, &[], Some("T"), Some("T"), false, None, Some("r"), Some("sp-req")), 0);
        assert!(sent(&d).iter().all(|a| a[1] != "create"));
        assert_eq!(String::from_utf8(out).unwrap().trim(), "sp-item");
    }

    const LISTED_NOW: &str = r#"[{"id":"sp-held","title":"T","issue_type":"task","created_at":"2999-01-01T00:00:00Z"}]"#;

    #[test_id(tn7w361)]
    #[test]
    fn a_retry_of_a_title_the_store_holds_sends_no_create_and_still_completes_the_bead() {
        let dir = testkit::TempDir::new("bead-door-retry");
        let _env = configured(dir.path());
        let d = install(vec![("content list", 0, LISTED_NOW), ("content create", 0, "sp-dup\n")]);
        let args = argv(&["create", "T", "--silent"]);
        assert_eq!(door_create(&mut std::io::sink(), &args, None, None, &["sp-a".to_string()], Some("T"), Some("T"), false, None, Some("r"), None), 0);
        let w = sent(&d);
        assert!(w.iter().all(|a| a[1] != "create"), "{w:?}");
        assert_eq!(w, [argv(&["create-bead", "sp-held", "--repo", "r"]), argv(&["content", "dep", "add", "sp-held", "sp-a", "--type", "blocks"])]);
    }

    #[test_id(t6dnoed)]
    #[test]
    fn an_edge_already_in_the_content_store_is_mirrored_to_the_lifecycle_store_not_failed() {
        let dir = testkit::TempDir::new("bead-door-present");
        let _env = configured(dir.path());
        let d = install(vec![("content dep add", 1, "already exists\n"), ("content dep list", 0, r#"[{"depends_on_id":"sp-a"}]"#)]);
        assert_eq!(blocks_edge_present_ok("sp-new", "sp-a"), 0);
        assert!(d.sent.lock().unwrap().iter().any(|(a, _)| a[0] == "dep-add" && a[1..] == argv(&["sp-new", "sp-a", "--type", "blocks"])[..]));
        install(vec![("content dep add", 1, "refused\n"), ("content dep list", 0, "[]")]);
        assert_eq!(blocks_edge_present_ok("sp-new", "sp-a"), 1, "an edge the store does not hold stays a failure");
    }

    #[test_id(tre495q)]
    #[test]
    fn an_unfinished_filing_names_the_express_mark_and_the_incident_edge_with_their_repairs() {
        let text = unrecovered(None, true, Some("sp-inc"));
        assert!(text.contains("bead.sh amend <id> --express") && text.contains("bd dep add sp-inc <id>"), "{text}");
        assert_eq!(unrecovered(Some("sp-x"), false, None), "");
    }

    #[test_id(t9thqof)]
    #[test]
    fn a_refusal_the_door_gives_is_the_exit_code_and_stderr_the_caller_sees() {
        install(vec![("content label add", 1, "bead: refusing\n")]);
        assert_eq!(put(&["label", "add", "sp-a", "x"], None), 1);
    }

    const TIP: &str = "1111111111111111111111111111111111111111";

    fn script_git(answers: Vec<(&'static str, i32, &'static str)>) {
        TEST_GIT.with(|g| *g.borrow_mut() = (Vec::new(), answers));
        TEST_SAID.with(|s| s.borrow_mut().clear());
    }

    fn git_calls() -> Vec<Vec<String>> {
        TEST_GIT.with(|g| g.borrow().0.clone())
    }

    fn said() -> String {
        TEST_SAID.with(|s| s.borrow().join("\n"))
    }

    fn file_submitted(answers: Vec<(&'static str, i32, &'static str)>) -> (i32, Arc<Door>, String) {
        let d = install(answers);
        let args = argv(&["create", "T", "--silent"]);
        let mut out = Vec::new();
        let rc = door_create(&mut out, &args, None, None, &[], Some("T"), Some("T"), false, Some((TIP, "co")), Some("r"), None);
        (rc, d, String::from_utf8(out).unwrap())
    }

    #[test_id(tiilc6g)]
    #[test]
    fn a_submitted_filing_creates_its_branch_at_the_tip_after_the_row_and_then_prints_the_id() {
        let dir = testkit::TempDir::new("bead-door-branch");
        let _env = configured(dir.path());
        script_git(vec![]);
        let (rc, d, out) = file_submitted(vec![("content create", 0, "sp-new\n")]);
        assert_eq!((rc, out.as_str()), (0, "sp-new\n"));
        assert!(sent(&d).iter().any(|a| a[..2] == argv(&["create-bead", "sp-new"])[..]));
        assert_eq!(git_calls(), [argv(&["-C", "co", "update-ref", "refs/heads/spira/sp-new", TIP, ""])]);
    }

    #[test_id(tqcevst)]
    #[test]
    fn a_submitted_filing_whose_branch_cannot_be_created_prints_no_id_and_the_repair() {
        let dir = testkit::TempDir::new("bead-door-branch-fail");
        let _env = configured(dir.path());
        script_git(vec![("update-ref", 128, "fatal: locked"), ("rev-parse", 1, "")]);
        let (rc, _, out) = file_submitted(vec![("content create", 0, "sp-new\n")]);
        assert_eq!((rc, out.as_str()), (1, ""));
        let said = said();
        assert!(said.contains("fatal: locked") && said.contains("no id is printed") && said.ends_with(&format!("Create it: git -C co update-ref refs/heads/spira/sp-new {TIP} \"\"")), "{said}");
    }

    #[test_id(tq3udzm)]
    #[test]
    fn a_submitted_filing_whose_row_is_not_created_makes_no_git_call() {
        let dir = testkit::TempDir::new("bead-door-branch-norow");
        let _env = configured(dir.path());
        script_git(vec![]);
        let (rc, _, out) = file_submitted(vec![("content create", 0, "sp-new\n"), ("create-bead", 1, "refused")]);
        assert_eq!((rc, out.as_str()), (1, ""));
        assert!(git_calls().is_empty(), "{:?}", git_calls());
    }

    fn file_through_the_command(dir: &std::path::Path, extra: &[&str]) -> i32 {
        let map = dir.join("fixture.repos");
        std::fs::write(&map, format!("home|{}|push\n", dir.join("home").display())).unwrap();
        let toml = spira_config::process::fixture_toml(dir, &[("SPIRA_REPO_MAP", &map.to_string_lossy()), ("SPIRA_HOME_REPO", "home")]);
        let spira_home = testkit::manifest_dir().join("../spira").to_string_lossy().into_owned();
        let _env = testkit::env(&[("SPIRA_TOML", Some(&toml.to_string_lossy())), ("SPIRA_HOME", Some(&spira_home)), ("SPIRA_RELEASE", None), ("SPIRA_REPO_MAP", None), ("SPIRA_HOME_REPO", None), ("SPIRA_REPO", None), ("SPIRA_REPO_DERIVED", None), ("SPIRA_CONCIERGE", None), ("SPIRA_RUNES_DIR", Some(&format!("{spira_home}/runes")))]);
        let body = dir.join("body.md");
        std::fs::write(&body, "## Build\nthe thing").unwrap();
        let mut args = argv(&["T", "--for", "builder", "--repo", "home", "--body-file", &body.to_string_lossy()]);
        args.extend(argv(extra));
        spira_config::process::with_fresh(|| cmd_file(&spira_home, &args)).expect("the fixture resolves")
    }

    #[test_id(t8xk4h4)]
    #[test]
    fn a_submitted_filing_whose_tip_is_no_commit_in_the_checkout_files_nothing() {
        let dir = testkit::TempDir::new("bead-file-bad-tip");
        let d = install(vec![("content create", 0, "sp-new\n")]);
        script_git(vec![("cat-file", 128, "fatal")]);
        assert_eq!(file_through_the_command(dir.path(), &["--submitted", "--tip", TIP]), 2);
        assert!(sent(&d).is_empty(), "{:?}", sent(&d));
        let root = dir.path().join("home").display().to_string();
        assert_eq!(said(), format!("bead: --submitted --tip {TIP}: {TIP} is not a commit in {root} (repo home); nothing filed"));
    }

    #[test_id(tl5q2gf)]
    #[test]
    fn an_unsubmitted_filing_makes_no_git_call() {
        let dir = testkit::TempDir::new("bead-file-unsubmitted");
        let d = install(vec![("content create", 0, "sp-new\n")]);
        script_git(vec![]);
        assert_eq!(file_through_the_command(dir.path(), &[]), 0);
        assert!(sent(&d).iter().any(|a| a[1] == "create"), "{:?}", sent(&d));
        assert!(git_calls().is_empty(), "{:?}", git_calls());
    }

    #[test_id(t7bwbll)]
    #[test]
    fn dep_convert_removes_the_blocks_edge_then_adds_the_parent_edge_and_leaves_the_edge_it_did_not_add() {
        let epic = ("content show sp-e", 0, r#"[{"id":"sp-e","issue_type":"epic","labels":[]}]"#);
        let d = install(vec![epic]);
        assert_eq!(cmd_dep_convert(&argv(&["sp-c", "sp-e"])), 0);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "remove", "sp-c", "sp-e"]), argv(&["content", "dep", "add", "sp-c", "sp-e", "--type", "parent-child"])]);

        let d = install(vec![epic, ("content dep add", 3, "refused\n")]);
        assert_eq!(cmd_dep_convert(&argv(&["sp-c", "sp-e"])), 3);
        assert_eq!(sent(&d).len(), 2, "the removal stood and the add was tried once");
        let left = convert_left_edge("sp-c", "sp-e", 3);
        assert!(left.contains("edge is now absent") && left.contains("bead.sh dep add sp-c sp-e --type parent-child"), "{left}");

        let d = install(vec![epic, ("content dep remove", 3, "refused\n")]);
        assert_eq!(cmd_dep_convert(&argv(&["sp-c", "sp-e"])), 3);
        assert_eq!(sent(&d), vec![argv(&["content", "dep", "remove", "sp-c", "sp-e"])], "a refused removal tries no add");
    }
}
