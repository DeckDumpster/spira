//! The verbs: named arguments, exit 0 ok, 1 refused, 2 usage, 3 the registry cannot be opened.

use std::collections::BTreeMap;

use rusqlite::Connection;
use runtime::ports::Rng;
use serde_json::{json, Value};
use spira_otel::vlogs::{Logs, LogsError};

use crate::store::{self, Row, Set};

pub const PATH_KEY: &str = "SPIRA_TEST_REGISTRY_PATH";
pub const HOLD_KEY: &str = "SPIRA_TEST_REGISTRY_HOLD_SECS";
pub const LOGS_KEY: &str = "SPIRA_VICTORIALOGS_URL";
const DEFAULT_WINDOW_HOURS: u64 = 168;
/// A week of every test's records takes the store longer to answer than a polling reader waits.
// batch-job: a report run by hand over every test record in the window; it holds no lock or lease
const EVENTS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

type Cfg<'a> = &'a dyn Fn(&str) -> Result<String, String>;
type Opener<'a> = &'a dyn Fn(&str) -> Result<Connection, String>;

struct Args {
    verb: String,
    json: bool,
    named: BTreeMap<String, String>,
}

const VERBS: &[(&str, &[&str], &[&str])] = &[
    ("disable", &["test", "bead", "reason", "actor"], &["until"]),
    (
        "quarantine",
        &["test", "bead", "reason", "actor"],
        &["until"],
    ),
    ("activate", &["test", "reason", "actor"], &[]),
    ("show", &["test"], &[]),
    ("list", &[], &["at"]),
    ("history", &["test"], &[]),
    ("deny-list", &["at"], &[]),
    ("clear-landed", &["bead", "actor"], &[]),
    ("repoint", &["from", "to", "actor"], &[]),
    ("backup", &["out"], &[]),
    ("mint", &[], &[]),
    ("stamp", &["path"], &[]),
    ("flips", &[], &["hours"]),
    ("rates", &[], &["hours", "test"]),
];

const REPEATABLE: &[(&str, &str)] = &[("clear-landed", "bead"), ("stamp", "path")];

fn parse(args: &[String]) -> Result<Args, String> {
    let (verb, rest) = args
        .split_first()
        .ok_or("usage: test-registry <verb> --name value … [--json]")?;
    let (_, required, optional) = VERBS
        .iter()
        .find(|(v, _, _)| v == verb)
        .ok_or_else(|| format!("unknown verb {verb:?}"))?;
    let mut named = BTreeMap::new();
    let mut json = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--json" {
            json = true;
            continue;
        }
        let name = a
            .strip_prefix("--")
            .filter(|n| required.contains(n) || optional.contains(n))
            .ok_or_else(|| {
                let hint = if verb == "stamp" { " (named arguments only: stamp --path <file> [--path <file>…])" } else { "" };
                format!("{verb}: unexpected argument {a:?}{hint}")
            })?;
        let value = it
            .next()
            .ok_or_else(|| format!("{verb}: --{name} needs a value"))?;
        if REPEATABLE.contains(&(verb.as_str(), name)) {
            let joined = named.entry(name.to_string()).or_insert_with(String::new);
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(value);
        } else if named.insert(name.to_string(), value.clone()).is_some() {
            return Err(format!("{verb}: --{name} given twice"));
        }
    }
    if let Some(missing) = required.iter().find(|r| !named.contains_key(**r)) {
        return Err(format!("{verb}: --{missing} is required"));
    }
    Ok(Args {
        verb: verb.clone(),
        json,
        named,
    })
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC) or plain epoch seconds.
pub fn parse_time(s: &str) -> Result<i64, String> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(n);
    }
    let bad = || format!("{s:?} is neither epoch seconds nor RFC3339 UTC (YYYY-MM-DDTHH:MM:SSZ)");
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return Err(bad());
    }
    let n = |r: std::ops::Range<usize>| {
        s.get(r)
            .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|t| t.parse::<i64>().ok())
            .ok_or_else(bad)
    };
    let (y, mo, d, h, mi, se) = (
        n(0..4)?,
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return Err(bad());
    }
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400 + h * 3600 + mi * 60 + se)
}

fn row_json(r: &Row, at: Option<i64>) -> Value {
    let mut v = json!({"test": r.test, "state": r.state, "since": r.since, "until": r.until, "bead": r.bead, "reason": r.reason, "actor": r.actor});
    if let Some(at) = at {
        v["expired"] = json!(r.until <= at);
    }
    v
}

fn row_text(r: &Row, at: Option<i64>) -> String {
    let mark = if at.is_some_and(|at| r.until <= at) {
        " expired"
    } else {
        ""
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}{mark}",
        r.state, r.test, r.bead, r.since, r.until, r.actor, r.reason
    )
}

fn out(code: i32, json_mode: bool, text: String, value: Value) -> Outcome {
    Outcome {
        code,
        stdout: if json_mode { value.to_string() } else { text },
        stderr: String::new(),
    }
}

