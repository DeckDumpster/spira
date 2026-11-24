//! The context meter: how much context a session carries, how fast it grows, how far the
//! archivist has got, and how full the account's two rate-limit windows are. One refresh is one
//! process that starts no other (DESIGN.md). The core takes `now`, paths and thresholds.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

pub mod files;
pub mod fmt;
pub mod limits;
pub mod transcript;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde_json::Value;

use files::read_kv;
use limits::{Sample, Win};

/// Seconds the collector's snapshot may be old before the in-flight segment says `?`.
const INFLIGHT_STALE_SECS: i64 = 60;

#[derive(Clone)]
pub struct Config {
    pub warn: i64,
    pub high: i64,
    pub limit: i64,
    pub projects: PathBuf,
    pub run: PathBuf,
    /// Seconds before a pointer's holder, or the pointer itself, counts as gone.
    pub idle: i64,
    pub hhmm: fn(i64) -> String,
}

impl Config {
    /// Every key from `get`; a missing or unparsable one is a refusal naming it.
    pub fn from_lookup(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Config, String> {
        let num = |k: &str| -> Result<i64, String> { spira_config::process::parse_declared(k, &get(k)?) };
        Ok(Config {
            warn: num("SPIRA_CTX_WARN")?,
            high: num("SPIRA_CTX_HIGH")?,
            limit: num("SPIRA_CTX_LIMIT")?,
            projects: get("SPIRA_TOKEN_PROJECTS")?.into(),
            run: get("SPIRA_RUN")?.into(),
            idle: num("SPIRA_ARCHIVIST_IDLE")?,
            hhmm: fmt::local_hhmm,
        })
    }

    pub fn resolve() -> Result<Config, String> {
        Config::from_lookup(&|k| spira_config::process::cfg(k))
    }
}

struct Archivist {
    name: String,
    behind: i64,
    filed: String,
    retry_at: i64,
    sweep_skipped: bool,
}

fn int_of(v: Option<&String>) -> Result<i64, ()> {
    match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(0),
        Some(s) => s.parse().map_err(|_| ()),
    }
}

/// The archivist's state for one session. No file is a real state (`none`), an unreadable one is `?`.
fn archivist(run: &Path, key: &str, turns: i64) -> Archivist {
    let mut a = Archivist { name: String::new(), behind: 0, filed: "0".into(), retry_at: 0, sweep_skipped: false };
    match read_kv(&run.join("archivist").join(format!("{key}.state")), true) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => a.name = "none".into(),
        Err(_) => a.name = "?".into(),
        Ok(st) => {
            a.name = st.get("state").cloned().unwrap_or_default();
            a.filed = st.get("items_filed").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "0".into());
            let parsed = int_of(st.get("retry_at")).and_then(|r| int_of(st.get("at_turn")).map(|t| (r, t)));
            match parsed {
                Ok((r, t)) => {
                    a.retry_at = r;
                    a.behind = (turns - t).max(0);
                }
                Err(()) => a.name = "?".into(),
            }
        }
    }
    if a.name == "none" {
        if let Ok(sw) = read_kv(&run.join("archivist/sweep.state"), true) {
            a.sweep_skipped = sw.get("sweep_state").is_some_and(|s| s == "skipped") && sw.get("reason").is_some_and(|s| s == "capacity");
        }
    }
    a
}

