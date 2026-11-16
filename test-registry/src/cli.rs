//! The verbs: named arguments, exit 0 ok, 1 refused, 2 usage, 3 the registry cannot be opened.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::store::{self, Row, Set};

pub const PATH_KEY: &str = "SPIRA_TEST_REGISTRY_PATH";
pub const HOLD_KEY: &str = "SPIRA_TEST_REGISTRY_HOLD_SECS";

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
];

const REPEATABLE: &[(&str, &str)] = &[("clear-landed", "bead")];

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
            .ok_or_else(|| format!("{verb}: unexpected argument {a:?}"))?;
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

pub fn run(args: &[String], now: i64, cfg: Cfg, open: Opener) -> Outcome {
    let json_mode = args.iter().any(|a| a == "--json");
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => return fail(2, json_mode, e),
    };
    let j = a.json;
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

    #[test]
    fn an_empty_path_exits_3_naming_the_key() {
        let o = run(&argv("list"), 1, &cfg_with(""), &mem);
        assert_eq!(o.code, 3);
        assert!(o.stderr.contains(PATH_KEY), "{}", o.stderr);
    }

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

    #[test]
    fn a_hold_that_is_not_an_unsigned_integer_exits_3() {
        let cfg = |k: &str| {
            Ok(if k == PATH_KEY {
                "/x".to_string()
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

    #[test]
    fn every_exit_path_prints_json_under_json() {
        let ok_cfg = cfg_with("/x");
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

    #[test]
    fn disable_without_a_bead_is_a_usage_error_and_a_malformed_bead_is_refused() {
        let c = cfg_with("/x");
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

    #[test]
    fn until_accepts_rfc3339_utc_and_epoch() {
        assert_eq!(parse_time("1970-01-02T00:00:00Z").unwrap(), 86400);
        assert_eq!(parse_time("2026-11-15T00:00:00Z").unwrap(), 1794700800);
        assert_eq!(parse_time("1234").unwrap(), 1234);
        assert!(parse_time("tomorrow").is_err());
    }
}
