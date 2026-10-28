// tsd-read — the observational queries over run/tsd/ families (see query.rs). A family in
// tsd::store::MOVED is read from the metrics store and the command refuses, with a named error
// and a non-zero exit, when the store is unreachable or holds no row for the window.
//
// usage: tsd-read where|rework|sentinel|time [<hours>]
//        tsd-read bead <id>
//        tsd-read baseline <family> <field> <hours>
//        tsd-read rate <family> <hours>
//        tsd-read dwell <family> <field> <p> [<hours>]
//        tsd-read by-group <family> <group> <field> [<hours>]
//        tsd-read count-by <family> <group> [<hours>]
//        tsd-read moved <family>                  exit 0 when the family lives in the metrics store
//        tsd-read rows <family> [<hours>]        the family's rows, one JSON object per line
//
// SPIRA_RUN is a raw env read and the store address comes from tsd::store::endpoint (tsd cannot
// depend on spira-config). SPIRA_TSD_NOW (epoch seconds) pins the window's end.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use tsd::query::{self, Failure, Source};
use tsd::store::Reader;

const DEFAULT_HOURS: u64 = 720;

fn usage() -> Failure {
    Failure { code: 2, message: "usage: tsd-read where|rework|sentinel|time [<hours>] | bead <id> | baseline <family> <field> <hours> | rate <family> <hours> | dwell <family> <field> <p> [<hours>] | by-group <family> <group> <field> [<hours>] | count-by <family> <group> [<hours>]".into() }
}

fn bad(what: &str, v: &str) -> Failure {
    Failure { code: 2, message: format!("tsd-read: bad {what} {v:?}") }
}

fn hours(v: Option<&String>) -> Result<Option<u64>, Failure> {
    v.map(|h| h.parse::<u64>().map_err(|_| bad("hours", h))).transpose()
}

fn ident<'a>(v: Option<&'a String>, what: &str) -> Result<&'a str, Failure> {
    let v = v.ok_or_else(usage)?;
    let mut c = v.chars();
    let ok = matches!(c.next(), Some(f) if f.is_ascii_lowercase() || f == '_') && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok { Ok(v) } else { Err(bad(what, v)) }
}

fn family(v: Option<&String>) -> Result<&str, Failure> {
    let v = v.ok_or_else(usage)?;
    if tsd::valid_family(v) { Ok(v) } else { Err(bad("family", v)) }
}

fn run(args: &[String]) -> Result<String, Failure> {
    if args.first().map(String::as_str) == Some("moved") {
        return match args.get(1) {
            Some(f) if tsd::store::moved(f) => Ok(String::new()),
            _ => Err(Failure { code: 1, message: String::new() }),
        };
    }
    let root = PathBuf::from(std::env::var("SPIRA_RUN").map_err(|_| Failure { code: 2, message: "tsd-read: SPIRA_RUN is not set".into() })?);
    let endpoint = tsd::store::endpoint();
    let now = match std::env::var("SPIRA_TSD_NOW") {
        Ok(v) => v.parse().map_err(|_| bad("SPIRA_TSD_NOW", &v))?,
        Err(_) => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    };
    let reader = Reader::new(endpoint.as_deref()).ok();
    let src = Source { reader: reader.as_ref(), root: &root };
    let a = |i: usize| args.get(i);
    let json = |v: Result<serde_json::Value, Failure>| v.map(|v| v.to_string());
    match args.first().map(String::as_str) {
        Some("where") => json(query::wip(&src, now, hours(a(1))?.unwrap_or(DEFAULT_HOURS))),
        Some("rework") => json(query::rework(&src, now, hours(a(1))?.unwrap_or(DEFAULT_HOURS))),
        Some("sentinel") => json(query::sentinel(&src, now, hours(a(1))?.unwrap_or(DEFAULT_HOURS))),
        Some("time") => json(query::time(&src, now, hours(a(1))?.unwrap_or(DEFAULT_HOURS))),
        Some("bead") => {
            let id = a(1).ok_or_else(usage)?;
            if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
                return Err(bad("bead", id));
            }
            json(query::bead(&src, id))
        }
        Some("baseline") => json(query::baseline(&src, family(a(1))?, ident(a(2), "field")?, now, hours(a(3))?.ok_or_else(usage)?)),
        Some("rate") => json(query::rate(&src, family(a(1))?, now, hours(a(2))?.ok_or_else(usage)?)),
        Some("dwell") => {
            let p = a(3).ok_or_else(usage)?;
            let pv: f64 = p.parse().ok().filter(|v| (0.0..=1.0).contains(v)).ok_or_else(|| bad("percentile", p))?;
            json(query::dwell(&src, family(a(1))?, ident(a(2), "field")?, pv, now, hours(a(4))?))
        }
        Some("by-group") => query::by_group(&src, family(a(1))?, ident(a(2), "group")?, ident(a(3), "field")?, now, hours(a(4))?),
        Some("rows") => {
            let rows = src.rows(family(a(1))?, now.saturating_sub(hours(a(2))?.unwrap_or(DEFAULT_HOURS) * 3600))?;
            Ok(rows.iter().map(|r| format!("{r}\n")).collect())
        }
        Some("count-by") => query::count_by(&src, family(a(1))?, ident(a(2), "group")?, now, hours(a(3))?),
        _ => Err(usage()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(out) => {
            if out.is_empty() || out.ends_with('\n') { print!("{out}") } else { println!("{out}") }
            ExitCode::SUCCESS
        }
        Err(f) => {
            if !f.message.is_empty() {
                eprintln!("{}", f.message);
            }
            ExitCode::from(f.code.clamp(1, 255) as u8)
        }
    }
}