fn fail(code: i32, json_mode: bool, msg: String) -> Outcome {
    if json_mode {
        Outcome {
            code,
            stdout: json!({"ok": false, "exit": code, "error": msg}).to_string(),
            stderr: String::new(),
        }
    } else {
        Outcome {
            code,
            stdout: String::new(),
            stderr: msg,
        }
    }
}

pub use spira_config::git_refs::REGISTRY_FILE;

/// `cfg` with an empty registry path read as the registry file in the run directory.
pub fn with_run_default(cfg: impl Fn(&str) -> Result<String, String>, run_dir: impl Fn() -> Result<String, String>) -> impl Fn(&str) -> Result<String, String> {
    move |k| match cfg(k) {
        Ok(p) if k == PATH_KEY && p.trim().is_empty() => run_dir().map(|d| format!("{}/{REGISTRY_FILE}", d.trim_end_matches('/'))),
        other => other,
    }
}

pub const INTENT: &str = "A test's id is stable with the test's intent, not its name or its code. Rename or move a test and keep its id. A test rewritten to check something else is a new test: delete the attribute and run `test-registry stamp`.";

pub const HELP: &str = "usage: test-registry <verb> --name value … [--json]
  disable|quarantine --test T --bead B --reason R --actor A [--until TIME]   (T: test:<id>, the last segment of the failing line, or suite:<file>)
  activate --test T --reason R --actor A
  show --test T | history --test T | list [--at TIME] | deny-list --at TIME
  clear-landed --bead B [--bead B…] --actor A | repoint --from F --to T --actor A | backup --out FILE
  flips [--hours N]               tests with a pass and a fail/timeout on one input hash (default 168 hours)
  rates [--hours N] [--test ID]   each test's runs, reds and failure rate, worst first
  mint                            print a fresh test id
  stamp --path FILE [--path FILE…]  write #[test_id(<id>)] above each unstamped test
exit codes: 0 ok, 1 refused (stamp: a #[test] inside macro_rules!), 2 usage, 3 the registry cannot be opened or the log store cannot be read
mint, stamp, flips and rates never open the registry; flips and rates read test.result events and run nothing.

";

fn mint_id(rng: &mut dyn Rng) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut n = rng.next_u64();
    let mut id = String::from("t");
    for _ in 0..6 {
        id.push(ALPHABET[(n % 36) as usize] as char);
        n /= 36;
    }
    id
}

fn stamp(paths: &str, rng: &mut dyn Rng) -> Outcome {
    let usage = |msg: String| Outcome { code: 2, stdout: String::new(), stderr: msg };
    let mut files: Vec<(String, String)> = Vec::new();
    for p in paths.split('\n') {
        if files.iter().any(|(q, _)| q == p) {
            continue;
        }
        match std::fs::read_to_string(p) {
            Ok(src) => files.push((p.to_string(), src)),
            Err(e) => return usage(format!("stamp: cannot read {p}: {e}")),
        }
    }
    let mut seen: std::collections::BTreeSet<String> = files
        .iter()
        .flat_map(|(_, src)| spira_lint::rust_test::test_sites(src.as_bytes()))
        .filter_map(|s| s.id)
        .collect();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    for (path, src) in &files {
        let mut text = src.clone();
        let mut sites = spira_lint::rust_test::test_sites(src.as_bytes());
        sites.sort_by_key(|s| s.line);
        let mut inserts: Vec<(usize, String)> = Vec::new();
        for s in sites.iter().filter(|s| s.id.is_none()) {
            if s.in_macro_rules {
                stderr.push(format!("{path}:{}: a #[test] inside macro_rules! cannot be stamped: give the macro an id argument and write #[test_id($id)] in its body", s.line));
                continue;
            }
            let id = loop {
                let id = mint_id(rng);
                if seen.insert(id.clone()) {
                    break id;
                }
            };
            stdout.push(format!("{path}:{}\ttest:{id}\t{}", s.line, s.fn_name));
            inserts.push((s.line, format!("{}#[test_id({id})]\n", s.indent)));
        }
        let starts: Vec<usize> = std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        for (line, attr) in inserts.iter().rev() {
            text.insert_str(starts[line - 1], attr);
        }
        if text != *src {
            if let Err(e) = std::fs::write(path, &text) {
                return usage(format!("stamp: cannot write {path}: {e}"));
            }
        }
    }
    Outcome {
        code: i32::from(!stderr.is_empty()),
        stdout: stdout.join("\n"),
        stderr: stderr.join("\n"),
    }
}

fn table(header: &str, rows: impl Iterator<Item = String>) -> String {
    std::iter::once(header.to_string()).chain(rows).collect::<Vec<_>>().join("\n")
}

