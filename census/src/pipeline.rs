//! The census classification pipeline: raw event rows -> causal-event counts, the merge of
//! all-time and since-watermark counts, and the remedy-label readers. Each function takes
//! the text its inputs carry and returns the exact text the caller prints, one `\n`-ended
//! line per record.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// `event_type` + cause -> failure-class name, shared by every pipeline below so two of them
/// cannot name the same event differently.
pub fn class_for(event_type: &str, cause: &str) -> Option<String> {
    let cause = if cause.is_empty() { "unrecorded" } else { cause };
    match event_type {
        "requeued" => Some(format!("sp-requeue-{cause}")),
        "recurred" => Some(format!("sp-recur-{cause}")),
        "reclaimed" if cause != "unrecorded" => Some(format!("sp-reclaim-{cause}")),
        "reclaimed" => Some("sp-reclaim".into()),
        "lapsed" => Some(format!("sp-lapsed-{cause}")),
        "reopen" | "reopened" => Some(format!("sp-reopen-{cause}")),
        _ => None,
    }
}

/// The data rows of `bd sql` tabular output with exactly `width` columns, separators,
/// headers and row-count footers dropped; each cell trimmed, outer empty cells removed.
fn table_rows(text: &str, width: usize) -> Vec<Vec<&str>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('+') || line.starts_with('(') {
            continue;
        }
        let mut parts: Vec<&str> = line.split('|').map(str::trim).collect();
        while parts.first() == Some(&"") {
            parts.remove(0);
        }
        while parts.last() == Some(&"") {
            parts.pop();
        }
        if parts.len() != width || parts[0] == "event_type" || parts[0].contains("COALESCE") {
            continue;
        }
        out.push(parts);
    }
    out
}

/// Raw `(event_type, new_value, issue_id, epoch)` rows -> `<causal> <victims> <detections>
/// <class>` lines, ranked by causal count, victims, detections (descending), then class.
/// Same-class events less than `gap_s` apart are one causal event.
pub fn cluster(tabular: &str, gap_s: i64) -> String {
    let mut by_class: BTreeMap<String, Vec<(i64, &str)>> = BTreeMap::new();
    for r in table_rows(tabular, 4) {
        let Ok(ts) = r[3].parse::<i64>() else { continue };
        let Some(cls) = class_for(r[0], r[1]) else { continue };
        by_class.entry(cls).or_default().push((ts, r[2]));
    }
    let mut out: Vec<(usize, usize, usize, String)> = Vec::new();
    for (cls, mut events) in by_class {
        events.sort_by_key(|e| e.0);
        let mut causal = 0;
        let mut prev: Option<i64> = None;
        for (ts, _) in &events {
            if prev.is_none_or(|p| ts - p >= gap_s) {
                causal += 1;
            }
            prev = Some(*ts);
        }
        let victims = events.iter().map(|e| e.1).collect::<std::collections::HashSet<_>>().len();
        out.push((causal, victims, events.len(), cls));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)).then_with(|| a.3.cmp(&b.3)));
    out.iter().map(|(c, v, d, cls)| format!("{c} {v} {d} {cls}\n")).collect()
}

type Counts = HashMap<String, i64>;

fn read_counts(text: &str) -> (Counts, Counts, Counts) {
    let (mut causal, mut victims, mut detections) = (Counts::new(), Counts::new(), Counts::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(parts) = split_ws_n(line, 4) else { continue };
        let class = parts[3].to_string();
        let Ok(c) = parts[0].parse::<i64>() else { continue };
        causal.insert(class.clone(), c);
        let Ok(v) = parts[1].parse::<i64>() else { continue };
        victims.insert(class.clone(), v);
        let Ok(d) = parts[2].parse::<i64>() else { continue };
        detections.insert(class, d);
    }
    (causal, victims, detections)
}

/// `str.split(None, n-1)`: runs of whitespace separate fields, the last field keeps the rest.
fn split_ws_n(line: &str, n: usize) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while out.len() + 1 < n {
        let end = rest.find(char::is_whitespace)?;
        out.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    if rest.is_empty() {
        return None;
    }
    out.push(rest);
    Some(out)
}

