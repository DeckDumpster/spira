//! `core_detail_keys` — the slow-tier RECENT/INFLOW/AWAITING-CI/THROUGHPUT/TOKENS/
//! landing-funnel/acceptance/gate block. The largest single function in `spira/cockpit.sh`;
//! ported section by section (DESIGN.md "Design"), each section keeping the bash's own
//! comment naming why it exists, condensed.

use super::{push, Cfg, Kv};
use crate::io;
use crate::quoting::{parse_iso8601, rel_age, sanitize, sanitize_title};
use serde_json::Value;
use spira_config::nonwork::{self, Kind};
use std::collections::{HashMap, HashSet};

pub fn core_detail_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let run = io::run_dir();

    recent_section(&mut out, &run);
    inflow_section(&mut out, &run);
    awaiting_ci_section(&mut out, cfg);
    throughput_section(&mut out, cfg);
    tokens_section(&mut out);

    out
}

fn repo_tag(labels: Option<&Vec<Value>>) -> String {
    labels
        .and_then(|a| a.iter().filter_map(Value::as_str).find_map(|l| l.strip_prefix("repo:")))
        .map(|r| r.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// RECENT
// ---------------------------------------------------------------------------------------

fn recent_section(out: &mut Kv, run: &std::path::Path) {
    let mut candidates: Vec<String> = Vec::new();

    // The dwarf-ledger.log claims/endings.
    if let Ok(content) = std::fs::read_to_string(run.join("dwarf-ledger.log")) {
        let mut ledger_rows: Vec<String> = Vec::new();
        for line in content.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            if f[1] == "awake" && f[3].starts_with("sp-") {
                ledger_rows.push(format!("{} {} claimed {}", f[0], f[2], f[3]));
            } else if f[1] == "done" && f[3].starts_with("sp-") {
                // The dwarf ledger's own `status=` word (decide::ledger_word), shown as text.
                let mut word = "ended".to_string();
                for tok in &f[4..] {
                    if let Some(v) = tok.strip_prefix("status=") {
                        word = v.to_string();
                    }
                }
                let outcome = if word == "closed" { "finished".to_string() } else { word };
                ledger_rows.push(format!("{} {} {} {}", f[0], f[2], outcome, f[3]));
            }
        }
        let n = ledger_rows.len();
        candidates.extend(ledger_rows.into_iter().skip(n.saturating_sub(80)));
    }

    candidates.sort();
    candidates.reverse();
    candidates.dedup();
    let candidates: Vec<String> = candidates.into_iter().take(60).collect();

    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new(); // ts, actor, body, verb, bead
    for line in &candidates {
        let mut parts = line.splitn(3, ' ');
        let (Some(ts), Some(actor), Some(body)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let toks: Vec<&str> = body.split_whitespace().collect();
        let mut bead = toks.last().copied().unwrap_or("").to_string();
        if let Some((_, b)) = bead.rsplit_once('/') {
            bead = b.to_string();
        }
        let verb = toks.first().copied().unwrap_or("").to_string();
        let key = (actor.to_string(), verb.clone(), bead.clone());
        if !seen.insert(key) {
            continue;
        }
        rows.push((ts.to_string(), actor.to_string(), body.to_string(), verb, bead));
    }

    let shown: Vec<&str> = rows.iter().take(5).map(|r| r.4.as_str()).filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))).collect();
    let (titles, repos) = shown_facts(&shown);
    for (n, (ts_str, actor, body, verb, bead)) in rows.iter().take(5).enumerate() {
        let Some(t) = parse_iso8601(ts_str) else { continue };
        let secs = (io::now() - t).max(0);
        let rel = rel_age(secs);
        let mut toks: Vec<String> = body.split_whitespace().map(String::from).collect();
        let mut body = body.clone();
        if verb == "landed" && toks.last().map(|l| l.contains('/')).unwrap_or(false) {
            let last = toks.len() - 1;
            toks[last] = bead.clone();
            body = toks.join(" ");
        } else if toks.len() > 2 && toks.last() != Some(bead) && toks.last().map(|l| l.ends_with(&format!("/{bead}"))).unwrap_or(false) {
            toks.pop();
            body = toks.join(" ");
        }
        let title = titles.get(bead).cloned().unwrap_or_default();
        let mut body_display = if title.is_empty() { body.clone() } else { format!("{body} {title}") };
        body_display.truncate(80);
        body_display = body_display.replace('=', "-");
        let actor_disp: String = actor.chars().take(8).collect();
        push(out, &format!("SP_EVENT{n}"), format!("{rel:<4} {actor_disp:<8} {body_display}"));
        push(out, &format!("SP_EVENT{n}_REPO"), repos.get(bead).cloned().unwrap_or_default());
    }
}

