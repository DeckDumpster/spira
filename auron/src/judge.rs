//! Auron's whole judgement, as a pure function of one observation object: no clock, no
//! file, no command, no network. `now` is handed in, the sentinel log arrives as a string
//! and every threshold is a field, so each rule replays against a captured window.
//!
//! Every rule fails quiet when it cannot see enough to judge, with one exception:
//! `sentinel-unobservable`, because "I could not look" and "all clear" must never render
//! the same (law-alerts-must-be-actionable, law-absence-needs-a-positive-control).

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::classify::Firing;

fn line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})Z spira: (.*)$").unwrap())
}

fn state_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^state: (?:\w+=\S+ )?open=([0-9]+) (?:plan_)?ready=([0-9]+) in_progress=([0-9]+) aeons=([0-9]+)").unwrap()
    })
}

const SUMMONED: &str = "— summoning";
const WITHHELD: [&str; 1] = ["out of capacity"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Bool(b)) => *b as i64,
        _ => 0,
    }
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn or_str(v: Option<&Value>, fallback: &str) -> String {
    let s = text_of(v);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

pub fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if !brk {
            continue;
        }
        out.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

pub fn iso_to_epoch(s: &str) -> i64 {
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().unwrap_or(0);
    let (y, mo, d) = (n(0..4), n(5..7), n(8..10));
    let (h, mi, sec) = (n(11..13), n(14..16), n(17..19));
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 }.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2).div_euclid(5) + d - 1;
    let doe = yoe * 365 + yoe.div_euclid(4) - yoe.div_euclid(100) + doy;
    (era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + sec
}

#[derive(Debug, Clone)]
struct Pass {
    at: i64,
    ready: i64,
    aeons: i64,
    summoned: bool,
    withheld: bool,
    complete: bool,
    last: i64,
}

fn parse_passes(text: &str) -> Vec<Pass> {
    let mut passes: Vec<Pass> = Vec::new();
    for raw in splitlines(text) {
        let Some(m) = line_re().captures(raw) else { continue };
        let at = iso_to_epoch(&m[1]);
        let msg = &m[2];
        if let Some(st) = state_re().captures(msg) {
            passes.push(Pass {
                at,
                ready: st[2].parse().unwrap_or(0),
                aeons: st[4].parse().unwrap_or(0),
                summoned: false,
                withheld: false,
                complete: false,
                last: at,
            });
            continue;
        }
        let Some(cur) = passes.last_mut() else { continue };
        cur.last = at;
        if msg.contains(SUMMONED) {
            cur.summoned = true;
        }
        if WITHHELD.iter().any(|w| msg.contains(w)) {
            cur.withheld = true;
        }
        if msg.starts_with("pass complete") {
            cur.complete = true;
        }
    }
    passes
}

fn starved(p: &Pass) -> bool {
    p.ready > 0 && p.aeons == 0 && !p.summoned && !p.withheld
}

