//! The subcommands (DESIGN-suites.md §2). Each returns the process exit code and writes only
//! through the `World` ports and its own state directory.

use std::collections::BTreeSet;
use std::fs;

use super::model::{self, Entry, LastResult, Runs, Unreadable};
use super::ports::{FlakeFiling, World};
use crate::suite::{SuiteState, SuiteStates};

pub const OK: i32 = 0;
pub const FAIL: i32 = 1;
pub const USAGE: i32 = 2;

fn gated(w: &World) -> Result<BTreeSet<String>, Unreadable> {
    model::gate_list(&w.s.gate_list)
}

fn last_result(w: &World, suite: &str) -> Option<LastResult> {
    model::read_text(&w.s.state_file(suite, "result")).and_then(|t| LastResult::parse(&t))
}

fn suite_text(w: &World, suite: &str) -> String {
    model::read_text(&w.s.suite_path(suite)).unwrap_or_default()
}

fn lifecycle(w: &World) -> SuiteStates {
    SuiteStates::parse(&model::read_text(&w.s.lifecycle_file()).unwrap_or_default())
}

// ------------------------------------------------------------------------------ list

pub fn list(w: &World) -> i32 {
    let g = gated(w);
    let states = lifecycle(w);
    let now = w.clock.now() as i64;
    w.out(format!(
        "{:<26} {:<12} {:<7} {:<9} {:<8} {}",
        "SUITE", "STATE", "RUNS", "LAST", "AGE", "COVERS"
    ));
    for s in model::population(&w.s.suite_dir) {
        let runs = Runs::of(&s, &g);
        let (mut last, mut age) = match last_result(w, &s) {
            Some(r) => (r.status, format!("{}m", (now - r.at as i64) / 60)),
            None => ("-".to_string(), "-".to_string()),
        };
        if runs == Runs::Gate {
            last = "-".into();
            age = "-".into();
        }
        w.out(format!(
            "{:<26} {:<12} {:<7} {:<9} {:<8} {}",
            s,
            states.state_of(&s).as_str(),
            runs.as_str(),
            last,
            age,
            model::covers_of(&suite_text(w, &s))
        ));
    }
    if g.is_err() {
        w.out("");
        w.out(format!(
            "{} is unreadable — which suites the gate runs is unknown",
            w.s.gate_list.display()
        ));
    }
    OK
}

// ----------------------------------------------------------------------------- names

pub fn names(w: &World) -> i32 {
    let Ok(g) = gated(w) else {
        w.log(&format!(
            "suites: {} is unreadable — refusing to guess which suites the gate runs",
            w.s.gate_list.display()
        ));
        return FAIL;
    };
    for s in model::population(&w.s.suite_dir) {
        if !g.contains(&s) {
            w.out(&s);
        }
    }
    OK
}

// ---------------------------------------------------------------------------- corpus

pub fn corpus(w: &World) -> i32 {
    let states = lifecycle(w);
    for s in model::population(&w.s.suite_dir) {
        if states.state_of(&s) != SuiteState::Disabled {
            w.out(&s);
        }
    }
    OK
}

// ---------------------------------------------------------------------------- status

fn row(w: &World, label: &str, value: &str) {
    w.out(format!("  {label:<36}{value}"));
}