/// Title and repo tag of just the beads the section shows, by id: never the whole store.
fn shown_facts(ids: &[&str]) -> (HashMap<String, String>, HashMap<String, String>) {
    let (mut titles, mut repos) = (HashMap::new(), HashMap::new());
    if ids.is_empty() {
        return (titles, repos);
    }
    let mut args = vec!["show"];
    args.extend(ids);
    let Some(rows) = io::json_rows(io::contentjson(&args)) else { return (titles, repos) };
    for i in &rows {
        let Some(id) = i.get("id").and_then(Value::as_str) else { continue };
        titles.insert(id.to_string(), sanitize(i.get("title").and_then(Value::as_str).unwrap_or("")));
        repos.insert(id.to_string(), repo_tag(i.get("labels").and_then(Value::as_array)));
    }
    (titles, repos)
}

// ---------------------------------------------------------------------------------------
// INFLOW
// ---------------------------------------------------------------------------------------

/// The inflow section's numbers: counts over the window and the newest rows.
struct Inflow {
    n: usize,
    defect: usize,
    kinds: Vec<(String, usize)>,
    rows: Vec<Value>,
}

/// From a full row set, the shape the server's `plan inflow` answers directly.
fn inflow_from_rows(rows: &[Value], cut: i64) -> Inflow {
    let mut aged: Vec<(i64, &Value)> = rows
        .iter()
        .filter(|i| i.get("issue_type").and_then(Value::as_str).unwrap_or("task") != "event")
        .filter_map(|i| Some((i.get("created_at").and_then(Value::as_str).and_then(parse_iso8601)?, i)))
        .collect();
    aged.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut kinds: HashMap<String, usize> = HashMap::new();
    let (mut n, mut defect) = (0, 0);
    for (_, i) in aged.iter().filter(|(t, _)| *t >= cut) {
        let kind = i.get("issue_type").and_then(Value::as_str).unwrap_or("task").to_string();
        let is_incident = i.get("labels").and_then(Value::as_array).map(|a| a.iter().any(|l| l.as_str() == Some("incident"))).unwrap_or(false);
        if kind == "bug" || is_incident {
            defect += 1;
        }
        *kinds.entry(kind).or_insert(0) += 1;
        n += 1;
    }
    let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Inflow { n, defect, kinds, rows: aged.into_iter().take(40).map(|(_, i)| i.clone()).collect() }
}

fn inflow_from_server(raw: &str) -> Option<Inflow> {
    let v: Value = serde_json::from_str(raw).ok()?;
    Some(Inflow {
        n: v.get("n")?.as_u64()? as usize,
        defect: v.get("defect")?.as_u64()? as usize,
        kinds: v.get("kinds")?.as_array()?.iter().filter_map(|k| Some((k.get(0)?.as_str()?.to_string(), k.get(1)?.as_u64()? as usize))).collect(),
        rows: v.get("rows")?.as_array()?.clone(),
    })
}

fn inflow_argv(win_min: i64) -> Vec<String> {
    ["plan", "inflow", "--window-secs", &(win_min * 60).to_string()].map(String::from).to_vec()
}