fn events(a: &Args, cfg: Cfg) -> Outcome {
    let (verb, j) = (a.verb.as_str(), a.json);
    let hours = match a.named.get("hours") {
        None => Some(DEFAULT_WINDOW_HOURS),
        Some(h) => h.parse::<u64>().ok().filter(|h| *h > 0),
    };
    let Some(window) = hours.and_then(|h| h.checked_mul(3600)) else {
        return fail(2, j, format!("{verb}: --hours must be a whole number of hours, at least 1"));
    };
    let url = match cfg(LOGS_KEY) {
        Ok(u) => u,
        Err(e) => return fail(3, j, format!("cannot read {LOGS_KEY}: {e}")),
    };
    let unread = |e: LogsError| fail(3, j, format!("{verb}: cannot read the log store at {url:?} ({LOGS_KEY}): {e}"));
    let logs = match Logs::new(&url) {
        Ok(l) => l.with_read_deadline(EVENTS_DEADLINE),
        Err(e) => return unread(e),
    };
    if verb == "flips" {
        return match logs.flips(window) {
            Ok(flips) => out(
                0,
                j,
                table(
                    "test\tname\tcrate\tinput_hash\tpasses\tfails\tfirst_seen\tlast_seen",
                    flips.iter().map(|f| format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", f.test_id, f.name, f.crate_name, f.input_hash, f.passes, f.fails, f.first_seen, f.last_seen)),
                ),
                flips.iter().map(|f| json!({"test": f.test_id, "name": f.name, "crate": f.crate_name, "input_hash": f.input_hash, "passes": f.passes, "fails": f.fails, "first_seen": f.first_seen, "last_seen": f.last_seen})).collect(),
            ),
            Err(e) => unread(e),
        };
    }
    match logs.rates(window, a.named.get("test").map(String::as_str)) {
        Ok(rates) => out(
            0,
            j,
            table("test\tname\tcrate\truns\treds\tfailure_rate", rates.iter().map(|r| format!("{}\t{}\t{}\t{}\t{}\t{:.3}", r.test_id, r.name, r.crate_name, r.runs, r.reds, r.rate))),
            rates.iter().map(|r| json!({"test": r.test_id, "name": r.name, "crate": r.crate_name, "runs": r.runs, "reds": r.reds, "failure_rate": r.rate})).collect(),
        ),
        Err(e) => unread(e),
    }
}

pub fn run(args: &[String], now: i64, cfg: Cfg, open: Opener, rng: &mut dyn Rng) -> Outcome {
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        return Outcome { code: 0, stdout: format!("{HELP}{INTENT}"), stderr: String::new() };
    }
    let json_mode = args.iter().any(|a| a == "--json");
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => return fail(2, json_mode, e),
    };
    let j = a.json;
    match a.verb.as_str() {
        "mint" => return out(0, j, mint_id(rng), json!({"ok": true})),
        "stamp" => return stamp(a.named.get("path").map(String::as_str).unwrap_or(""), rng),
        "flips" | "rates" => return events(&a, cfg),
        _ => {}
    }
    let get = |k: &str| a.named.get(k).map(String::as_str).unwrap_or("");
    let time = |k: &str| a.named.get(k).map(|v| parse_time(v)).transpose();
    let (until, at) = match (time("until"), time("at")) {
        (Ok(u), Ok(at)) => (u, at),
        (Err(e), _) | (_, Err(e)) => return fail(2, j, format!("{}: {e}", a.verb)),
    };
    let path = match cfg(PATH_KEY) {
        Ok(p) if !p.trim().is_empty() => p,
        Ok(_) => return fail(3, j, format!("{PATH_KEY} (spira.test_registry_path) is empty: the registry has no file; refusing")),
        Err(e) => return fail(3, j, format!("cannot read {PATH_KEY}: {e}")),
    };
    if a.verb == "backup" && !std::path::Path::new(&path).exists() {
        return fail(3, j, format!("backup: {path} does not exist: there is nothing to copy, and opening it would create an empty registry"));
    }
    let conn = match open(&path) {
        Ok(c) => c,
        Err(e) => return fail(3, j, e),
    };
    let refused = |e: String| fail(1, j, e);
    match a.verb.as_str() {
        "disable" | "quarantine" => {
            let hold = match cfg(HOLD_KEY)
                .and_then(|v| spira_config::process::parse_declared::<u64>(HOLD_KEY, &v))
            {
                Ok(h) => i64::try_from(h).unwrap_or(i64::MAX / 2),
                Err(e) => return fail(3, j, e),
            };
            let set = Set {
                test: get("test"),
                bead: get("bead"),
                reason: get("reason"),
                actor: get("actor"),
                until,
            };
            let res = if a.verb == "disable" {
                store::disable(&conn, now, hold, &set)
            } else {
                store::quarantine(&conn, now, hold, &set)
            };
            match res {
                Ok(r) => out(
                    0,
                    j,
                    row_text(&r, None),
                    json!({"ok": true, "row": row_json(&r, None)}),
                ),
                Err(e) => refused(e),
            }
        }
        "activate" => match store::activate(&conn, now, get("test"), get("reason"), get("actor")) {
            Ok(()) => out(
                0,
                j,
                format!("{} active", get("test")),
                json!({"ok": true, "test": get("test"), "state": "active"}),
            ),
            Err(e) => refused(e),
        },
        "clear-landed" => {
            let beads: Vec<&str> = get("bead").split('\n').collect();
            match store::clear_landed(&conn, now, &beads, get("actor")) {
                Ok(cleared) => out(
                    0,
                    j,
                    cleared.join("\n"),
                    json!({"ok": true, "cleared": cleared}),
                ),
                Err(e) => refused(e),
            }
        }
        "repoint" => match store::repoint(&conn, now, get("from"), get("to"), get("actor")) {
            Ok(moved) => out(0, j, moved.join("\n"), json!({"ok": true, "moved": moved})),
            Err(e) => refused(e),
        },
        "show" => match store::show(&conn, get("test")) {
            Ok(Some(r)) => out(
                0,
                j,
                row_text(&r, Some(now)),
                json!({"ok": true, "state": r.state, "row": row_json(&r, Some(now))}),
            ),
            Ok(None) => out(
                0,
                j,
                "active".into(),
                json!({"ok": true, "state": "active", "row": null}),
            ),
            Err(e) => refused(e),
        },
        "list" => {
            let at = at.unwrap_or(now);
            match store::list(&conn) {
                Ok(rows) => {
                    let text = rows
                        .iter()
                        .map(|r| row_text(r, Some(at)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    out(
                        0,
                        j,
                        text,
                        json!({"ok": true, "at": at, "rows": rows.iter().map(|r| row_json(r, Some(at))).collect::<Vec<_>>()}),
                    )
                }
                Err(e) => refused(e),
            }
        }
        "history" => match store::history(&conn, get("test")) {
            Ok(h) => {
                let text = h
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                out(0, j, text, json!({"ok": true, "history": h}))
            }
            Err(e) => refused(e),
        },
        "backup" => match store::backup(&conn, get("out")) {
            Ok((bytes, rows)) => out(
                0,
                j,
                format!("{} {bytes} {rows}", get("out")),
                json!({"ok": true, "out": get("out"), "bytes": bytes, "rows": rows}),
            ),
            Err(e) => fail(3, j, e),
        },
        _ => {
            let at = at.unwrap_or(now);
            let text = match store::try_deny_list(&conn, at) {
                Ok(t) => t,
                Err(e) => return fail(3, j, e),
            };
            let rows = text.lines().count() - 1;
            let value = json!({"ok": true, "at": at, "rows": rows, "digest": store::digest(&text), "text": text});
            out(0, j, text, value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::SeededRng;
    use spira_otel::vlogs::fake::{FakeLogs, Script};

    fn run(args: &[String], now: i64, cfg: Cfg, open: Opener) -> Outcome {
        super::run(args, now, cfg, open, &mut SeededRng::new(0))
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn cfg_with(path: &'static str) -> impl Fn(&str) -> Result<String, String> {
        move |k| match k {
            PATH_KEY => Ok(path.into()),
            HOLD_KEY => Ok("600".into()),
            _ => Err(format!("{k} unknown")),
        }
    }

    fn mem(_: &str) -> Result<Connection, String> {
        let c = Connection::open_in_memory().map_err(|e| e.to_string())?;
        store::init(&c)?;
        Ok(c)
    }

    fn two_runs() -> FakeLogs {
        let row = |id: &str, hash: &str, outcome: &str| {
            format!(r#"{{"test_id":"{id}","input_hash":"{hash}","outcome":"{outcome}","name":"a::{id}","crate":"queue","n":"1","first":"2026-11-27T01:00:00Z","last":"2026-11-27T01:00:00Z"}}"#)
        };
        let rows = [row("tm1a9x4", "h1", "pass"), row("tm1a9x4", "h1", "fail"), row("tm1a9x4", "h2", "pass"), row("tabc123", "h1", "fail"), row("tabc123", "h3", "pass")];
        let fake = FakeLogs::start();
        fake.answer("test_id:=\"tm1a9x4\"", Script { status: 200, body: rows[..3].join("\n"), hang: false });
        fake.answer("test.result", Script { status: 200, body: rows.join("\n"), hang: false });
        fake
    }

    /// Every key but the log store's address is unset, the registry path among them.
    fn logs_cfg(url: String) -> impl Fn(&str) -> Result<String, String> {
        move |k| if k == LOGS_KEY { Ok(url.clone()) } else { Err(format!("{k} unset")) }
    }

    fn never_open(p: &str) -> Result<Connection, String> {
        panic!("the registry was opened: {p}")
    }

    fn json_of(o: &Outcome) -> Value {
        serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {:?} {}", o.stdout, o.stderr))
    }

    #[test_id(tl6tges)]
    #[test]
    fn flips_lists_the_one_flip_without_a_registry_path_and_never_opens_the_registry() {
        let fake = two_runs();
        let cfg = logs_cfg(fake.url());
        let o = run(&argv("flips --json"), 1, &cfg, &never_open);
        assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
        assert_eq!(json_of(&o), json!([{"test": "tm1a9x4", "name": "a::tm1a9x4", "crate": "queue", "input_hash": "h1", "passes": 1, "fails": 1, "first_seen": "2026-11-27T01:00:00Z", "last_seen": "2026-11-27T01:00:00Z"}]));
        assert!(fake.queries()[0].starts_with("_time:604800s "), "the default window is 168 hours: {:?}", fake.queries());
        let o = run(&argv("flips --hours 24"), 1, &cfg, &never_open);
        assert_eq!(o.stdout, "test\tname\tcrate\tinput_hash\tpasses\tfails\tfirst_seen\tlast_seen\ntm1a9x4\ta::tm1a9x4\tqueue\th1\t1\t1\t2026-11-27T01:00:00Z\t2026-11-27T01:00:00Z");
        assert!(fake.queries()[1].starts_with("_time:86400s "));
    }

    #[test_id(t0oxpkf)]
    #[test]
    fn rates_prints_one_tests_runs_reds_and_failure_rate() {
        let fake = two_runs();
        let cfg = logs_cfg(fake.url());
        let o = run(&argv("rates --test tm1a9x4 --json"), 1, &cfg, &never_open);
        assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
        let got = json_of(&o);
        assert_eq!(got.as_array().map(Vec::len), Some(1));
        assert_eq!((&got[0]["test"], &got[0]["runs"], &got[0]["reds"]), (&json!("tm1a9x4"), &json!(3), &json!(1)));
        assert!((got[0]["failure_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
        let o = run(&argv("rates --test tm1a9x4"), 1, &cfg, &never_open);
        assert!(o.stdout.ends_with("tm1a9x4\ta::tm1a9x4\tqueue\t3\t1\t0.333"), "{}", o.stdout);
    }

    #[test_id(t9efo1k)]
    #[test]
    fn an_unreadable_log_store_exits_non_zero_naming_the_url() {
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", gone.local_addr().unwrap());
        drop(gone);
        for verb in ["flips", "rates"] {
            let o = run(&argv(verb), 1, &logs_cfg(url.clone()), &never_open);
            assert_ne!(o.code, 0, "{verb}");
            assert!(o.stderr.contains(&url) && o.stdout.is_empty(), "{verb}: {} {}", o.stderr, o.stdout);
            let o = run(&argv(&format!("{verb} --json")), 1, &logs_cfg(url.clone()), &never_open);
            assert!(o.code != 0 && json_of(&o)["error"].as_str().is_some_and(|e| e.contains(&url)), "{verb}: {}", o.stdout);
        }
        let o = run(&argv("flips"), 1, &logs_cfg(String::new()), &never_open);
        assert!(o.code != 0 && o.stderr.contains(LOGS_KEY), "{}", o.stderr);
    }

    #[test_id(tgao8cp)]
    #[test]
    fn a_window_that_is_not_a_whole_number_of_hours_is_a_usage_error() {
        for line in ["flips --hours 0", "rates --hours x", "flips --hours 99999999999999999999"] {
            let o = run(&argv(line), 1, &logs_cfg("http://127.0.0.1:1".into()), &never_open);
            assert_eq!(o.code, 2, "{line}: {}", o.stderr);
        }
    }

    #[test_id(tu62qp2)]
    #[test]
    fn an_empty_path_falls_back_to_the_registry_file_in_the_run_dir() {
        let c = with_run_default(cfg_with(""), || Ok("r/run/".into()));
        assert_eq!(c(PATH_KEY).unwrap(), "r/run/test-registry.sqlite");
        assert_eq!(c(HOLD_KEY).unwrap(), "600");
        let set = with_run_default(cfg_with("elsewhere.sqlite"), || Err("no run dir".into()));
        assert_eq!(set(PATH_KEY).unwrap(), "elsewhere.sqlite");
        let none = with_run_default(cfg_with(""), || Err("no run dir".into()));
        assert!(none(PATH_KEY).is_err());
    }

    #[test_id(t2cz1xr)]
    #[test]
    fn an_empty_path_exits_3_naming_the_key() {
        let o = run(&argv("list"), 1, &cfg_with(""), &mem);
        assert_eq!(o.code, 3);
        assert!(o.stderr.contains(PATH_KEY), "{}", o.stderr);
    }

    #[test_id(theczrj)]
    #[test]
    fn an_empty_path_exits_3_for_a_write_verb_and_for_deny_list_even_with_a_bad_time() {
        for line in [
            "disable --test suite:a.sh --bead sp-a --reason r --actor a",
            "deny-list --at 5",
            "list --at 5",
        ] {
            assert_eq!(run(&argv(line), 1, &cfg_with(""), &mem).code, 3, "{line}");
        }
    }

    #[test_id(t9mqi8c)]
    #[test]
    fn a_hold_that_is_not_an_unsigned_integer_exits_3() {
        let cfg = |k: &str| {
            Ok(if k == PATH_KEY {
                "x".to_string()
            } else {
                "-5".to_string()
            })
        };
        let o = run(
            &argv("disable --test suite:a.sh --bead sp-a --reason r --actor a"),
            1,
            &cfg,
            &mem,
        );
        assert_eq!(o.code, 3, "{}", o.stderr);
    }

    #[test_id(to598n8)]
    #[test]
    fn every_exit_path_prints_json_under_json() {
        let ok_cfg = cfg_with("x");
        let cases: Vec<(Outcome, i32)> = vec![
            (run(&argv("list --json"), 1, &cfg_with(""), &mem), 3),
            (
                run(&argv("list --json"), 1, &ok_cfg, &|_| {
                    Err("cannot open".into())
                }),
                3,
            ),
            (run(&argv("nonsense --json"), 1, &ok_cfg, &mem), 2),
            (
                run(&argv("disable --json --test suite:a.sh"), 1, &ok_cfg, &mem),
                2,
            ),
            (
                run(
                    &argv("activate --json --test suite:a.sh --reason r --actor a"),
                    1,
                    &ok_cfg,
                    &mem,
                ),
                1,
            ),
            (run(&argv("list --json"), 1, &ok_cfg, &mem), 0),
        ];
        for (o, code) in cases {
            assert_eq!(o.code, code);
            let v: Value = serde_json::from_str(&o.stdout)
                .unwrap_or_else(|e| panic!("exit {code}: {e}: {:?}", o.stdout));
            assert!(v.is_object());
        }
    }

    #[test_id(t1o2hap)]
    #[test]
    fn disable_without_a_bead_is_a_usage_error_and_a_malformed_bead_is_refused() {
        let c = cfg_with("x");
        assert_eq!(
            run(
                &argv("disable --test suite:a.sh --reason r --actor a"),
                1,
                &c,
                &mem
            )
            .code,
            2
        );
        assert_eq!(
            run(
                &argv("disable --test suite:a.sh --bead ../x --reason r --actor a"),
                1,
                &c,
                &mem
            )
            .code,
            1
        );
        assert_eq!(
            run(
                &argv("disable --test bogus --bead sp-a --reason r --actor a"),
                1,
                &c,
                &mem
            )
            .code,
            1
        );
    }

    #[test_id(thtpqk3)]
    #[test]
    fn disable_then_deny_list_round_trips_through_the_verbs() {
        let dir = testkit::TempDir::new("test-registry-cli");
        let file: &'static str =
            Box::leak(dir.join("r.sqlite").display().to_string().into_boxed_str());
        let c = cfg_with(file);
        let real = |p: &str| store::open(p);
        let o = run(
            &argv("disable --test suite:a.sh --bead sp-a --reason r --actor a --until 5000"),
            1000,
            &c,
            &real,
        );
        assert_eq!(o.code, 0, "{}", o.stderr);
        let o = run(&argv("deny-list --at 2000"), 1000, &c, &real);
        assert_eq!(
            o.stdout,
            "# test-registry deny-list at=2000 rows=1\ndisabled\tsuite:a.sh\tsp-a\t5000\n"
        );
        let o = run(&argv("deny-list --at 5000 --json"), 1000, &c, &real);
        assert!(o.stdout.contains("\"rows\":0"), "{}", o.stdout);
    }

    #[test_id(t1m8ppn)]
    #[test]
    fn clear_landed_takes_repeated_beads_and_prints_the_cleared_tests() {
        let dir = testkit::TempDir::new("test-registry-cli-clear");
        let file: &'static str =
            Box::leak(dir.join("r.sqlite").display().to_string().into_boxed_str());
        let c = cfg_with(file);
        let real = |p: &str| store::open(p);
        for (t, b) in [
            ("suite:a.sh", "sp-a"),
            ("suite:b.sh", "sp-b"),
            ("suite:c.sh", "sp-c"),
        ] {
            let line = format!("disable --test {t} --bead {b} --reason r --actor a --until 5000");
            assert_eq!(run(&argv(&line), 1000, &c, &real).code, 0);
        }
        let o = run(
            &argv("clear-landed --bead sp-a --bead sp-b --actor batcher"),
            1100,
            &c,
            &real,
        );
        assert_eq!(
            (o.code, o.stdout.as_str()),
            (0, "suite:a.sh\nsuite:b.sh"),
            "{}",
            o.stderr
        );
        let o = run(
            &argv("clear-landed --bead sp-none --actor batcher --json"),
            1100,
            &c,
            &real,
        );
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(
            (o.code, v),
            (0, serde_json::json!({"cleared": [], "ok": true}))
        );
        let o = run(&argv("list"), 1100, &c, &real);
        assert!(
            o.stdout.contains("suite:c.sh") && !o.stdout.contains("suite:a.sh"),
            "{}",
            o.stdout
        );
    }

    #[test_id(tguc1nk)]
    #[test]
    fn repoint_prints_the_moved_tests_in_text_and_json() {
        let dir = testkit::TempDir::new("test-registry-cli-repoint");
        let file: &'static str =
            Box::leak(dir.join("r.sqlite").display().to_string().into_boxed_str());
        let c = cfg_with(file);
        let real = |p: &str| store::open(p);
        for (t, b) in [("suite:a.sh", "sp-i"), ("suite:b.sh", "sp-x")] {
            let line = format!("disable --test {t} --bead {b} --reason r --actor a --until 5000");
            assert_eq!(run(&argv(&line), 1000, &c, &real).code, 0);
        }
        let o = run(
            &argv("repoint --from sp-i --to sp-f --actor bead"),
            1100,
            &c,
            &real,
        );
        assert_eq!(
            (o.code, o.stdout.as_str()),
            (0, "suite:a.sh"),
            "{}",
            o.stderr
        );
        let o = run(
            &argv("repoint --from sp-i --to sp-f --actor bead --json"),
            1100,
            &c,
            &real,
        );
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(
            (o.code, v),
            (0, serde_json::json!({"moved": [], "ok": true}))
        );
        let o = run(
            &argv("clear-landed --bead sp-f --actor batcher"),
            1200,
            &c,
            &real,
        );
        assert_eq!(o.stdout, "suite:a.sh");
    }

    #[test_id(t3aq1iq)]
    #[test]
    fn backup_writes_a_file_that_opens_with_the_same_rows() {
        let dir = testkit::TempDir::new("test-registry-backup");
        let file: &'static str =
            Box::leak(dir.join("r.sqlite").display().to_string().into_boxed_str());
        let c = cfg_with(file);
        let real = |p: &str| store::open(p);
        for t in ["suite:a.sh", "suite:b.sh"] {
            let line = format!("disable --test {t} --bead sp-a --reason r --actor a --until 5000");
            assert_eq!(run(&argv(&line), 1000, &c, &real).code, 0);
        }
        let out = dir.join("copy.sqlite").display().to_string();
        let o = run(&argv(&format!("backup --out {out}")), 1000, &c, &real);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let fields: Vec<&str> = o.stdout.split(' ').collect();
        assert_eq!(fields[0], out);
        assert_eq!(fields[2], "2", "{}", o.stdout);
        assert!(fields[1].parse::<u64>().unwrap() > 0);
        let copy = Connection::open(&out).unwrap();
        let names: Vec<String> = store::list(&copy)
            .unwrap()
            .into_iter()
            .map(|r| r.test)
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(!std::path::Path::new(&format!("{out}.tmp")).exists());
    }

    #[test_id(tz2568w)]
    #[test]
    fn backup_of_a_registry_that_does_not_exist_refuses_and_creates_nothing() {
        let dir = testkit::TempDir::new("test-registry-backup-missing");
        let file: &'static str = Box::leak(
            dir.join("absent.sqlite")
                .display()
                .to_string()
                .into_boxed_str(),
        );
        let out = dir.join("copy.sqlite").display().to_string();
        let o = run(
            &argv(&format!("backup --out {out}")),
            1,
            &cfg_with(file),
            &|p| store::open(p),
        );
        assert_eq!(o.code, 3, "{}", o.stderr);
        assert!(!std::path::Path::new(file).exists() && !std::path::Path::new(&out).exists());
    }

    #[test_id(taja21a)]
    #[test]
    fn until_accepts_rfc3339_utc_and_epoch() {
        assert_eq!(parse_time("1970-01-02T00:00:00Z").unwrap(), 86400);
        assert_eq!(parse_time("2026-11-15T00:00:00Z").unwrap(), 1794700800);
        assert_eq!(parse_time("1234").unwrap(), 1234);
        assert!(parse_time("tomorrow").is_err());
    }

    fn no_registry(_: &str) -> Result<Connection, String> {
        panic!("mint and stamp must not open the registry")
    }

    fn id_run(line: &str, seed: u64) -> Outcome {
        super::run(&argv(line), 1, &cfg_with(""), &no_registry, &mut SeededRng::new(seed))
    }

    fn is_id(s: &str) -> bool {
        s.len() == 7 && s.starts_with('t') && s[1..].bytes().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
    }

    const SRC: &str = "const S: &str = \"#[test] fn fake() {}\";\n#[test]\nfn one() {}\n\nmod m {\n    #[tokio::test]\n    async fn two() {}\n\n    #[test_id(t7k3m9q)]\n    #[test]\n    fn three() {}\n}\n";

    fn fixture(tag: &str, src: &str) -> (testkit::TempDir, String) {
        let dir = testkit::TempDir::new(tag);
        let p = dir.path().join("f.rs");
        std::fs::write(&p, src).unwrap();
        let p = p.display().to_string();
        (dir, p)
    }

    #[test_id(ta66b4k)]
    #[test]
    fn mint_under_one_seed_prints_the_same_id_twice_and_it_fits_the_grammar() {
        let (a, b) = (id_run("mint", 7), id_run("mint", 7));
        assert_eq!((a.code, a.stdout.as_str()), (0, b.stdout.as_str()));
        assert!(is_id(&a.stdout), "{}", a.stdout);
        assert_ne!(id_run("mint", 8).stdout, a.stdout);
    }

    #[test_id(ti4kxbp)]
    #[test]
    fn stamp_writes_an_attribute_above_each_unstamped_test_at_its_indent_and_only_there() {
        let (_d, p) = fixture("test-registry-stamp", SRC);
        let o = id_run(&format!("stamp --path {p}"), 7);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let after = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = after.lines().collect();
        assert_eq!(after.matches("#[test_id(").count(), 3, "{after}");
        let one = lines.iter().position(|l| *l == "fn one() {}").unwrap();
        assert!(lines[one - 2].starts_with("#[test_id(t") && lines[one - 1] == "#[test]", "{after}");
        let two = lines.iter().position(|l| l.contains("async fn two")).unwrap();
        assert!(lines[two - 2].starts_with("    #[test_id(t") && lines[two - 1] == "    #[tokio::test]", "{after}");
        assert!(after.contains("\"#[test] fn fake() {}\""), "{after}");
        let reported: Vec<&str> = o.stdout.lines().collect();
        assert_eq!(reported.len(), 2, "{}", o.stdout);
        assert!(reported[0].starts_with(&format!("{p}:2\ttest:t")) && reported[0].ends_with("\tone"), "{}", o.stdout);
        let ids: Vec<&str> = after.lines().filter_map(|l| l.trim().strip_prefix("#[test_id(")?.strip_suffix(")]")).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|i| is_id(i)) && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == 3);
    }

    #[test_id(tfn5v04)]
    #[test]
    fn a_second_stamp_changes_no_byte_and_exits_0() {
        let (_d, p) = fixture("test-registry-stamp-twice", SRC);
        id_run(&format!("stamp --path {p}"), 7);
        let once = std::fs::read(&p).unwrap();
        let o = id_run(&format!("stamp --path {p}"), 9);
        assert_eq!((o.code, o.stdout.as_str()), (0, ""));
        assert_eq!(std::fs::read(&p).unwrap(), once);
    }

    #[test_id(tndddc8)]
    #[test]
    fn a_test_inside_macro_rules_is_refused_by_line_and_the_rest_is_still_stamped() {
        let src = "macro_rules! m {\n    ($n:ident) => {\n        #[test]\n        fn $n() {}\n    };\n}\n#[test]\nfn plain() {}\n";
        let (_d, p) = fixture("test-registry-stamp-macro", src);
        let o = id_run(&format!("stamp --path {p}"), 7);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains(&format!("{p}:3: a #[test] inside macro_rules! cannot be stamped")), "{}", o.stderr);
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after.matches("#[test_id(").count(), 1, "{after}");
        assert!(after.contains("fn plain"));
    }

    #[test_id(t6veeqs)]
    #[test]
    fn stamp_without_a_path_or_with_a_positional_one_is_usage_naming_path() {
        let none = id_run("stamp", 7);
        assert_eq!(none.code, 2);
        assert!(none.stderr.contains("--path"), "{}", none.stderr);
        let (_d, p) = fixture("test-registry-stamp-positional", SRC);
        let pos = id_run(&format!("stamp {p}"), 7);
        assert_eq!(pos.code, 2);
        assert!(pos.stderr.contains("--path"), "{}", pos.stderr);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), SRC);
        assert_eq!(id_run("stamp --path /nonexistent/zz.rs", 7).code, 2);
    }

    #[test_id(trjf6i5)]
    #[test]
    fn stamp_never_mints_an_id_already_in_the_given_files() {
        let first = id_run("mint", 7).stdout;
        let src = format!("#[test_id({first})]\n#[test]\nfn held() {{}}\n#[test]\nfn bare() {{}}\n");
        let (_d, p) = fixture("test-registry-stamp-collide", &src);
        let o = id_run(&format!("stamp --path {p}"), 7);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let minted = o.stdout.split("test:").nth(1).unwrap().split('\t').next().unwrap().to_string();
        assert_ne!(minted, first);
    }

    #[test_id(thqw9fu)]
    #[test]
    fn stamp_across_two_files_and_a_repeated_path_mints_distinct_ids_once_per_test() {
        let (_d, a) = fixture("test-registry-stamp-two-a", "#[test]\nfn a() {}\n");
        let (_e, b) = fixture("test-registry-stamp-two-b", "#[test]\nfn b() {}\n");
        let o = id_run(&format!("stamp --path {a} --path {b} --path {a}"), 7);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout.lines().count(), 2, "{}", o.stdout);
        for p in [&a, &b] {
            assert_eq!(std::fs::read_to_string(p).unwrap().matches("#[test_id(").count(), 1);
        }
    }

    #[test_id(tznssud)]
    #[test]
    fn help_carries_the_intent_paragraph_and_each_new_verb() {
        let o = id_run("-h", 7);
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains(INTENT) && o.stdout.contains("stamp --path") && o.stdout.contains("mint"));
    }
}