pub fn status(w: &World) -> i32 {
    let now = w.clock.now();
    let Ok(g) = gated(w) else {
        w.out(format!(
            "suites          ?   {} is unreadable — the gated set is unknown",
            w.s.gate_list.display()
        ));
        return OK;
    };
    let (mut total, mut gate_n, mut timed_n, mut never, mut stale) = (0, 0, 0, 0, 0);
    let (mut red, mut skip, mut fault) = (0, 0, 0);
    let mut oldest: Option<(u64, String)> = None;
    for s in model::population(&w.s.suite_dir) {
        total += 1;
        if g.contains(&s) {
            gate_n += 1;
            continue;
        }
        timed_n += 1;
        let Some(r) = last_result(w, &s) else {
            never += 1;
            continue;
        };
        if r.is_red() {
            red += 1;
        } else if r.is_skip() {
            skip += 1;
        } else if r.is_fault() {
            fault += 1;
        }
        if now.saturating_sub(r.at) > w.s.stale {
            stale += 1;
        }
        if oldest.as_ref().map_or(true, |(at, _)| r.at < *at) {
            oldest = Some((r.at, s.clone()));
        }
    }
    let q = |flag: &str| {
        w.host
            .count(flag)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "?".into())
    };
    let (undeclared, copying) = (q("--count-undeclared"), q("--count-copying"));
    row(w, "suites in the tree", &format!("{total}   ({gate_n} gated, {timed_n} timed)"));
    row(w, "host suites without # host-reason:", &undeclared);
    row(w, "suites still copying/stubbing (wave 2)", &copying);
    row(w, "timed suites with no result yet", &never.to_string());
    row(w, &format!("timed results older than {}h", w.s.stale / 3600), &stale.to_string());
    row(w, "timed suites red at last run", &red.to_string());
    row(w, "timed suites setup/fixture fault at last run", &fault.to_string());
    row(w, "timed suites skipped at last run", &skip.to_string());
    match oldest {
        Some((at, s)) => w.out(format!(
            "  {:<36}{}m   {s}",
            "oldest timed result",
            (now as i64 - at as i64) / 60
        )),
        None => row(w, "oldest timed result", "?   (nothing has run)"),
    }
    OK
}

// --------------------------------------------------------------------- observe-flake

fn usable_suite(w: &World, label: &str, suite: &str) -> Result<(), i32> {
    if !model::valid_suite_name(suite) || !w.s.suite_path(suite).is_file() {
        w.err(format!("suites {label}: no such suite: {suite}"));
        return Err(USAGE);
    }
    Ok(())
}

pub fn observe_flake(w: &World, suite: &str, run_id: &str) -> i32 {
    if suite.is_empty() {
        w.err("suites observe-flake: suite name required");
        return USAGE;
    }
    if run_id.is_empty() {
        w.err("suites observe-flake: run id required");
        return USAGE;
    }
    if usable_suite(w, "observe-flake", suite).is_err() {
        return USAGE;
    }
    // A run id is one token on one line of the observation file.
    let run_id: String = run_id.split_whitespace().collect::<Vec<_>>().join("_");
    let _ = fs::create_dir_all(&w.s.state);
    let f = w.s.state_file(suite, "flakeobs");
    let obs = model::parse_flakeobs(&model::read_text(&f).unwrap_or_default());
    let now = w.clock.now();
    let mut all = obs.clone();
    if !model::flakeobs_has(&obs, &run_id) {
        use std::io::Write;
        match fs::OpenOptions::new().create(true).append(true).open(&f) {
            Ok(mut h) => {
                let _ = writeln!(h, "{now} {run_id}");
            }
            Err(e) => w.log(&format!("suites: cannot record a flake observation in {}: {e}", f.display())),
        }
        all.push(model::FlakeObs { at: Some(now), run_id: run_id.clone() });
    }
    let count = model::flakeobs_in_window(&all, now, w.s.flake_window);
    let threshold = w.s.flake_at;
    w.out(format!(
        "observe-flake: {suite}: {count} observation(s) in window (threshold {threshold})"
    ));
    if count < threshold {
        return OK;
    }
    match file_flake(w, suite, count) {
        Some(id) => w.out(format!("observe-flake: {suite} reported (bead: {id})")),
        None => w.out(format!(
            "observe-flake: {suite} crossed threshold but the finding could not be filed"
        )),
    }
    OK
}

/// The payload suites.sh wrote on incident.sh's stdin (DESIGN-suites.md §3.5).
pub fn flake_payload(suite: &str, count: u64, window: u64) -> String {
    [
        format!("{suite} has {count} flake observation(s) within the {window}s window. It is filed and nothing is"),
        "quarantined: the suite keeps running in every batch, so a repeat failure still reaches the".into(),
        "gate instead of being hidden.".into(),
        String::new(),
        format!("  suite            {suite}"),
        format!("  observations     {count} in {window}s"),
        format!("  reproduce        bash spira/{suite}"),
        String::new(),
        format!("The dedupe ref is flake:{suite} — a later observation bumps recurrence on this bead rather than"),
        "filing another.".into(),
        String::new(),
    ]
    .join("\n")
}

