//! The typed record of each stop and release the `world` verbs make: the `loop-stop` family
//! goodput charges halted time from. A stamp file is deleted on release; this row is not.

use std::path::Path;

use tsd::stop::{Kind, Reason, StopRecord};

pub const ACTOR: &str = "world";

/// The closed reason a verb was given with `--cause`, or `default` when none was.
pub fn cause(args: &[String], default: Reason) -> Result<Reason, String> {
    match args.iter().position(|a| a == "--cause") {
        None => Ok(default),
        Some(i) => Reason::parse(args.get(i + 1).map(String::as_str).ok_or("--cause needs a value")?),
    }
}

/// Appends one row under `run`; a failure is returned for the caller to say, never to stop on.
#[allow(clippy::too_many_arguments)]
pub fn record(run: &Path, now: u64, kind: Kind, reason: Reason, actor: &str, repo: Option<&str>, bead: Option<&str>, detail: &str) -> Result<(), String> {
    let rec = StopRecord { kind, reason, actor: actor.into(), repo: repo.map(str::to_string), bead: bead.map(str::to_string), detail: detail.into() };
    rec.write(run, &tsd::default_host(), &tsd::iso_utc(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_cause_is_one_of_the_closed_set_or_the_verbs_default() {
        assert_eq!(cause(&v(&["--why", "x"]), Reason::Operator), Ok(Reason::Operator));
        assert_eq!(cause(&v(&["--cause", "incident"]), Reason::Operator), Ok(Reason::Incident));
        assert!(cause(&v(&["--cause", "tired"]), Reason::Operator).is_err());
        assert!(cause(&v(&["--cause"]), Reason::Operator).is_err());
    }

    #[test]
    fn a_halt_and_its_release_are_two_rows_the_lifecycle_section_reads_as_one_interval() {
        let tmp = testkit::TempDir::new("world-stoprec");
        record(tmp.path(), 1_000, Kind::Halt, Reason::Drain, ACTOR, None, None, "free text").unwrap();
        record(tmp.path(), 2_000, Kind::Release, Reason::Operator, ACTOR, None, None, "").unwrap();
        let text = std::fs::read_to_string(tsd::family_path(tmp.path(), tsd::stop::FAMILY)).unwrap();
        let kinds: Vec<_> = text.lines().map(|l| StopRecord::from_row(&serde_json::from_str(l).unwrap()).unwrap().kind).collect();
        assert_eq!(kinds, vec![Kind::Halt, Kind::Release]);
    }
}