fn stem(tp: &str) -> String {
    let base = Path::new(tp).file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
    base.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(base)
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn hook_win(v: Option<&Value>) -> Option<Win> {
    let o = v?.as_object()?;
    let (p, r) = (number(o.get("used_percentage"))?, number(o.get("resets_at"))?);
    (0.0..=100.0).contains(&p).then_some(Win { pct: p, resets_at: r as i64 })
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn next_band(ctx: i64, c: &Config) -> (&'static str, i64) {
    [("warn", c.warn), ("high", c.high), ("limit", c.limit)].into_iter().find(|(_, at)| ctx < *at).map(|(n, at)| (n, at - ctx)).unwrap_or(("over", 0))
}

fn growth(tail: &[i64]) -> i64 {
    if tail.len() >= 21 {
        ((tail[tail.len() - 1] - tail[0]) / 20).max(0)
    } else {
        0
    }
}

/// `SP_CTX_*` and `SP_LIMIT_*` lines for `transcript`, or for the operator's session named by the
/// pointer when none is named. Reads no stdin.
pub fn env(cfg: &Config, now: i64, transcript: Option<&Path>) -> Vec<String> {
    let run = cfg.run.as_path();
    let mut ptr_age: i64 = -1;
    let tp: String = match transcript.filter(|t| !t.as_os_str().is_empty()) {
        Some(t) => t.display().to_string(),
        None => match transcript::read_pointer(run) {
            Some((ptp, ts)) => {
                ptr_age = (now - ts).max(0);
                if !ptp.is_empty() && Path::new(&ptp).exists() && ptr_age <= cfg.idle {
                    ptp
                } else {
                    String::new()
                }
            }
            None => String::new(),
        },
    };
    let s = if tp.is_empty() { transcript::Scan { broken: true, ..Default::default() } } else { transcript::scan(run, &tp) };
    let ctx = s.tail.last().copied();
    let (samples, lim_broken) = limits::load(&run.join("limits.samples"));
    let limit_lines = limits::env_lines(&samples, lim_broken, now);
    let mut out = Vec::new();
    let Some(ctx) = ctx else {
        for k in ["NOW", "TURNS", "GROWTH", "NEXT", "HEADROOM", "TURNS_LEFT"] {
            out.push(format!("SP_CTX_{k}=-"));
        }
        out.push(format!("SP_CTX_AGE={}", if ptr_age >= 0 { ptr_age.to_string() } else { "-".into() }));
        out.extend(["SP_CTX_ARCHIVIST=-", "SP_CTX_ARCHIVIST_BEHIND=-", "SP_CTX_ARCHIVIST_FILED=-"].map(String::from));
        out.push(format!("SP_CTX_SCAN_BYTES={}", if tp.is_empty() { 0 } else { s.scanned }));
        out.extend(limit_lines);
        return out;
    };
    let arc = archivist(run, &stem(&tp), s.turns);
    let (next, head) = next_band(ctx, cfg);
    let g = growth(&s.tail);
    let age = if ptr_age >= 0 {
        ptr_age
    } else {
        transcript_mtime_secs(&tp).map(|m| now - m).unwrap_or(-1)
    };
    out.push(format!("SP_CTX_NOW={ctx}"));
    out.push(format!("SP_CTX_TURNS={}", s.turns));
    out.push(format!("SP_CTX_GROWTH={g}"));
    out.push(format!("SP_CTX_NEXT={next}"));
    out.push(format!("SP_CTX_HEADROOM={head}"));
    out.push(format!("SP_CTX_TURNS_LEFT={}", if g > 0 { (head / g).to_string() } else { "-".into() }));
    out.push(format!("SP_CTX_AGE={age}"));
    let name = if arc.sweep_skipped { "skipped" } else if arc.name.is_empty() { "-" } else { arc.name.as_str() };
    out.push(format!("SP_CTX_ARCHIVIST={name}"));
    out.push(format!("SP_CTX_ARCHIVIST_BEHIND={}", arc.behind));
    out.push(format!("SP_CTX_ARCHIVIST_FILED={}", arc.filed));
    out.push(format!("SP_CTX_SCAN_BYTES={}", s.scanned));
    out.extend(limit_lines);
    out
}

fn transcript_mtime_secs(tp: &str) -> Option<i64> {
    let d = std::fs::metadata(tp).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() as i64)
}

fn in_flight(run: &Path, now: i64) -> String {
    const BAD: &str = " \x1b[31m\u{2699} ?inflight\x1b[0m";
    let snap = run.join("cockpit.env");
    let Some(m) = std::fs::metadata(&snap).ok().and_then(|m| m.modified().ok()).and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()) else { return BAD.into() };
    if now as f64 - m.as_secs_f64() > INFLIGHT_STALE_SECS as f64 {
        return BAD.into();
    }
    let Ok(bytes) = std::fs::read(&snap) else { return BAD.into() };
    for line in String::from_utf8_lossy(&bytes).lines() {
        if let Some(v) = line.strip_prefix("SP_INFLIGHT_SEG=") {
            let v = if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') { &v[1..v.len() - 1] } else { v };
            if v.is_empty() {
                return BAD.into();
            }
            return format!(" {}{v}\x1b[0m", if v.contains('?') { "\x1b[31m" } else { "\x1b[2m" });
        }
    }
    BAD.into()
}

