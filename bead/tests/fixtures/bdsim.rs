//! Pure, in-memory bd query functions driven by a canned-JSON fixture: the engine behind
//! `SPIRA_BDJSON_FIXTURE`. `bdq` and every reader that honours the variable answer
//! `list`/`show`/`memories` from the fixture file instead of a live store.
//!
//! The fixture is a flat array of bead objects (serves `list` and `show`), or an object with
//! `list`/`show`/`memories` keys. An unreadable or malformed fixture prints nothing and exits 1
//! — the shape of a `bd` that cannot reach its store.
//!
//! Only the shapes the fixture suites issue are simulated: list's `--status/--all/--label/
//! --limit/--closed-after`, show's positional ids, bare `memories`. Anything else exits naming
//! what it does not simulate; a hand-written model of bd's filtering would drift from the real
//! one (law-prefer-the-real-dependency), and such queries belong in test-cockpit-bd-contract.sh.

use std::collections::BTreeSet;

use serde_json::{json, Value};

/// `Ok(stdout)`, or `Err((exit code, stderr))`.
pub type Answer = Result<String, (i32, String)>;

struct ListArgs {
    status: Option<BTreeSet<String>>,
    labels: Vec<String>,
    all: bool,
    limit: usize,
    closed_after: Option<String>,
}

fn parse_list_args(args: &[String]) -> Result<ListArgs, (i32, String)> {
    let mut a = ListArgs { status: None, labels: Vec::new(), all: false, limit: 0, closed_after: None };
    let mut i = 0;
    let value = |i: usize| args.get(i).cloned().ok_or_else(|| (1, "bdsim: missing value".to_string()));
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            "--all" => a.all = true,
            "--status" => {
                i += 1;
                a.status = Some(value(i)?.split(',').map(str::to_string).collect());
            }
            "--label" | "-l" => {
                i += 1;
                a.labels.extend(value(i)?.split(',').filter(|x| !x.is_empty()).map(str::to_string));
            }
            "--closed-after" => {
                i += 1;
                a.closed_after = Some(value(i)?);
            }
            "--limit" | "-n" => {
                i += 1;
                a.limit = value(i)?.parse().map_err(|_| (1, "bdsim: bad --limit".to_string()))?;
            }
            other => return Err((1, format!("bdsim: list '{other}' is not simulated"))),
        }
        i += 1;
    }
    Ok(a)
}

fn bd_list(beads: &[Value], a: &ListArgs) -> Vec<Value> {
    let keep = |b: &Value| {
        let status = b.get("status").and_then(Value::as_str);
        if let Some(after) = &a.closed_after {
            let closed_at = b.get("closed_at").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(after);
            if status != Some("closed") || closed_at < after.as_str() {
                return false;
            }
        }
        match &a.status {
            Some(set) => {
                if !status.is_some_and(|s| set.contains(s)) {
                    return false;
                }
            }
            None => {
                if !a.all && status == Some("closed") {
                    return false;
                }
            }
        }
        if !a.labels.is_empty() {
            let have: BTreeSet<&str> =
                b.get("labels").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            if !a.labels.iter().all(|l| have.contains(l.as_str())) {
                return false;
            }
        }
        true
    };
    let out: Vec<Value> = beads.iter().filter(|b| keep(b)).cloned().collect();
    if a.limit > 0 {
        out.into_iter().take(a.limit).collect()
    } else {
        out
    }
}

fn bd_show(pool: &[Value], ids: &[&String]) -> Vec<Value> {
    ids.iter()
        .filter_map(|want| pool.iter().rev().find(|b| b.get("id").and_then(Value::as_str) == Some(want.as_str())).cloned())
        .collect()
}