/// The id incident.sh printed: its last line, whitespace removed, `[A-Za-z0-9-]+` without
/// a leading or trailing `-`.
pub fn intake_id(stdout: &str) -> Option<String> {
    let last = stdout.trim_end_matches('\n').rsplit('\n').next().unwrap_or("");
    let id: String = last.chars().filter(|c| !c.is_whitespace()).collect();
    let ok = !id.is_empty()
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-');
    ok.then_some(id)
}

fn file_flake(w: &World, suite: &str, count: u64) -> Option<String> {
    let conf = match w.lib.conf() {
        Ok(c) => c,
        Err(e) => {
            w.log(&format!("suites: cannot resolve the home repository for {suite}'s flake finding: {e}"));
            return None;
        }
    };
    let labels = match conf.scope_label.as_deref() {
        Some(l) if !l.is_empty() => format!("{l},plan"),
        _ => "plan".to_string(),
    };
    let filing = FlakeFiling {
        suite: suite.to_string(),
        title: format!("why does {suite} fail intermittently"),
        priority: model::priority_of(&suite_text(w, suite), w.s.priority),
        labels,
        repo: conf.home_repo,
        reference: format!("flake:{suite}"),
        path: w.s.suite_path(suite),
        db: conf.db,
        payload: flake_payload(suite, count, w.s.flake_window),
    };
    let out = match w.intake.file(&filing) {
        Ok(o) => o,
        Err(e) => {
            w.log(&format!("suites: {e}"));
            return None;
        }
    };
    let id = intake_id(&out);
    if id.is_none() {
        w.log(&format!("suites: the intake returned no id for {suite} flake finding"));
    }
    id
}

// ------------------------------------------------------------------------ transitions

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Quarantine { bead: String, reason: String, until: Option<String> },
    Disable { reason: String },
    Activate,
}

impl Transition {
    pub fn state(&self) -> SuiteState {
        match self {
            Transition::Quarantine { .. } => SuiteState::Quarantined,
            Transition::Disable { .. } => SuiteState::Disabled,
            Transition::Activate => SuiteState::Active,
        }
    }
    fn bead(&self) -> &str {
        match self {
            Transition::Quarantine { bead, .. } => bead,
            _ => "",
        }
    }
    fn reason(&self) -> &str {
        match self {
            Transition::Quarantine { reason, .. } | Transition::Disable { reason } => reason,
            Transition::Activate => "",
        }
    }
}

/// The writer's claim on the change bead: long enough to outlast the gate `queue submit`
/// runs, short enough that a writer that dies mid-transition is reaped (sentinel's
/// stale-lease sweep) rather than holding the bead forever.
pub const CHANGE_LEASE_SECS: u64 = 3600;
/// The holder (and event actor) the change bead's claim names.
pub const CHANGE_HOLDER: &str = "suites";