fn archivist_segment(a: &Archivist, ctx: i64, key: &str, cfg: &Config) -> String {
    match a.name.as_str() {
        "sweeping" => " \x1b[36m⟳ sweeping\x1b[0m".into(),
        "archiving" => " \x1b[36m⇣ archiving\x1b[0m".into(),
        "safe" if a.behind <= 2 => format!(" \x1b[32m✓ safe to clear\x1b[0m \x1b[2m({} filed)\x1b[0m", a.filed),
        "safe" => format!(" \x1b[33m✓ safe as of {}t ago\x1b[0m", a.behind),
        n @ ("failed" | "retry") if a.retry_at != 0 => {
            format!(" \x1b[33m⏸ archive {}, will retry at {}\x1b[0m", if n == "failed" { "failed" } else { "deferred" }, (cfg.hhmm)(a.retry_at))
        }
        "failed" => format!(" \x1b[31m! archive failed for good (run: archivist now {key})\x1b[0m"),
        "timeout" => " \x1b[33m⏸ archive timed out, retrying\x1b[0m".into(),
        "capacity" => " \x1b[33m⏸ archive deferred (capacity)\x1b[0m".into(),
        "none" if a.sweep_skipped => " \x1b[33m⏸ skipped (capacity)\x1b[0m".into(),
        "none" if ctx >= cfg.warn => " \x1b[2m· not archived\x1b[0m".into(),
        "none" => String::new(),
        "?" => " \x1b[31m· archivist ?\x1b[0m".into(),
        n => format!(" \x1b[2m{n}\x1b[0m"),
    }
}

/// The one status line for the status-line hook JSON in `stdin`; `cwd` is where the client ran
/// when the hook names none. Persists the operator pointer and the limits sample as it goes.
pub fn line(cfg: &Config, now: i64, stdin: &str, cwd: &Path) -> String {
    let run = cfg.run.as_path();
    let hook: Value = serde_json::from_str(stdin).ok().filter(Value::is_object).unwrap_or(Value::Null);
    let (sid, hp) = (str_of(&hook, "session_id"), str_of(&hook, "transcript_path"));
    let named = !sid.is_empty() || !hp.is_empty();
    let cwd_s = hook
        .get("workspace")
        .map(|w| str_of(w, "current_dir"))
        .filter(|s| !s.is_empty())
        .or_else(|| Some(str_of(&hook, "cwd")).filter(|s| !s.is_empty()))
        .map(String::from)
        .unwrap_or_else(|| cwd.display().to_string());
    let tp = transcript::resolve_line(&cfg.projects, sid, hp, &cwd_s);
    if !tp.is_empty() {
        transcript::write_pointer(run, &tp, sid, now, cfg.idle);
    }
    let s = if tp.is_empty() { transcript::Scan { broken: !named, ..Default::default() } } else { transcript::scan(run, &tp) };
    let supplied = hook.get("context_window").filter(|cw| cw.get("current_usage").is_some_and(|u| !u.is_null())).and_then(|cw| number(cw.get("total_input_tokens"))).map(|v| v as i64);
    let ctx = supplied.or_else(|| s.tail.last().copied());

    let lim_path = run.join("limits.samples");
    let (mut samples, mut lim_broken) = limits::load(&lim_path);
    let rl = hook.get("rate_limits").filter(|r| r.is_object());
    let now_sample = rl.map(|rl| Sample { at: now, five: hook_win(rl.get("five_hour")), seven: hook_win(rl.get("seven_day")) }).filter(|s| s.five.is_some() || s.seven.is_some());
    if let Some(ns) = now_sample {
        samples = limits::record(&lim_path, if lim_broken { &[] } else { &samples }, ns);
        lim_broken = false;
    }
    let _ = lim_broken;
    let lim_seg = now_sample.map(|ns| limits::segment(&ns, &samples, now)).unwrap_or_default();

    let Some(ctx) = ctx else {
        let head = if named && !s.broken { "\x1b[2m· ctx fresh\x1b[0m" } else { "ctx ?" };
        return format!("{head}{lim_seg}{}", in_flight(run, now));
    };
    let key = if sid.is_empty() { stem(&tp) } else { sid.to_string() };
    let arc = archivist(run, &key, s.turns);
    let g = growth(&s.tail);
    let hue = if ctx < cfg.warn { "\x1b[32m" } else if ctx < cfg.high { "\x1b[33m" } else { "\x1b[31m" };
    const BLK: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let lvl = if cfg.limit > 0 { ((ctx as f64 / cfg.limit as f64 * 8.0) as usize).min(7) } else { 0 };
    let k = format!("{:.0}k", ctx as f64 / 1000.0);
    let grown = if g > 0 { format!(" +{:.1}k/turn", g as f64 / 1000.0) } else { String::new() };
    let turned = if s.turns > 0 { format!("/{}t", s.turns) } else { String::new() };
    let arc = archivist_segment(&arc, ctx, &key, cfg);
    format!("{hue}{} ctx {k}\x1b[0m \x1b[2m{turned}{grown}\x1b[0m{arc}{lim_seg}{}", BLK[lvl], in_flight(run, now))
}
