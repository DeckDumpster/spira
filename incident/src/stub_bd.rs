//! A stateful stand-in `bd` for suites exercising incident's decision layer without a Dolt
//! fixture. Nothing but a test's `SPIRA_BD` points here. It is a small stateful bd rather than a
//! canned response because dedup and recurrence are a sequence of calls that must see each
//! other's writes.
//!
//! State is a JSON file named by `STUB_BD_STATE`; every invocation is appended, space-joined,
//! to `STUB_BD_LOG` so a suite can count calls. Implemented: `list`, `create`, `show`,
//! `label add|remove|list`, `note`, `set-state`, `sql` (only `INSERT INTO events` and
//! `SELECT COUNT(*) FROM events`) and `seed` (merge a JSON bead from stdin).

use std::collections::BTreeSet;
use std::io::{Read, Write};

use regex::Regex;
use serde_json::{json, Map, Value};

fn load_state(path: &str) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"beads": {}, "events": [], "next_id": 1}))
}

fn save_state(path: &str, state: &Value) {
    let _ = std::fs::write(path, state.to_string());
}

fn beads(state: &mut Value) -> &mut Map<String, Value> {
    state["beads"].as_object_mut().expect("beads is an object")
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn labels_of(b: &Value) -> Vec<&str> {
    b.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

fn bead_entry<'a>(state: &'a mut Value, id: &str, default: Value) -> &'a mut Value {
    beads(state).entry(id.to_string()).or_insert(default)
}