/// A bead id the edit may land as: it becomes `spira/<id>`, so it must be a plain ref
/// component.
fn valid_bead_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with(['-', '.'])
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Change the suite's row in the home repository's lifecycle file on `base` (default: its
/// landing ref, else HEAD) as a change bead — `change` when the caller hands one, else a bead
/// this filed through `bead.sh file` — claim the bead's lifecycle row, put the commit on
/// `spira/<bead>` and submit it to the queue, which certifies it on the lifecycle machine like
/// any work bead (sp-lck63: no bead-less `spira-suite-state/*` branch, no certification the
/// queue keeps of its own). Touches no checkout (DESIGN-suites.md §6 D2). Ok(branch) or
/// Err(exit code); every refusal is printed.
pub fn transition(w: &World, t: &Transition, suite: &str, base: Option<&str>, change: Option<&str>) -> Result<String, i32> {
    let label = t.state().as_str();
    if w.s.aeon {
        w.err(format!("suites {label}: aeons may not write suite-state transitions; submit a branch from an operator or Ops session"));
        return Err(FAIL);
    }
    if suite.is_empty() {
        w.err(format!("suites {label}: suite name required"));
        return Err(USAGE);
    }
    if !model::valid_suite_name(suite) {
        w.err(format!("suites {label}: no such suite: {suite}"));
        return Err(USAGE);
    }
    if let Transition::Quarantine { until: Some(u), .. } = t {
        match model::parse_iso_utc(u) {
            None => {
                w.err(format!("suites {label}: --until wants UTC like 2026-10-10T00:00:00Z, not {u}"));
                return Err(USAGE);
            }
            Some(at) if at <= w.clock.now() => {
                w.err(format!("suites {label}: --until {u} is already past; it would expire at once"));
                return Err(USAGE);
            }
            Some(_) => {}
        }
    }
    if matches!(t, Transition::Quarantine { .. }) && t.bead().is_empty() {
        w.err("suites quarantine: bead id required");
        return Err(USAGE);
    }
    if !matches!(t, Transition::Activate) && t.reason().is_empty() {
        w.err(format!("suites {label}: reason required"));
        return Err(USAGE);
    }
    for (name, v, last) in [("suite name", suite, false), ("bead id", t.bead(), false), ("reason", t.reason(), true)] {
        if let Some(p) = model::field_problem(v, last) {
            w.err(format!("suites {label}: the {name} may not contain {p} (spira/suite-state cannot carry it)"));
            return Err(USAGE);
        }
    }
    if let Some(c) = change {
        if !valid_bead_id(c) {
            w.err(format!("suites {label}: --change-bead {c:?} is not a bead id (it names the branch spira/<id>)"));
            return Err(USAGE);
        }
    }
    let conf = match w.lib.conf() {
        Ok(c) => c,
        Err(e) => {
            w.err(format!("suites {label}: cannot resolve the home repository: {e}"));
            return Err(FAIL);
        }
    };
    let Some(repo) = conf.repo_path.clone() else {
        w.err(format!("suites {label}: cannot resolve the home repository ({})", conf.home_repo));
        return Err(FAIL);
    };
    let base = base
        .map(str::to_string)
        .or(conf.landref.clone())
        .unwrap_or_else(|| "HEAD".into());
    let Some(parent) = w.git.commit_of(&repo, &base) else {
        w.err(format!("suites {label}: cannot resolve base {base} in {}", repo.display()));
        return Err(FAIL);
    };
    if !w.git.tree_has(&repo, &parent, &format!("spira/{suite}")) {
        w.err(format!("suites {label}: no such suite: {suite}"));
        return Err(USAGE);
    }
    let path = w.s.suite_state_file.as_str();
    let current = w.git.show(&repo, &parent, path).unwrap_or_default();
    let now = w.clock.now();
    let entry = Entry {
        state: t.state(),
        since: crate::util::iso_utc(now),
        bead: t.bead().to_string(),
        reason: t.reason().to_string(),
        until: match t {
            Transition::Quarantine { until, .. } => until.clone(),
            _ => None,
        },
    };
    let new = model::rewrite_state(&current, suite, Some(&entry));

    // The change bead: handed, else filed. Filed --submitted, so no persona claims it out
    // from under the writer before the lifecycle claim below.
    let id = match change {
        Some(c) => c.to_string(),
        None => match w.change.file(&format!("suite-state: {suite} -> {label}"), &conf.home_repo, &conf.db) {
            Ok(id) if valid_bead_id(&id) => id,
            Ok(id) => {
                w.err(format!("suites {label}: bead.sh file returned {id:?}, not a bead id"));
                return Err(FAIL);
            }
            Err(e) => {
                w.err(format!("suites {label}: cannot file the change bead: {e}"));
                return Err(FAIL);
            }
        },
    };
    let branch = format!("spira/{id}");
    if w.git.commit_of(&repo, &format!("refs/heads/{branch}")).is_some() {
        w.err(format!("suites {label}: {branch} already exists — the change bead {id} already carries a branch"));
        return Err(FAIL);
    }
    let msg = format!("{id}: suite-state: {suite} -> {label}\n");
    let commit = match w.git.commit_file(&repo, &parent, path, &new, &msg, (&w.s.git_name, &w.s.git_email)) {
        Ok(c) => c,
        Err(e) => {
            w.err(format!("suites {label}: commit failed ({e})"));
            return Err(FAIL);
        }
    };
    if let Err(e) = w.change.claim(&id, CHANGE_HOLDER, now + CHANGE_LEASE_SECS) {
        w.err(format!("suites {label}: cannot claim the change bead {id} on the lifecycle machine: {e}"));
        return Err(FAIL);
    }
    if !w.git.create_branch(&repo, &branch, &commit) {
        w.err(format!("suites {label}: cannot create branch {branch}"));
        return Err(FAIL);
    }
    if !w.queue.submit(&branch) {
        w.err(format!("suites {label}: queue submit failed for {branch} — branch exists but is not certified"));
        return Err(FAIL);
    }
    Ok(branch)
}

pub fn run_transition(w: &World, t: &Transition, suite: &str, base: Option<&str>, change: Option<&str>) -> i32 {
    match transition(w, t, suite, base, change) {
        Ok(b) => {
            w.out(&b);
            OK
        }
        Err(rc) => rc,
    }
}

// ------------------------------------------------------------------------------ lint

/// `spira/suite-state.sh`'s `suite_state_lint` and `suite_state_parse`, merged into one pass
/// (DESIGN-suites.md §6b, sp-9gd4e): diagnostics to stderr (one per violation, `errors > 0`
/// fails), one tab-separated row per known-state entry to stdout — `suite_state_parse`'s
/// exact shape (`suite\tstate\tsince\tbead\treason`), so `suite-state-fence.sh` reads this
/// command's stdout exactly as it read the bash function's. An absent lifecycle file is not
/// a fault: it means every suite is active, so there is nothing to lint (mirrors the fence's
/// own pre-check, which never called the bash lint on a missing file either).
pub fn lint(w: &World) -> i32 {
    let Some(text) = model::read_text(&w.s.lifecycle_file()) else {
        return OK;
    };
    let mut errors = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let stripped = raw.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let parts: Vec<&str> = stripped.splitn(5, '|').map(str::trim).collect();
        if parts.len() < 5 {
            w.err(format!(
                "suite-state:{lineno}: not parseable (expected suite|state|since|bead|reason): {stripped}"
            ));
            errors += 1;
            continue;
        }
        let (suite, state, since, bead, reason) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
        if !w.s.suite_path(suite).is_file() {
            w.err(format!("suite-state:{lineno}: suite does not exist: {suite}"));
            errors += 1;
        }
        let known = matches!(state, "active" | "quarantined" | "disabled");
        if !known {
            w.err(format!(
                "suite-state:{lineno}: unknown state {state} (valid: active quarantined disabled)"
            ));
            errors += 1;
        }
        if reason.is_empty() {
            w.err(format!("suite-state:{lineno}: missing reason for {suite}"));
            errors += 1;
        }
        if state == "quarantined" && bead.is_empty() {
            w.err(format!("suite-state:{lineno}: quarantined suite {suite} has no bead id"));
            errors += 1;
        }
        if known && !suite.is_empty() {
            w.out(format!("{suite}\t{state}\t{since}\t{bead}\t{reason}"));
        }
    }
    if errors > 0 {
        FAIL
    } else {
        OK
    }
}

