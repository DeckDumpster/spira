//! `ratelim_keys` — the account's two rate-limit windows, read from the newest
//! `rate_limit_event` across live aeon traces, plus an ETA extrapolated from the collector's
//! own history CSV. Ported from the `python3 /dev/fd/3` heredoc in `spira/cockpit.sh`
//! (DESIGN.md "Design" — native `serde_json` in place of the embedded interpreter).

use super::{push, Kv};
use crate::io;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use serde_json::Value;

const TAIL: u64 = 131072;
const KEYS: &[&str] = &[
    "SP_RATELIM_5H", "SP_RATELIM_7D", "SP_RATELIM_5H_PCT", "SP_RATELIM_7D_PCT",
    "SP_RATELIM_5H_MIN", "SP_RATELIM_7D_MIN", "SP_RATELIM_5H_ETA", "SP_RATELIM_7D_ETA",
    "SP_RATELIM_AGE",
];

fn all_question_marks() -> Kv {
    KEYS.iter().map(|k| (k.to_string(), "?".to_string())).collect()
}

struct Window {
    util: Option<f64>,
    reset: Option<i64>,
}

fn win_vals(w: &Value, now: i64) -> Window {
    let u = w.get("utilization").and_then(|v| if v.is_boolean() { None } else { v.as_f64() });
    let r = w.get("resetsAt").and_then(|v| if v.is_boolean() { None } else { v.as_f64() });
    match (u, r) {
        (Some(u), Some(r)) if (0.0..=1.0).contains(&u) && (r as i64) > now => Window { util: Some(u), reset: Some(r as i64) },
        _ => Window { util: None, reset: None },
    }
}