fn list(state: &Value, argv: &[String]) -> String {
    let (mut statuses, mut label, mut closed_after, mut ids): (Vec<&str>, Option<&str>, Option<&str>, Option<BTreeSet<&str>>) =
        (Vec::new(), None, None, None);
    let mut i = 1;
    while i < argv.len() {
        let val = argv.get(i + 1).map(String::as_str).unwrap_or("");
        match argv[i].as_str() {
            "--status" => {
                statuses = val.split(',').collect();
                i += 2;
            }
            "--label" => {
                label = Some(val);
                i += 2;
            }
            "--closed-after" => {
                closed_after = Some(val);
                i += 2;
            }
            "--id" => {
                ids = Some(val.split(',').collect());
                i += 2;
            }
            _ => i += 1,
        }
    }
    let want: Vec<&str> = label.map(|l| l.split(',').collect()).unwrap_or_default();
    let rows: Vec<Value> = state["beads"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter(|b| {
            if !statuses.is_empty() && !statuses.contains(&str_of(b, "status")) {
                return false;
            }
            if ids.as_ref().is_some_and(|s| !s.contains(str_of(b, "id"))) {
                return false;
            }
            if !want.is_empty() {
                let have = labels_of(b);
                if !want.iter().all(|w| have.contains(w)) {
                    return false;
                }
            }
            !closed_after.is_some_and(|c| !c.is_empty() && str_of(b, "closed_at") < c)
        })
        .cloned()
        .collect();
    Value::Array(rows).to_string()
}

fn sql(state: &mut Value, query: &str) -> Option<String> {
    let insert = Regex::new(r"INSERT INTO events .*VALUES \('([^']*)', '([^']*)', '([^']*)', '([^']*)', '([^']*)', \w+\(\)\)").unwrap();
    if let Some(m) = insert.captures(query) {
        let ev = json!({"issue_id": &m[2], "event_type": &m[3], "new_value": &m[5]});
        state["events"].as_array_mut()?.push(ev);
        return None;
    }
    let count = Regex::new(r"SELECT COUNT\(\*\) FROM events WHERE issue_id='([^']*)' AND event_type='([^']*)'").unwrap();
    let m = count.captures(query)?;
    let n = state["events"]
        .as_array()?
        .iter()
        .filter(|e| str_of(e, "issue_id") == &m[1] && str_of(e, "event_type") == &m[2])
        .count();
    Some(format!("header\n----\n{n}\n"))
}

/// Runs one invocation; `Ok(stdout)`.
pub fn run(argv: &[String], stdin: &mut dyn Read) -> Result<String, String> {
    let argv: &[String] = if argv.first().map(String::as_str) == Some("-C") { argv.get(2..).unwrap_or(&[]) } else { argv };
    if let Ok(log) = std::env::var("STUB_BD_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{}", argv.join(" "));
        }
    }
    let state_path = std::env::var("STUB_BD_STATE").map_err(|_| "incident-stub-bd: STUB_BD_STATE is not set".to_string())?;
    let mut state = load_state(&state_path);
    let Some(cmd) = argv.first() else { return Ok(String::new()) };
    let arg = |n: usize| argv.get(n).map(String::as_str).ok_or_else(|| format!("incident-stub-bd: {cmd}: missing argument {n}"));
    let mut out = String::new();
    match cmd.as_str() {
        "seed" => {
            let mut text = String::new();
            stdin.read_to_string(&mut text).map_err(|e| e.to_string())?;
            let bead: Value = serde_json::from_str(&text).map_err(|e| format!("incident-stub-bd: seed: {e}"))?;
            let id = bead.get("id").and_then(Value::as_str).ok_or("incident-stub-bd: seed: no id")?.to_string();
            beads(&mut state).insert(id, bead);
        }
        "list" => return Ok(format!("{}\n", list(&state, argv))),
        "create" => {
            let title = argv.get(1).cloned().unwrap_or_default();
            let (mut labels, mut ext_ref, mut itype, mut priority) = (Vec::new(), String::new(), "bug".to_string(), "2".to_string());
            let mut i = 2;
            while i < argv.len() {
                let val = argv.get(i + 1).cloned().unwrap_or_default();
                match argv[i].as_str() {
                    "--labels" => labels = val.split(',').map(str::to_string).collect(),
                    "--external-ref" => ext_ref = val,
                    "--type" => itype = val,
                    "--priority" => priority = val,
                    _ => {
                        i += 1;
                        continue;
                    }
                }
                i += 2;
            }
            let n = state["next_id"].as_i64().unwrap_or(1);
            state["next_id"] = json!(n + 1);
            let bid = format!("sp-stub{n}");
            beads(&mut state).insert(
                bid.clone(),
                json!({"id": bid, "title": title, "type": itype, "priority": priority, "labels": labels,
                       "external_ref": ext_ref, "status": "open", "closed_at": null, "notes": ""}),
            );
            out = format!("{bid}\n");
        }
        "show" => {
            let b = state["beads"].get(arg(1)?).cloned().unwrap_or_else(|| json!({}));
            return Ok(format!("{b}\n"));
        }
        "label" => {
            let (sub, bid) = (arg(1)?, arg(2)?);
            let bead = bead_entry(&mut state, bid, json!({"id": bid, "labels": []}));
            if bead.get("labels").is_none() {
                bead["labels"] = json!([]);
            }
            let labs = bead["labels"].as_array_mut().ok_or("incident-stub-bd: labels is not a list")?;
            match sub {
                "add" => {
                    let lab = json!(arg(3)?);
                    if !labs.contains(&lab) {
                        labs.push(lab);
                    }
                }
                "remove" => {
                    let lab = json!(arg(3)?);
                    if let Some(p) = labs.iter().position(|l| *l == lab) {
                        labs.remove(p);
                    }
                }
                "list" => return Ok(format!("{}\n", labs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))),
                _ => {}
            }
        }
        "note" => {
            let (bid, text) = (arg(1)?, arg(2)?);
            let bead = bead_entry(&mut state, bid, json!({"id": bid, "notes": ""}));
            let prev = str_of(bead, "notes").to_string();
            bead["notes"] = json!(if prev.is_empty() { text.to_string() } else { format!("{prev}\n\n{text}") });
        }
        "set-state" => {
            let (bid, kv) = (arg(1)?, arg(2)?);
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            bead_entry(&mut state, bid, json!({"id": bid}))[k] = json!(v);
        }
        "sql" => {
            if let Some(o) = sql(&mut state, arg(1)?) {
                return Ok(o);
            }
        }
        _ => return Ok(String::new()),
    }
    save_state(&state_path, &state);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[&str]) -> String {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        run(&v, &mut std::io::empty()).unwrap()
    }

    #[test]
    fn a_second_call_sees_the_first_calls_writes() {
        let d = testkit::TempDir::new("stub-bd");
        let (state, log) = (d.join("state.json"), d.join("bd.log"));
        let _env = testkit::env(&[("STUB_BD_STATE", state.to_str()), ("STUB_BD_LOG", log.to_str())]);
        assert_eq!(call(&["-C", "db", "create", "t", "--labels", "a,b", "--external-ref", "r"]), "sp-stub1\n");
        assert_eq!(call(&["create", "u"]), "sp-stub2\n");
        let listed: Value = serde_json::from_str(&call(&["list", "--label", "a,b", "--json"])).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        call(&["label", "add", "sp-stub2", "x"]);
        call(&["label", "add", "sp-stub2", "x"]);
        assert_eq!(call(&["label", "list", "sp-stub2"]), "x\n");
        call(&["note", "sp-stub2", "one"]);
        call(&["note", "sp-stub2", "two"]);
        let shown: Value = serde_json::from_str(&call(&["show", "sp-stub2"])).unwrap();
        assert_eq!(shown["notes"], "one\n\ntwo");
        call(&["set-state", "sp-stub2", "status=closed"]);
        assert_eq!(call(&["show", "sp-nope"]), "{}\n");
        call(&["sql", "INSERT INTO events (a) VALUES ('1', 'sp-stub1', 'ev', 'me', 'v', NOW())"]);
        assert_eq!(call(&["sql", "SELECT COUNT(*) FROM events WHERE issue_id='sp-stub1' AND event_type='ev'"]), "header\n----\n1\n");
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().next(), Some("create t --labels a,b --external-ref r"));
    }
}