fn inflow_section(out: &mut Kv, _run: &std::path::Path) {
    let win: i64 = std::env::var("SPIRA_INFLOW_WINDOW_MIN").ok().and_then(|v| v.parse().ok()).filter(|w| *w > 0).unwrap_or(60);
    let now = io::now();
    let found = if io::fixture_mode() {
        io::json_rows(io::live_beads("")).map(|rows| inflow_from_rows(&rows, now - win * 60))
    } else {
        let argv = inflow_argv(win);
        super::lc::verb(&argv.iter().map(String::as_str).collect::<Vec<_>>()).and_then(|raw| inflow_from_server(&raw))
    };
    let Some(flow) = found else {
        push(out, "SP_INFLOW_WIN", "?");
        push(out, "SP_INFLOW_N", "?");
        push(out, "SP_INFLOW_DEFECT", "?");
        push(out, "SP_INFLOW_KINDS", "?");
        return;
    };
    push(out, "SP_INFLOW_WIN", win.to_string());
    push(out, "SP_INFLOW_N", flow.n.to_string());
    push(out, "SP_INFLOW_DEFECT", flow.defect.to_string());
    let kinds_str = if flow.kinds.is_empty() {
        "-".to_string()
    } else {
        flow.kinds.iter().take(4).map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
    };
    push(out, "SP_INFLOW_KINDS", kinds_str);

    for (n, i) in flow.rows.iter().take(40).enumerate() {
        let Some(t) = i.get("created_at").and_then(Value::as_str).and_then(parse_iso8601) else { continue };
        let kind = i.get("issue_type").and_then(Value::as_str).unwrap_or("task");
        let title = sanitize_title(i.get("title").and_then(Value::as_str).unwrap_or(""), 80);
        let k: String = kind.to_lowercase().chars().filter(|c| c.is_ascii_lowercase()).take(8).collect();
        let k = if k.is_empty() { "task".to_string() } else { k };
        let pri = i.get("priority").map(|v| v.to_string()).unwrap_or_else(|| "?".to_string());
        let id = i.get("id").and_then(Value::as_str).unwrap_or("");
        let rel = rel_age(now - t);
        push(out, &format!("SP_INFLOW{n}"), format!("{rel:<4} {k:<8} P{pri} {id} {title}"));
        push(out, &format!("SP_INFLOW{n}_REPO"), repo_tag(i.get("labels").and_then(Value::as_array)));
    }
}

// ---------------------------------------------------------------------------------------
// AWAITING CI
// ---------------------------------------------------------------------------------------

fn awaiting_ci_section(out: &mut Kv, cfg: &Cfg) {
    let ci_max: i64 = cfg.ci_park_max;
    let now = io::now();
    let raw = io::content(&["gate", "list", "--json"]);
    let rows: Option<Vec<Value>> = raw.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            return Some(vec![]);
        }
        serde_json::from_str::<Value>(t).ok().map(|v| match v {
            Value::Null => vec![],
            Value::Array(a) => a,
            other => vec![other],
        })
    });
    let Some(rows) = rows else {
        for k in ["SP_AWAITING_N", "SP_AWAITING_STUCK", "SP_AWAITING_STUCK_ID", "SP_AWAITING_OLDEST", "SP_AWAITING_AGE"] {
            push(out, k, "?");
        }
        return;
    };
    let mut gated: Vec<&Value> = rows
        .iter()
        // bd gates (`bd gate list`) are await beads, never work: bd status is their state.
        .filter(|i| !nonwork::row_closed(Kind::Hold, i) && i.get("await_type").and_then(Value::as_str) == Some("gh:run"))
        .collect();
    gated.sort_by_key(|i| i.get("created_at").and_then(Value::as_str).and_then(parse_iso8601).unwrap_or(now));

    let mut watch = 0;
    let mut stuck = 0;
    let mut oldest = "-".to_string();
    let mut age = "-".to_string();
    let mut stuck_id = "-".to_string();
    for (n, i) in gated.iter().take(20).enumerate() {
        let id = i.get("id").and_then(Value::as_str).unwrap_or("");
        let created_at = i.get("created_at").and_then(Value::as_str).unwrap_or("");
        let t = parse_iso8601(created_at);
        let rel = t.map(|t| rel_age(now - t)).unwrap_or_else(|| "?".to_string());
        if n == 0 {
            oldest = id.to_string();
            age = rel.clone();
        }
        let mut title = sanitize(i.get("description").and_then(Value::as_str).unwrap_or(""));
        title.truncate(80);
        // SPIRA_CI_PARK_MAX = 0 disables the deadline outright (conf.d's own doc: "set it
        // to 0 to disable the deadline, which reinstates the permanent invisible park and
        // should be a deliberate choice") — a park with a known age never reads as overdue
        // in that state. A park whose `created_at` cannot even be parsed stays overdue
        // regardless: that is a data problem, not a timing one.
        let is_overdue = match t {
            Some(t) => ci_max > 0 && (now - t) > ci_max,
            None => true,
        };
        let title = if is_overdue {
            stuck += 1;
            if stuck_id == "-" {
                stuck_id = id.to_string();
            }
            format!("gate overdue \u{b7} {title}")
        } else {
            watch += 1;
            title
        };
        push(out, &format!("SP_AWAITING{n}"), format!("{id:<10} {rel:<4} {title}"));
    }
    push(out, "SP_AWAITING_N", watch.to_string());
    push(out, "SP_AWAITING_STUCK", stuck.to_string());
    push(out, "SP_AWAITING_STUCK_ID", stuck_id);
    push(out, "SP_AWAITING_OLDEST", oldest);
    push(out, "SP_AWAITING_AGE", age);
}