// --------------------------------------------------------------------------- hygiene

pub fn hygiene(w: &World) -> i32 {
    let states = lifecycle(w);
    let now = w.clock.now();
    let mut mailed = 0;
    let from = "Suite hygiene <hygiene@spira>";
    for r in states.rows().filter(|r| r.state == SuiteState::Quarantined) {
        let s = &r.suite;
        let mailed_f = w.s.state_file(s, "maxage-mailed");
        // -- max age: mail the operator once per quarantine period --
        let Some(since) = model::parse_iso_utc(&r.since) else {
            continue;
        };
        let age = now.saturating_sub(since);
        if age >= w.s.max_age && !mailed_f.exists() {
            let days = w.s.max_age / 86_400;
            let body = format!("quarantine for {s} exceeds {days} days.\n\nSince: {}\n", r.since);
            let bead = (!r.bead.is_empty()).then_some(r.bead.as_str());
            if w.mail.send_operator(from, &format!("{s} quarantine exceeds {days}d"), bead, &body) {
                let _ = fs::create_dir_all(&w.s.state);
                if fs::write(&mailed_f, "").is_ok() {
                    w.out(format!("hygiene: mailed operator about {s} (age {age}s)"));
                    mailed += 1;
                }
            }
        }
    }
    w.out(format!("hygiene: {mailed} max-age mailed"));
    OK
}