fn tail_file(path: &Path, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn ratelim_keys() -> Kv {
    let run = io::run_dir();
    let now = io::now();

    let mut logs: Vec<(std::path::PathBuf, i64)> = match std::fs::read_dir(&run) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("log") {
                    return None;
                }
                let mtime = std::fs::metadata(&p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
                Some((p, mtime))
            })
            .collect(),
        Err(_) => return all_question_marks(),
    };
    logs.sort_by(|a, b| b.1.cmp(&a.1));

    let mut five_h: Option<Value> = None;
    let mut seven_d: Option<Value> = None;
    let mut file_mtime: Option<i64> = None;

    'outer: for (path, mtime) in logs.iter().take(30) {
        let Some(chunk) = tail_file(path, TAIL) else { continue };
        let mut last: Option<(Option<Value>, Option<Value>)> = None;
        for line in chunk.lines() {
            if !line.contains("\"rate_limit_event\"") {
                continue;
            }
            let Ok(d) = serde_json::from_str::<Value>(line) else { continue };
            if d.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
                continue;
            }
            let uw = d.get("rate_limit_info").and_then(|r| r.get("unifiedWindows"));
            let Some(uw) = uw else { continue };
            let fh = uw.get("five_hour").cloned();
            let sd = uw.get("seven_day").cloned();
            if fh.is_some() || sd.is_some() {
                last = Some((fh, sd));
            }
        }
        if let Some((fh, sd)) = last {
            five_h = fh;
            seven_d = sd;
            file_mtime = Some(*mtime);
            break 'outer;
        }
    }

    if five_h.is_none() && seven_d.is_none() {
        return all_question_marks();
    }

    let w5 = five_h.as_ref().map(|w| win_vals(w, now)).unwrap_or(Window { util: None, reset: None });
    let w7 = seven_d.as_ref().map(|w| win_vals(w, now)).unwrap_or(Window { util: None, reset: None });

    let mins_to = |epoch: Option<i64>| epoch.map(|e| ((e - now) / 60).max(0).to_string()).unwrap_or_else(|| "?".to_string());

    let hist = run.join("cockpit-history.csv");
    let eta_from_history = |col_name: &str, util: Option<f64>, reset: Option<i64>| -> String {
        let Some(util) = util else { return "?".to_string() };
        if util >= 1.0 {
            return "0".to_string();
        }
        let Ok(content) = std::fs::read_to_string(&hist) else { return "?".to_string() };
        let mut lines = content.lines();
        let Some(header) = lines.next() else { return "?".to_string() };
        let cols: Vec<&str> = header.split(',').collect();
        let Some(col_idx) = cols.iter().position(|c| *c == col_name) else { return "?".to_string() };
        let hour_ago = now - 3600;
        let mut rows: Vec<(i64, f64)> = Vec::new();
        for line in lines {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() <= col_idx {
                continue;
            }
            let Ok(ts) = parts[0].parse::<i64>() else { continue };
            let val_s = parts[col_idx];
            if val_s == "?" || val_s == "-" || val_s.is_empty() {
                continue;
            }
            let Ok(val) = val_s.parse::<f64>() else { continue };
            if ts >= hour_ago {
                rows.push((ts, val));
            }
        }
        if rows.len() < 2 {
            return "?".to_string();
        }
        rows.sort_by_key(|r| r.0);
        let span = rows[rows.len() - 1].0 - rows[0].0;
        let mv = rows[rows.len() - 1].1 - rows[0].1;
        if span < 600 || mv < 0.02 {
            return "?".to_string();
        }
        let slope = mv / span as f64;
        if slope <= 0.0 {
            return "?".to_string();
        }
        let eta_secs = ((1.0 - util) / slope) as i64;
        if let Some(reset_epoch) = reset {
            let secs_to_reset = (reset_epoch - now).max(0);
            if secs_to_reset < eta_secs {
                return "-".to_string();
            }
        }
        eta_secs.to_string()
    };

    let eta_5h = eta_from_history("ratelim_5h", w5.util, w5.reset);
    let eta_7d = eta_from_history("ratelim_7d", w7.util, w7.reset);

    let fmt_util = |u: Option<f64>| u.map(|v| format!("{v:.3}")).unwrap_or_else(|| "?".to_string());
    let fmt_pct = |u: Option<f64>| u.map(|v| (v * 100.0).round().to_string()).unwrap_or_else(|| "?".to_string());

    let mut out = Kv::new();
    push(&mut out, "SP_RATELIM_5H", fmt_util(w5.util));
    push(&mut out, "SP_RATELIM_7D", fmt_util(w7.util));
    push(&mut out, "SP_RATELIM_5H_PCT", fmt_pct(w5.util));
    push(&mut out, "SP_RATELIM_7D_PCT", fmt_pct(w7.util));
    push(&mut out, "SP_RATELIM_5H_MIN", mins_to(w5.reset));
    push(&mut out, "SP_RATELIM_7D_MIN", mins_to(w7.reset));
    push(&mut out, "SP_RATELIM_5H_ETA", eta_5h);
    push(&mut out, "SP_RATELIM_7D_ETA", eta_7d);
    push(&mut out, "SP_RATELIM_AGE", file_mtime.map(|m| (now - m).to_string()).unwrap_or_else(|| "?".to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_vals_rejects_bool_and_out_of_range() {
        let w = win_vals(&serde_json::json!({"utilization": true, "resetsAt": 100}), 50);
        assert!(w.util.is_none());
        let w = win_vals(&serde_json::json!({"utilization": 1.5, "resetsAt": 100}), 50);
        assert!(w.util.is_none());
        let w = win_vals(&serde_json::json!({"utilization": 0.42, "resetsAt": 100}), 50);
        assert_eq!(w.util, Some(0.42));
        assert_eq!(w.reset, Some(100));
    }

    #[test]
    fn win_vals_rejects_a_window_that_already_reset() {
        let w = win_vals(&serde_json::json!({"utilization": 0.96, "resetsAt": 100}), 100);
        assert!(w.util.is_none() && w.reset.is_none());
        let w = win_vals(&serde_json::json!({"utilization": 0.96, "resetsAt": 100}), 500);
        assert!(w.util.is_none());
    }

    #[test]
    fn no_logs_renders_all_question_marks() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let run = testkit::TempDir::new("cc-ratelim-empty");
        std::env::set_var("SPIRA_RUN", run.path());
        let kv = ratelim_keys();
        assert!(kv.iter().all(|(_, v)| v == "?"));
        assert_eq!(kv.len(), KEYS.len());
    }
}
