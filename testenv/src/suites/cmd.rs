//! The subcommands (DESIGN-suites.md §2). Each returns the process exit code and writes only
//! through the `World` ports and its own state directory.

use std::collections::BTreeSet;
use std::fs;

use super::model::{self, LastResult, Runs, Unreadable};
use super::ports::{FlakeFiling, World};

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

// ------------------------------------------------------------------------------ list

pub fn list(w: &World) -> i32 {
    let g = gated(w);
    let now = w.clock.now() as i64;
    w.out(format!(
        "{:<26} {:<7} {:<9} {:<8} {}",
        "SUITE", "RUNS", "LAST", "AGE", "COVERS"
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
            "{:<26} {:<7} {:<9} {:<8} {}",
            s,
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
    for s in model::population(&w.s.suite_dir) {
        w.out(&s);
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
        if oldest.as_ref().is_none_or(|(at, _)| r.at < *at) {
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
