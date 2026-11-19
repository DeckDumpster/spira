//! The subcommands (DESIGN-suites.md §2). Each returns the process exit code and writes only
//! through the `World` ports and its own state directory.

use std::collections::BTreeSet;

use super::model::{self, LastResult, Runs, Unreadable};
use super::ports::World;

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
