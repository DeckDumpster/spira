//! Refused bead events, grouped by (actor, event, from_state, refusal); one filing per class
//! over its baseline, repeated at most once per repeat window.

use serde_json::Value;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub actor: String,
    pub event: String,
    pub from_state: String,
    pub refusal: String,
    pub n: u64,
    pub first_at: i64,
    pub last_at: i64,
    pub examples: Vec<(String, i64)>,
}

impl Class {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.actor, self.event, self.from_state, self.refusal
        )
    }

    pub fn title(&self) -> String {
        format!(
            "refused {} from {} ({}) by {}",
            self.event, self.from_state, self.refusal, self.actor
        )
    }
}

pub struct Config {
    pub window_secs: u64,
    pub baseline: u64,
    pub repeat_secs: u64,
    pub max_files: usize,
}

fn num(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn text(row: &Value, k: &str) -> String {
    row.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn examples(raw: &str) -> Vec<(String, i64)> {
    raw.split(',')
        .filter_map(|e| e.rsplit_once('@'))
        .filter_map(|(b, at)| Some((b.to_string(), at.parse().ok()?)))
        .collect()
}

/// `spira-lc ops-refusals` output: the clock it was measured against and the classes.
pub fn parse(json: &str) -> Result<(i64, Vec<Class>), String> {
    let doc: Value = serde_json::from_str(json.trim()).map_err(|e| format!("not JSON: {e}"))?;
    let now = doc
        .get("now")
        .and_then(num)
        .ok_or("no clock in the answer")?;
    let rows = doc
        .get("classes")
        .and_then(Value::as_array)
        .ok_or("no classes in the answer")?;
    let mut out = Vec::new();
    for r in rows {
        let n = r.get("n").and_then(num).ok_or("a class has no count")?;
        out.push(Class {
            actor: text(r, "actor"),
            event: text(r, "event"),
            from_state: text(r, "from_state"),
            refusal: text(r, "refusal"),
            n: u64::try_from(n).map_err(|_| "a class has a negative count")?,
            first_at: r.get("first_at").and_then(num).unwrap_or(0),
            last_at: r.get("last_at").and_then(num).unwrap_or(0),
            examples: examples(&text(r, "examples")),
        });
    }
    Ok((now, out))
}

pub fn parse_state(raw: &str) -> BTreeMap<String, i64> {
    raw.lines()
        .filter_map(|l| l.rsplit_once('\t'))
        .filter_map(|(k, t)| Some((k.to_string(), t.parse().ok()?)))
        .collect()
}

pub fn render_state(state: &BTreeMap<String, i64>) -> String {
    state.iter().fold(String::new(), |mut out, (k, t)| {
        out.push_str(&format!("{k}\t{t}\n"));
        out
    })
}

/// Classes over baseline that were not filed within the repeat window, worst first.
pub fn due<'a>(
    classes: &'a [Class],
    cfg: &Config,
    state: &BTreeMap<String, i64>,
    now: i64,
) -> Vec<&'a Class> {
    let mut d: Vec<&Class> = classes
        .iter()
        .filter(|c| c.n > cfg.baseline)
        .filter(|c| {
            state
                .get(&c.key())
                .is_none_or(|t| now - t >= cfg.repeat_secs as i64)
        })
        .collect();
    d.sort_by(|a, b| b.n.cmp(&a.n).then_with(|| a.key().cmp(&b.key())));
    d
}

fn iso(epoch: i64) -> String {
    let (days, rem) = (epoch.div_euclid(86400), epoch.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn body(c: &Class, cfg: &Config) -> String {
    let mut b = format!(
        "{} refused `{}` events from state {} with refusal `{}` in the last {}s (baseline {}).\n\nCaller: {}\nFirst: {}\nLast: {}\n\nExamples (newest first):\n",
        c.n, c.event, c.from_state, c.refusal, cfg.window_secs, cfg.baseline, c.actor, iso(c.first_at), iso(c.last_at)
    );
    for (bead, at) in &c.examples {
        b.push_str(&format!("- {bead} at {}\n", iso(*at)));
    }
    b.push_str("\nFind why the caller repeats a move the machine refuses and fix the caller or the machine; `spira-lc ops-bead <bead>` shows the whole timeline.\n");
    b
}

pub struct Outcome {
    pub filed: Vec<String>,
    pub failed: Vec<String>,
    pub deferred: usize,
    pub state: BTreeMap<String, i64>,
}

/// Files at most `max_files` due classes through `file(title, body)`. A class is recorded as
/// filed only when the filing succeeded, so a failure is retried on the next pass.
pub fn pass(
    classes: &[Class],
    now: i64,
    cfg: &Config,
    mut state: BTreeMap<String, i64>,
    file: &mut dyn FnMut(&str, &str) -> Result<(), String>,
) -> Outcome {
    let due = due(classes, cfg, &state, now);
    let deferred = due.len().saturating_sub(cfg.max_files);
    let (mut filed, mut failed) = (Vec::new(), Vec::new());
    for c in due.into_iter().take(cfg.max_files) {
        match file(&c.title(), &body(c, cfg)) {
            Ok(()) => {
                state.insert(c.key(), now);
                filed.push(c.title());
            }
            Err(e) => failed.push(format!("{}: {e}", c.title())),
        }
    }
    state.retain(|_, t| now - *t < (cfg.repeat_secs as i64).saturating_mul(7));
    Outcome {
        filed,
        failed,
        deferred,
        state,
    }
}
