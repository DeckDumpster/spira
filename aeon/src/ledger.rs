//! The ledger (`$SPIRA_RUN/aeon-ledger.log`, DESIGN.md §2.4) and the trace readers it needs:
//! `attempt_trace` (the last attempt's segment), `spira_trace_mark` (the segment's head) and
//! `session_result_fields` (what the session spent).

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::util::{iso_utc, round_half_even};

pub const TRIM_OVER: usize = 20_000;
pub const TRIM_KEEP: usize = 5_000;

pub struct Ledger {
    pub path: PathBuf,
    /// A dry run inspects; it does not summon — every write is muzzled.
    pub dry: bool,
}

impl Ledger {
    /// `<ts> <rest>` appended with O_APPEND, one write.
    pub fn write(&self, now: i64, rest: &str) {
        if self.dry {
            return;
        }
        let line = format!("{} {rest}\n", iso_utc(now));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub fn born(&self, now: i64, fayth: &str, pid: u32) {
        self.write(now, &format!("born {fayth} {pid}"));
    }

    pub fn awake(&self, now: i64, fayth: &str, what: &str) {
        self.write(now, &format!("awake {fayth} {what}"));
    }

    pub fn done(&self, now: i64, fayth: &str, bead: &str, rc: i32, status: &str, fields: &SessionFields) {
        self.write(now, &done_rest(fayth, bead, rc, status, fields));
    }

    /// Bounded here rather than by logrotate: >20,000 lines keeps the last 5,000.
    pub fn trim(&self) {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return };
        let n = text.lines().count();
        if n <= TRIM_OVER {
            return;
        }
        let keep: Vec<&str> = text.lines().skip(n - TRIM_KEEP).collect();
        let tmp = self.path.with_extension("log.trim");
        if std::fs::write(&tmp, keep.join("\n") + "\n").is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

pub fn done_rest(fayth: &str, bead: &str, rc: i32, status: &str, fields: &SessionFields) -> String {
    format!("done {fayth} {bead} rc={rc} status={status} {}", fields.render())
}

/// What a session spent, parsed from its trace's `result` records. `None` renders `?`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFields {
    pub wall_s: Option<i64>,
    pub api_s: Option<i64>,
    pub turns: Option<i64>,
    pub in_tok: Option<i64>,
    pub cache_read_tok: Option<i64>,
    pub out_tok: Option<i64>,
    pub think_tok: Option<i64>,
    pub cost_usd: Option<f64>,
}

fn q(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}

impl SessionFields {
    pub fn render(&self) -> String {
        format!(
            "wall_s={} api_s={} turns={} in_tok={} cache_read_tok={} out_tok={} think_tok={} cost_usd={}",
            q(self.wall_s),
            q(self.api_s),
            q(self.turns),
            q(self.in_tok),
            q(self.cache_read_tok),
            q(self.out_tok),
            q(self.think_tok),
            self.cost_usd.map(|c| format!("{c:.4}")).unwrap_or_else(|| "?".into())
        )
    }
}

/// A JSON number that is a real number (a JSON `true` is not a count).
fn numeric(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn obj<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a serde_json::Value> {
    v.get(k).filter(|x| x.is_object())
}

/// `session_result_fields` over an already-extracted trace segment.
pub fn session_fields(segment: &str) -> SessionFields {
    let recs: Vec<serde_json::Value> = segment
        .lines()
        .filter(|l| l.contains("\"type\":\"result\""))
        .map(|l| l.trim())
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|d| d.is_object() && d.get("type").and_then(|t| t.as_str()) == Some("result"))
        .collect();
    let sum = |f: &dyn Fn(&serde_json::Value) -> Option<f64>| -> Option<f64> {
        let mut t: Option<f64> = None;
        for d in &recs {
            if let Some(v) = f(d) {
                t = Some(t.unwrap_or(0.0) + v);
            }
        }
        t
    };
    let last = |f: &dyn Fn(&serde_json::Value) -> Option<f64>| -> Option<f64> { recs.iter().filter_map(f).last() };
    let usage = |d: &serde_json::Value, k: &str| obj(d, "usage").and_then(|u| numeric(u.get(k)));
    let r = |v: Option<f64>, div: f64| v.map(|x| round_half_even(x / div));
    SessionFields {
        wall_s: r(sum(&|d| numeric(d.get("duration_ms"))), 1000.0),
        api_s: r(last(&|d| numeric(d.get("duration_api_ms"))), 1000.0),
        turns: r(sum(&|d| numeric(d.get("num_turns"))), 1.0),
        in_tok: r(sum(&|d| usage(d, "input_tokens")), 1.0),
        cache_read_tok: r(sum(&|d| usage(d, "cache_read_input_tokens")), 1.0),
        out_tok: r(sum(&|d| usage(d, "output_tokens")), 1.0),
        think_tok: r(
            sum(&|d| obj(d, "usage").and_then(|u| obj(u, "output_tokens_details")).and_then(|o| numeric(o.get("thinking_tokens")))),
            1.0,
        ),
        cost_usd: last(&|d| numeric(d.get("total_cost_usd"))),
    }
}

/// `attempt_trace <file> [cap]`: the bytes from the last `\n<mark>` (or a leading mark) to
/// the end; the whole file when it holds no mark; at most `cap` trailing bytes when cap > 0.
/// Read backwards in 64 KiB chunks so the cost does not grow with the bead's history.
pub fn attempt_trace(path: &Path, cap: u64, mark: &str) -> Vec<u8> {
    let Ok(mut fh) = std::fs::File::open(path) else { return Vec::new() };
    let size = fh.metadata().map(|m| m.len()).unwrap_or(0);
    let mark = mark.as_bytes();
    const CH: u64 = 1 << 16;
    let keep = mark.len() + 1;
    let (mut start, mut pos, mut carry) = (0u64, size, Vec::<u8>::new());
    let mut nl_mark = vec![b'\n'];
    nl_mark.extend_from_slice(mark);
    while pos > 0 {
        let step = CH.min(pos);
        pos -= step;
        let mut buf = vec![0u8; step as usize];
        if fh.seek(SeekFrom::Start(pos)).is_err() || fh.read_exact(&mut buf).is_err() {
            break;
        }
        buf.extend_from_slice(&carry);
        if let Some(i) = rfind(&buf, &nl_mark) {
            start = pos + i as u64 + 1;
            break;
        }
        if pos == 0 && buf.starts_with(mark) {
            start = 0;
            break;
        }
        carry = buf[..keep.min(buf.len())].to_vec();
    }
    let from = if cap > 0 { start.max(size.saturating_sub(cap)) } else { start };
    let mut out = Vec::new();
    if fh.seek(SeekFrom::Start(from)).is_ok() {
        let _ = fh.read_to_end(&mut out);
    }
    out
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}

/// `session_result_fields <file>`: every field `?` for an absent or unreadable file.
pub fn session_result_fields(path: Option<&Path>, mark: &str) -> SessionFields {
    match path {
        Some(p) if p.is_file() => session_fields(&String::from_utf8_lossy(&attempt_trace(p, 0, mark))),
        _ => SessionFields::default(),
    }
}

/// `spira_trace_mark <file> <who>`: `<mark> <n+1> aeon=<who> at=<ts> kept=<bytes>`.
pub fn trace_mark_line(path: &Path, who: &str, mark: &str, now: i64) -> String {
    let kept = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let prefix = format!("{mark} ");
    let n = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).lines().filter(|l| l.starts_with(&prefix)).count())
        .unwrap_or(0);
    format!("{mark} {} aeon={who} at={} kept={kept}", n + 1, iso_utc(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aeon-ledger-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ledger_formats_are_pinned() {
        let d = tmp("fmt");
        let l = Ledger { path: d.join("aeon-ledger.log"), dry: false };
        l.born(0, "builder", 4242);
        l.awake(1, "builder", "sp-a");
        l.awake(2, "builder", "claim-error epic_rank_rows failed rc=2");
        l.done(3, "builder", "sp-a", 0, "closed", &SessionFields::default());
        let f = SessionFields { wall_s: Some(12), api_s: Some(9), turns: Some(3), in_tok: Some(10), cache_read_tok: Some(20), out_tok: Some(30), think_tok: Some(0), cost_usd: Some(0.12345) };
        l.done(4, "ops", "sweep", 1, "sweep", &f);
        let text = std::fs::read_to_string(d.join("aeon-ledger.log")).unwrap();
        assert_eq!(
            text,
            "1970-01-01T00:00:00Z born builder 4242\n\
             1970-01-01T00:00:01Z awake builder sp-a\n\
             1970-01-01T00:00:02Z awake builder claim-error epic_rank_rows failed rc=2\n\
             1970-01-01T00:00:03Z done builder sp-a rc=0 status=closed wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?\n\
             1970-01-01T00:00:04Z done ops sweep rc=1 status=sweep wall_s=12 api_s=9 turns=3 in_tok=10 cache_read_tok=20 out_tok=30 think_tok=0 cost_usd=0.1235\n"
        );
        // cockpit-metrics.py's LEDGER_RE reads every line.
        let re = regex::Regex::new(r"^(\S+) (born|awake|done) (\S+)(?: (.*))?$").unwrap();
        assert!(text.lines().all(|l| re.is_match(l)));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let d = tmp("dry");
        let l = Ledger { path: d.join("aeon-ledger.log"), dry: true };
        l.born(0, "builder", 1);
        l.awake(0, "builder", "capacity");
        assert!(!d.join("aeon-ledger.log").exists());
    }

    #[test]
    fn trim_keeps_the_last_5000_over_20000() {
        let d = tmp("trim");
        let p = d.join("aeon-ledger.log");
        let body: String = (0..20_001).map(|i| format!("l{i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        Ledger { path: p.clone(), dry: false }.trim();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.lines().count(), 5000);
        assert_eq!(t.lines().next(), Some("l15001"));
        let small: String = (0..20_000).map(|i| format!("l{i}\n")).collect();
        std::fs::write(&p, &small).unwrap();
        Ledger { path: p.clone(), dry: false }.trim();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), small);
    }

    // test-session-result-fields.sh: per-turn fields sum, cumulative ones take the last,
    // a JSON true is not a number, a partial last line is ignored.
    #[test]
    fn session_fields_sum_and_last() {
        let seg = concat!(
            "{\"type\":\"assistant\"}\n",
            "{\"type\":\"result\",\"duration_ms\":1500,\"duration_api_ms\":1000,\"num_turns\":2,\"total_cost_usd\":0.1,\"usage\":{\"input_tokens\":5,\"output_tokens\":true,\"output_tokens_details\":{\"thinking_tokens\":7}}}\n",
            "{\"type\":\"result\",\"duration_ms\":1000,\"duration_api_ms\":2500,\"num_turns\":1,\"total_cost_usd\":0.25,\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":null}}\n",
            "{\"type\":\"result\",\"duration_ms\":99"
        );
        let f = session_fields(seg);
        assert_eq!(f.wall_s, Some(2), "2.5 s rounds half-to-even");
        assert_eq!(f.api_s, Some(2), "last cumulative value, 2.5 → 2");
        assert_eq!(f.turns, Some(3));
        assert_eq!(f.in_tok, Some(10));
        assert_eq!(f.out_tok, None, "a JSON true is never a count");
        assert_eq!(f.cache_read_tok, None, "null is unknown, not 0");
        assert_eq!(f.think_tok, Some(7));
        assert_eq!(f.cost_usd, Some(0.25));
        assert_eq!(session_fields("").render(), SessionFields::default().render());
    }

    #[test]
    fn attempt_trace_returns_the_last_segment() {
        let d = tmp("seg");
        let p = d.join("sp-a.log");
        let mark = "=== spira attempt";
        std::fs::write(&p, "old\n=== spira attempt 1 aeon=x\nfirst\n=== spira attempt 2 aeon=y\nsecond\n").unwrap();
        assert_eq!(attempt_trace(&p, 0, mark), b"=== spira attempt 2 aeon=y\nsecond\n");
        std::fs::write(&p, "=== spira attempt 1 aeon=x\nonly\n").unwrap();
        assert_eq!(attempt_trace(&p, 0, mark), b"=== spira attempt 1 aeon=x\nonly\n");
        std::fs::write(&p, "no marks at all\n").unwrap();
        assert_eq!(attempt_trace(&p, 0, mark), b"no marks at all\n");
        assert_eq!(attempt_trace(&p, 6, mark), b"t all\n");
        // A mark far behind a large tail, across the 64 KiB chunk boundary.
        let mut big = String::from("=== spira attempt 1 aeon=a\n");
        big.push_str(&"x".repeat(70_000));
        big.push_str("\n=== spira attempt 2 aeon=b\ntail\n");
        std::fs::write(&p, &big).unwrap();
        assert_eq!(attempt_trace(&p, 0, mark), b"=== spira attempt 2 aeon=b\ntail\n");
        assert!(attempt_trace(&d.join("missing"), 0, mark).is_empty());
    }

    #[test]
    fn trace_mark_counts_prior_attempts() {
        let d = tmp("mark");
        let p = d.join("sp-a.log");
        assert_eq!(trace_mark_line(&p, "ifrit", "=== spira attempt", 0), "=== spira attempt 1 aeon=ifrit at=1970-01-01T00:00:00Z kept=0");
        std::fs::write(&p, "=== spira attempt 1 aeon=x at=t kept=0\nbody\n").unwrap();
        let l = trace_mark_line(&p, "shiva", "=== spira attempt", 0);
        assert!(l.starts_with("=== spira attempt 2 aeon=shiva at=1970-01-01T00:00:00Z kept="), "{l}");
    }
}