/// Two `cluster` outputs -> `<causal-since> <class> (<victims> victims, <detections>
/// detections, <causal-all-time> all-time)` lines, ranked by since-watermark counts.
pub fn cluster_merge(all_time: &str, since_wm: &str) -> String {
    let (a_c, _, _) = read_counts(all_time);
    let (w_c, w_v, w_d) = read_counts(since_wm);
    let mut classes: Vec<&String> = a_c.keys().chain(w_c.keys()).collect();
    classes.sort();
    classes.dedup();
    let get = |m: &Counts, c: &str| m.get(c).copied().unwrap_or(0);
    classes.sort_by(|x, y| {
        get(&w_c, y).cmp(&get(&w_c, x)).then(get(&w_v, y).cmp(&get(&w_v, x))).then(get(&w_d, y).cmp(&get(&w_d, x))).then_with(|| x.cmp(y))
    });
    classes
        .iter()
        .map(|c| {
            format!("{} {} ({} victims, {} detections, {} all-time)\n", get(&w_c, c), c, get(&w_v, c), get(&w_d, c), get(&a_c, c))
        })
        .collect()
}

fn fold_map(text: &str) -> HashMap<&str, &str> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            m.insert(parts[0], parts[1]);
        }
    }
    m
}

fn beads(json: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Array(a)) => a,
        Ok(v) => vec![v],
        Err(_) => Vec::new(),
    }
}

fn covered<'a>(bead: &'a Value) -> impl Iterator<Item = &'a str> {
    bead.get("labels").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter_map(|l| l.strip_prefix("covers:"))
}

/// A bead list -> one covered class per line, resolved through the fold map.
pub fn covers(json: &str, fold: &str) -> String {
    let fold = fold_map(fold);
    let mut out = String::new();
    for b in beads(json) {
        for cls in covered(&b) {
            out.push_str(fold.get(cls).copied().unwrap_or(cls));
            out.push('\n');
        }
    }
    out
}

/// A bead list -> `<bead-id> <class>` per covers: label of a bead closed within
/// `window_days` of `now_epoch`. A close time that does not parse as a zoned timestamp is
/// not excluded.
pub fn covers_closed(json: &str, fold: &str, window_days: i64, now_epoch: i64) -> String {
    let fold = fold_map(fold);
    let cutoff = now_epoch - window_days * 86400;
    let mut out = String::new();
    for b in beads(json) {
        let closed_at = b.get("closed_at").and_then(Value::as_str).unwrap_or("");
        if parse_zoned_epoch(closed_at).is_some_and(|t| t < cutoff) {
            continue;
        }
        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        for cls in covered(&b) {
            out.push_str(&format!("{id} {}\n", fold.get(cls).copied().unwrap_or(cls)));
        }
    }
    out
}

/// `YYYY-MM-DD[T ]HH:MM:SS[.fraction](Z|±HH[:MM])` -> unix seconds; None for anything else.
fn parse_zoned_epoch(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20 || !s.is_char_boundary(19) || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &r[digits..];
    }
    let offset = if rest == "Z" || rest == "z" {
        0
    } else {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let z = rest[1..].replace(':', "");
        if z.len() != 2 && z.len() != 4 || !z.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let oh: i64 = z[..2].parse().ok()?;
        let om: i64 = if z.len() == 4 { z[2..].parse().ok()? } else { 0 };
        sign * (oh * 3600 + om * 60)
    };
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + se - offset)
}

/// Actor-excluded events (`event_type|new_value|actor|beads|events`) -> one `hand-written
/// (not ranked): ...` line per (class, actor), ranked by beads descending.
pub fn handwritten(tabular: &str) -> String {
    let mut rows: Vec<(i64, String, &str)> = Vec::new();
    for r in table_rows(tabular, 5) {
        let Ok(n) = r[3].parse::<i64>() else { continue };
        if n == 0 {
            continue;
        }
        let Some(cls) = class_for(r[0], r[1]) else { continue };
        rows.push((n, cls, r[2]));
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(b.2)));
    rows.iter().map(|(n, cls, actor)| format!("hand-written (not ranked): {cls} {n} beads (actor {actor})\n")).collect()
}

/// Deliberate-cause events (`event_type|new_value|beads|events`) -> one `deliberate, not
/// ranked: ...` line per class, ranked by beads descending.
pub fn deliberate(tabular: &str) -> String {
    let mut rows: Vec<(i64, i64, String)> = Vec::new();
    for r in table_rows(tabular, 4) {
        let (Ok(nb), Ok(ne)) = (r[2].parse::<i64>(), r[3].parse::<i64>()) else { continue };
        if nb == 0 {
            continue;
        }
        let Some(cls) = class_for(r[0], r[1]) else { continue };
        rows.push((nb, ne, cls));
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    rows.iter().map(|(nb, ne, cls)| format!("deliberate, not ranked: {cls} {nb} beads ({ne} events)\n")).collect()
}