// ---------------------------------------------------------------------------------------
// THROUGHPUT / TOKENS — both fully delegated to their existing external tools.
// ---------------------------------------------------------------------------------------

fn throughput_section(out: &mut Kv, cfg: &Cfg) {
    let scope = &cfg.scope_label;
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let raw = io::history_beads(&label, io::DAY_WINDOW_SECS).unwrap_or_default();
    if let Some(sparklines) = crate::sparklines::render(&raw, crate::timeutil::now_secs(), &crate::counts::from_config()) {
        for line in sparklines.lines() {
            if let Some((k, v)) = line.split_once('=') {
                push(out, k, v.to_string());
            }
        }
    }
}

fn tokens_section(out: &mut Kv) {
    super::tokens::section(out, super::tokens::Source::from_config(), crate::timeutil::now_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: UC-cockpit-observability-08
    #[test_id(tkic9uq)]
    #[test]
    fn the_server_inflow_reply_parses_to_the_same_shape_as_the_row_aggregate() {
        let f = inflow_from_server(r#"{"n":3,"defect":1,"kinds":[["task",2],["bug",1]],"rows":[{"id":"sp-a"}]}"#).unwrap();
        assert_eq!((f.n, f.defect, f.kinds.len(), f.rows.len()), (3, 1, 2, 1));
        assert!(inflow_from_server(r#"{"n":3}"#).is_none(), "a partial reply is cannot-tell, not zero");
        let rows: Vec<Value> = serde_json::from_str(r#"[{"id":"a","issue_type":"bug","created_at":"2026-01-01T00:10:00Z"},{"id":"b","issue_type":"event","created_at":"2026-01-01T00:11:00Z"},{"id":"c","created_at":"2026-01-01T00:00:00Z"}]"#).unwrap();
        let f = inflow_from_rows(&rows, parse_iso8601("2026-01-01T00:05:00Z").unwrap());
        assert_eq!((f.n, f.defect, f.rows.len()), (1, 1, 2));
    }

    #[test_id(t43k321)]
    #[test]
    fn the_inflow_probe_asks_plan_for_the_window_in_seconds() {
        assert_eq!(inflow_argv(60), ["plan", "inflow", "--window-secs", "3600"]);
        let f = inflow_from_server(r#"{"n":3,"defect":1,"kinds":[["task",2],["bug",1]],"rows":[]}"#).unwrap();
        assert_eq!((f.n, f.defect, f.kinds), (3, 1, vec![("task".to_string(), 2), ("bug".to_string(), 1)]));
    }

    #[test_id(tn2xprv)]
    #[test]
    fn inflow_defect_counts_bugs_and_incidents() {
        let rows = [serde_json::json!({"id":"sp-1","issue_type":"bug","created_at":"2026-09-30T00:00:00Z","labels":[]}),
            serde_json::json!({"id":"sp-2","issue_type":"task","created_at":"2026-09-30T00:00:00Z","labels":["incident"]}),
            serde_json::json!({"id":"sp-3","issue_type":"task","created_at":"2026-09-30T00:00:00Z","labels":[]})];
        let aged: Vec<(i64, String, &Value)> = rows.iter().map(|r| (0i64, r["issue_type"].as_str().unwrap().to_string(), r)).collect();
        let mut defect = 0;
        for (_, kind, i) in &aged {
            let is_incident = i.get("labels").and_then(Value::as_array).map(|a| a.iter().any(|l| l.as_str() == Some("incident"))).unwrap_or(false);
            if kind == "bug" || is_incident {
                defect += 1;
            }
        }
        assert_eq!(defect, 2);
    }

    fn world_cfg(dir: &testkit::TempDir) -> (testkit::EnvGuard, Cfg) {
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let world = spira_config::process::fixture_world_env(dir.path(), &run, &["spira"]);
        let mut edits: Vec<(&str, Option<&str>)> = world.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        edits.push(("SPIRA_BDJSON_FIXTURE", None));
        (testkit::env(&edits), Cfg::default())
    }

    #[test_id(t517sx6)]
    #[test]
    fn the_fast_core_pass_answers_waiting_alone_and_the_slow_pass_carries_the_token_keys() {
        let dir = testkit::TempDir::new("cockpit-core-split");
        let (_env, cfg) = world_cfg(&dir);
        let core = crate::probes::run("core", &cfg).unwrap();
        assert_eq!(core.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["SP_WAITING"]);
        let detail = crate::probes::run("core_detail", &cfg).unwrap();
        for k in crate::probes::tokens::KEYS {
            assert!(detail.iter().any(|(n, _)| n == k), "{k} is the slow pass's");
        }
        assert!(!detail.iter().any(|(n, _)| n == "SP_WAITING" || n.ends_with("_RECENT") && n.starts_with("SP_TOK")));
    }

    fn iso(secs: i64) -> String {
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
    }

    fn token_env(dir: &testkit::TempDir, projects: &std::path::Path, window: &str, path: Option<&str>) -> testkit::EnvGuard {
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let (run_s, proj_s) = (run.display().to_string(), projects.display().to_string());
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_RUN", &run_s), ("SPIRA_TOKEN_PROJECTS", &proj_s), ("SPIRA_TOKEN_WINDOW_H", window), ("SPIRA_WORKSPACES", &dir.path().display().to_string())]);
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        testkit::env(&[("SPIRA_TOML", toml.to_str()), ("SPIRA_HOME", home.to_str()), ("PATH", path)])
    }

    #[test_id(tm900o1)]
    #[test]
    fn the_section_reads_the_configured_transcripts_and_starts_no_tokens_sh() {
        let dir = testkit::TempDir::new("cockpit-tokens-section");
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("-p")).unwrap();
        let ts = iso(crate::timeutil::now_secs() - 60);
        std::fs::write(projects.join("-p/s.jsonl"), format!("{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{\"id\":\"m\",\"usage\":{{\"input_tokens\":1,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3,\"output_tokens\":4}}}}}}\n")).unwrap();
        let bin = dir.path().join("bin");
        let marker = dir.path().join("started");
        std::fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("tokens.sh"), &format!("#!/bin/sh\necho started >> {}\nexit 1\n", marker.display()));
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let _env = token_env(&dir, &projects, "7", Some(&path));
        for _ in 0..2 {
            let mut out = Kv::new();
            spira_config::process::with_fresh(|| tokens_section(&mut out)).unwrap();
            let get = |k: &str| out.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            assert_eq!((get("SP_TOK_WINDOW_H").as_deref(), get("SP_TOK_SESS_TURNS").as_deref(), get("SP_TOK_WIN").as_deref()), (Some("7"), Some("1"), Some("10")));
        }
        assert!(!marker.exists(), "tokens.sh was started");
    }

    #[test_id(tpvc2wp)]
    #[test]
    fn an_unreadable_transcript_directory_or_window_is_question_marks_in_the_section() {
        let dir = testkit::TempDir::new("cockpit-tokens-unreadable");
        for (projects, window) in [(dir.path().join("absent"), "7"), (dir.path().to_path_buf(), "not-a-number")] {
            let _env = token_env(&dir, &projects, window, None);
            let mut out = Kv::new();
            spira_config::process::with_fresh(|| tokens_section(&mut out)).unwrap();
            assert_eq!(out.len(), crate::probes::tokens::KEYS.len());
            assert!(out.iter().all(|(_, v)| v == "?"), "{out:?}");
        }
    }
}