/// `fixture` is the fixture file's text.
pub fn answer(fixture: &str, sub: &str, rest: &[String]) -> Answer {
    let fx: Value = serde_json::from_str(fixture).map_err(|_| (1, String::new()))?;
    let (top_list, top): (Vec<Value>, Value) = match &fx {
        Value::Array(a) => (a.clone(), json!({})),
        Value::Object(o) => (o.get("list").and_then(Value::as_array).cloned().unwrap_or_default(), fx.clone()),
        _ => return Err((1, String::new())),
    };
    let out = match sub {
        "list" => Value::Array(bd_list(&top_list, &parse_list_args(rest)?)),
        "show" => {
            let ids: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
            let pool: Vec<Value> = match top.get("show") {
                Some(Value::Object(m)) => m.values().cloned().collect(),
                Some(Value::Array(a)) => a.clone(),
                _ => top_list,
            };
            Value::Array(bd_show(&pool, &ids))
        }
        "memories" => top.get("memories").cloned().unwrap_or_else(|| json!({})),
        other => return Err((1, format!("bdsim: subcommand '{other}' is not simulated"))),
    };
    Ok(format!("{out}\n"))
}

/// `bdsim <fixture-file> <bd-subcommand> [args...]`: the whole command, returning its exit code.
pub fn run(argv: &[String]) -> i32 {
    if argv.len() < 2 {
        eprintln!("usage: bdsim <fixture-file> <bd-subcommand> [args...]");
        return 1;
    }
    let Ok(text) = std::fs::read_to_string(&argv[0]) else { return 1 };
    match answer(&text, &argv[1], &argv[2..]) {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err((code, err)) => {
            if !err.is_empty() {
                eprintln!("{err}");
            }
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    const FX: &str = r#"[{"id":"a","status":"open","labels":["x","y"]},{"id":"b","status":"closed","labels":["x"],"closed_at":"2026-01-02"},{"id":"c","status":"open","labels":["y"]}]"#;

    fn ids(out: &str) -> Vec<String> {
        serde_json::from_str::<Vec<Value>>(out).unwrap().iter().map(|b| b["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn list_hides_closed_unless_asked_and_ands_labels() {
        assert_eq!(ids(&answer(FX, "list", &s(&["--json"])).unwrap()), ["a", "c"]);
        assert_eq!(ids(&answer(FX, "list", &s(&["--all"])).unwrap()), ["a", "b", "c"]);
        assert_eq!(ids(&answer(FX, "list", &s(&["--label", "x,y"])).unwrap()), ["a"]);
        assert_eq!(ids(&answer(FX, "list", &s(&["--status", "closed"])).unwrap()), ["b"]);
        assert_eq!(ids(&answer(FX, "list", &s(&["--all", "--limit", "2"])).unwrap()), ["a", "b"]);
        assert_eq!(ids(&answer(FX, "list", &s(&["--all", "--closed-after", "2026-01-01"])).unwrap()), ["b"]);
        assert!(ids(&answer(FX, "list", &s(&["--all", "--closed-after", "2026-02-01"])).unwrap()).is_empty());
    }

    #[test]
    fn show_returns_requested_ids_in_order_and_object_fixtures_split_keys() {
        assert_eq!(ids(&answer(FX, "show", &s(&["c", "--json", "a", "zz"])).unwrap()), ["c", "a"]);
        let obj = r#"{"list":[{"id":"a"}],"show":{"k":{"id":"s"}},"memories":{"m":1}}"#;
        assert_eq!(ids(&answer(obj, "show", &s(&["s"])).unwrap()), ["s"]);
        assert_eq!(answer(obj, "memories", &[]).unwrap().trim(), r#"{"m":1}"#);
    }

    #[test]
    fn unsimulated_shapes_and_bad_fixtures_fail() {
        assert_eq!(answer(FX, "list", &s(&["--type", "x"])).unwrap_err().0, 1);
        assert_eq!(answer(FX, "create", &[]).unwrap_err().0, 1);
        let bad = answer("not json", "list", &[]).unwrap_err();
        assert_eq!(bad, (1, String::new()));
    }
}