fn fmt_age(sec: i64) -> String {
    if sec < 120 {
        format!("{sec}s")
    } else if sec < 7200 {
        format!("{}m", sec.div_euclid(60))
    } else {
        format!("{:.1}h", sec as f64 / 3600.0)
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

pub fn classify(o: &Value) -> Result<Vec<Firing>, String> {
    let obj = o.as_object().ok_or("observations are not an object")?;
    let now = match obj.get("now") {
        Some(v) if !v.is_null() => int_of(Some(v)),
        _ => return Err("observations carry no `now`".into()),
    };
    let th = obj.get("thresholds").and_then(Value::as_object);
    let thv = |k: &str, d: i64| match th.and_then(|t| t.get(k)) {
        Some(v) if truthy(Some(v)) => int_of(Some(v)),
        Some(v) if !v.is_null() => 0,
        _ => d,
    };
    let pass_stale = thv("pass_stale", 600);
    let starve_passes = thv("starve_passes", 5);
    let mirror_stale = thv("mirror_stale", 90000);
    let ghost_stale = thv("ghost_stale", 1800);
    let restart_threshold = thv("restart_threshold", 5);
    let mut out: Vec<Firing> = Vec::new();
    let mut alert = |key: &str, title: String, evidence: String| {
        out.push(Firing { key: key.to_string(), title, evidence: evidence.trim().to_string() });
    };

    let world_halted = truthy(obj.get("world_halted"));
    let world_draining = truthy(obj.get("world_draining"));
    let log_text = text_of(obj.get("sentinel_log"));
    let log_path = or_str(obj.get("sentinel_log_path"), "the sentinel log");

    if !truthy(obj.get("sentinel_log_readable")) {
        alert(
            "sentinel-unobservable",
            "Auron cannot read the sentinel log — the loop's liveness is unknown".into(),
            format!(
                "PATH      {}\nREASON    {}\n\n\
                 Nothing below this line was checked. An unreadable log is not a quiet\n\
                 one: every other reading Auron takes about the loop is derived from it,\n\
                 so a healthy pane here would be a guess wearing a measurement's clothes.",
                log_path,
                or_str(obj.get("sentinel_log_error"), "unreadable")
            ),
        );
    } else {
        let passes = parse_passes(&log_text);
        let done = passes.iter().rev().find(|p| p.complete).map(|p| p.last);
        let floor = int_of(obj.get("auron_first")).max(passes.first().map_or(0, |p| p.at));
        let since = done.unwrap_or(floor);
        let age = now - since;
        if since != 0 && age > pass_stale && !world_halted && !world_draining {
            let timer = or_str(obj.get("sentinel_timer"), "unknown");
            let mtime = int_of(obj.get("sentinel_log_mtime"));
            let quiet = if mtime != 0 { now - mtime } else { -1 };
            let quiet_s = if quiet >= 0 { fmt_age(quiet) } else { "an unknown time".to_string() };
            let shape = if quiet >= 0 && quiet < pass_stale.div_euclid(2) {
                format!("the log is still being WRITTEN ({} ago) but no pass has FINISHED — a pass is wedged mid-flight", fmt_age(quiet))
            } else if timer != "active" {
                format!("spira-sentinel.timer is {timer} — the loop is not being run at all")
            } else {
                format!("nothing has been written to the log in {quiet_s} either — the pass is not running, or is dying before it can log")
            };
            let lines = splitlines(&log_text);
            let tail = lines[lines.len().saturating_sub(12)..].join("\n");
            alert(
                "sentinel-stalled",
                format!("The Spira sentinel has not completed a pass in {}", fmt_age(age)),
                format!(
                    "LAST PASS {} ago{}\n\
                     THRESHOLD {}s (ordinary pass ~22s, worst gap ever measured here 436s)\n\
                     TIMER     spira-sentinel.timer is {}\n\
                     LOG       {}, last written {} ago\n\
                     SHAPE     {}\n\n\
                     Nothing is being claimed, landed, reaped or reclaimed while this holds.\n\
                     Aeons already running are unaffected and will finish.\n\n\
                     --- sentinel.log (tail) ---\n{}",
                    fmt_age(age),
                    if done.is_some() { "" } else { " (no completed pass in the tail at all)" },
                    pass_stale,
                    timer,
                    log_path,
                    if quiet >= 0 { fmt_age(quiet) } else { "never".to_string() },
                    shape,
                    tail
                ),
            );
        }

        let finished: Vec<&Pass> = passes.iter().filter(|p| p.complete).collect();
        if starve_passes >= 0 && finished.len() as i64 >= starve_passes {
            let window = &finished[finished.len() - starve_passes as usize..];
            if let (Some(first), Some(last)) = (window.first(), window.last()) {
                let fresh = now - last.at <= pass_stale;
                if fresh && window.iter().all(|p| starved(p)) {
                    let rows: Vec<String> = window
                        .iter()
                        .map(|p| format!("  {}s ago  ready={:<3} aeons={}  summoned=no", now - p.at, p.ready, p.aeons))
                        .collect();
                    alert(
                        "summon-starved",
                        format!("{} ready beads, no aeon running, and {} passes in a row summoned nothing", last.ready, starve_passes),
                        format!(
                            "PASSES    {} consecutive, spanning {}\n\
                             READY     {} claimable beads at the last pass\n\
                             AEONS     0 — every slot is free, so this is not the concurrency cap\n\
                             WITHHELD  no — the account's window did not decline\n\
                             NOISE     2 passes of 962 matched this rule over 33 hours, in one streak of 2; the threshold is {}\n\n\
                             The loop is running and is refusing to start work it has capacity for.\n\
                             The last time this happened it ran for 76 passes before a human saw it.\n\n\
                             --- the passes ---\n{}",
                            starve_passes,
                            fmt_age(now - first.at),
                            last.ready,
                            starve_passes,
                            rows.join("\n")
                        ),
                    );
                }
            }
        }
    }

    if obj.get("db_reachable").is_some_and(|v| !truthy(Some(v))) {
        alert(
            "db-unreachable",
            "The Spira beads database did not answer".into(),
            format!(
                "DB        {}\nERROR     {}\n\n\
                 Every claim, close, lease and label goes through this. Aeons already\n\
                 running will fail their next bd call; the sentinel's passes will complete\n\
                 having done nothing. This alert is in {} because it could not be written\n\
                 to beads — which is the condition it is reporting.",
                or_str(obj.get("db_path"), "?"),
                or_str(obj.get("db_error"), "no answer"),
                or_str(obj.get("fallback_path"), "the fallback file")
            ),
        );
    }

    let mir = obj.get("mirror").and_then(Value::as_object);
    let mg = |k: &str| mir.and_then(|m| m.get(k));
    if truthy(mg("configured")) {
        let path = or_str(mg("path"), "?");
        let exporter = or_str(mg("exporter"), "?");
        if !truthy(mg("exists")) {
            alert(
                "mirror-stale",
                "The beads exporter is configured but has written no mirror".into(),
                format!(
                    "EXPORTER  {exporter}\nEXPECTED  {path}\n\n\
                     The mirror is the copy of the database that reaches git, review and\n\
                     the phone. Dolt's own remote is the other half and fails independently;\n\
                     with neither, a database loss is unrecoverable and silent."
                ),
            );
        } else {
            let age = now - int_of(mg("mtime"));
            if age > mirror_stale {
                alert(
                    "mirror-stale",
                    format!("The beads mirror has not been rewritten in {}", fmt_age(age)),
                    format!(
                        "PATH      {}\nWRITTEN   {} ago\nTHRESHOLD {}\nEXPORTER  {}\n\n\
                         The exporter runs on a timer and rewrites this every pass in which\n\
                         the database changed. Silence for longer than a day means the timer,\n\
                         the exporter or its refusal check is broken, not that nothing changed.",
                        path,
                        fmt_age(age),
                        fmt_age(mirror_stale),
                        exporter
                    ),
                );
            }
        }
    }

    let mut ghosts: Vec<(String, i64, bool, bool)> = Vec::new();
    if let Some(strands) = obj.get("strands").and_then(Value::as_object) {
        let mut keys: Vec<&String> = strands.keys().collect();
        keys.sort();
        for key in keys {
            let Some(id) = key.strip_prefix("ghost:") else { continue };
            let e = strands.get(key);
            let ev = e.and_then(Value::as_object);
            let g = |k: &str| int_of(ev.and_then(|m| m.get(k)));
            let first = g("first");
            if first != 0 && now - first > ghost_stale {
                ghosts.push((id.to_string(), now - first, g("acted") != 0, g("escalated") != 0));
            }
        }
    }
    if !ghosts.is_empty() {
        let rows: Vec<String> = ghosts
            .iter()
            .map(|(i, a, ac, es)| format!("  {:<22} stranded {} ago, acted={} escalated={}", i, fmt_age(*a), yes_no(*ac), yes_no(*es)))
            .collect();
        alert(
            "lease-unreclaimed",
            format!("{} bead(s) have held a lease with no live aeon for over {}", ghosts.len(), fmt_age(ghost_stale)),
            format!(
                "THRESHOLD {} — strand reclaims a ghost within one pass and escalates once\n\
                 if it cannot, so a ghost still standing after this is the REAPER failing,\n\
                 not a worker dying.\n\n\
                 Each of these beads reads as in_progress, so nothing else will claim it and\n\
                 everything downstream of it stays blocked.\n\n\
                 --- ghosts ---\n{}",
                fmt_age(ghost_stale),
                rows.join("\n")
            ),
        );
    }

    if let Some(ras) = obj.get("restart_alerts").and_then(Value::as_array) {
        for ra in ras {
            let g = |k: &str| ra.as_object().and_then(|m| m.get(k));
            let unit = or_str(g("unit"), "?");
            let delta = int_of(g("delta"));
            if delta <= restart_threshold {
                continue;
            }
            let current = int_of(g("current"));
            let window = if truthy(g("window")) { int_of(g("window")) } else { 3600 };
            alert(
                &format!("restart-loop:{unit}"),
                format!("{unit} restarted {delta} time(s) in the last {}", fmt_age(window)),
                format!(
                    "UNIT      {unit}\n\
                     RESTARTS  {delta} in this {} window (running total {current}, threshold {restart_threshold})\n\n\
                     A Restart=always unit never reaches 'failed' — incident intake misses it.\n\
                     This is Auron's notification of an active restart loop.\n\n\
                     Read the unit's own log with: systemctl --user status {unit}\n",
                    fmt_age(window)
                ),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn epoch_matches_the_known_value() {
        assert_eq!(iso_to_epoch("2026-09-06T18:26:32"), 1788719192);
        assert_eq!(iso_to_epoch("1970-01-01T00:00:01"), 1);
    }

    #[test]
    fn an_unreadable_log_alerts_and_a_halted_stale_one_does_not() {
        let f = classify(&json!({"now": 1800000000, "sentinel_log_readable": false})).unwrap();
        assert_eq!(f.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["sentinel-unobservable"]);
        let log = "2026-09-06T18:00:00Z spira: state: open=1 ready=0 in_progress=0 aeons=0\n2026-09-06T18:00:20Z spira: pass complete\n";
        let o = json!({"now": 1800000000, "sentinel_log_readable": true, "sentinel_log": log, "world_halted": true});
        assert!(classify(&o).unwrap().is_empty());
        let o = json!({"now": 1800000000, "sentinel_log_readable": true, "sentinel_log": log});
        assert_eq!(classify(&o).unwrap()[0].key, "sentinel-stalled");
    }

    #[test]
    fn a_missing_now_is_an_error() {
        assert!(classify(&json!({})).is_err());
    }

    #[test]
    fn splitlines_matches_the_python_definition() {
        assert_eq!(splitlines("a\r\nb\rc\n"), ["a", "b", "c"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
    }
}
