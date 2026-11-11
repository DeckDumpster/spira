//! Where a unit gate's build phase went: cargo's `--timings` report read back, and the
//! seconds its rustc wrappers spent waiting in spira-admit.

use serde_json::Value;

/// Where `cargo build --timings` leaves the latest report, under the tree's target directory.
pub const REPORT: &str = "target/cargo-timings/cargo-timing.html";

const SLOWEST: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub package: String,
    pub kind: String,
    pub secs: f64,
    pub start: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Build {
    pub compiled: u64,
    pub fresh: u64,
    pub admit_wait_secs: u64,
    pub slowest: Vec<Unit>,
}

impl Build {
    /// `package/kind:secs@start,…`, slowest first; one token, so gate.log stays splittable.
    pub fn slowest_field(&self) -> String {
        if self.slowest.is_empty() {
            return "-".into();
        }
        let u: Vec<String> =
            self.slowest.iter().map(|u| format!("{}/{}:{:.1}@{:.1}", u.package, u.kind, u.secs, u.start)).collect();
        u.join(",")
    }

    /// The gate.log fields after the phases.
    pub fn suffix(&self) -> String {
        format!(
            " compiled={} fresh={} admit_wait={} slowest={}",
            self.compiled,
            self.fresh,
            self.admit_wait_secs,
            self.slowest_field()
        )
    }
}

/// Seconds spent waiting in spira-admit, summed over every `admitted to compile slot N after Ns`.
pub fn admit_wait_secs(output: &str) -> u64 {
    output
        .lines()
        .filter_map(|l| l.split_once("admitted to compile slot ")?.1.split_once(" after ")?.1.trim().strip_suffix('s')?.parse::<u64>().ok())
        .sum()
}

fn kind(target: &str) -> String {
    let t = target.trim();
    let quoted = t.split('"').nth(1).filter(|k| !k.is_empty());
    let mode = t.rsplit_once('(').and_then(|(_, m)| m.strip_suffix(')'));
    match (quoted, mode) {
        (None, None) => "lib".into(),
        (Some(k), None) => k.into(),
        (None, Some(m)) => m.into(),
        (Some(k), Some(m)) => format!("{k}-{m}"),
    }
}

fn summary(html: &str, label: &str) -> Option<u64> {
    html.split_once(&format!("<td>{label}:</td><td>"))?.1.split('<').next()?.trim().parse().ok()
}

/// The report's units and counts, with the admit wait of the build output alongside. None when
/// the report is not a cargo timing report.
pub fn parse(html: &str, build_output: &str) -> Option<Build> {
    let data = html.split_once("const UNIT_DATA = ")?.1;
    let units = serde_json::Deserializer::from_str(data).into_iter::<Value>().next()?.ok()?;
    let mut all: Vec<Unit> = units
        .as_array()?
        .iter()
        .map(|u| Unit {
            package: u["name"].as_str().unwrap_or("?").to_string(),
            kind: kind(u["target"].as_str().unwrap_or("")),
            secs: u["duration"].as_f64().unwrap_or(0.0),
            start: u["start"].as_f64().unwrap_or(0.0),
        })
        .collect();
    all.sort_by(|a, b| b.secs.total_cmp(&a.secs));
    all.truncate(SLOWEST);
    Some(Build {
        compiled: summary(html, "Dirty units").unwrap_or(0),
        fresh: summary(html, "Fresh units").unwrap_or(0),
        admit_wait_secs: admit_wait_secs(build_output),
        slowest: all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT_HTML: &str = r#"<table><tr><td>Fresh units:</td><td>3</td></tr>
<tr><td>Dirty units:</td><td>7</td></tr>
<tr><td>Total units:</td><td>10</td></tr></table>
<script>
const UNIT_DATA = [
  {"i":0,"name":"runtime","version":"0.1.0","mode":"todo","target":"","start":4.2,"duration":88.4},
  {"i":1,"name":"gate","version":"0.1.0","mode":"todo","target":" gate \"bin\" (test)","start":90.0,"duration":31.0},
  {"i":2,"name":"a","version":"0.1.0","mode":"todo","target":"","start":0.1,"duration":0.5},
  {"i":3,"name":"b","version":"0.1.0","mode":"todo","target":" build-script-build \"custom-build\"","start":0.2,"duration":9.0},
  {"i":4,"name":"c","version":"0.1.0","mode":"todo","target":"","start":0.3,"duration":1.0},
  {"i":5,"name":"d","version":"0.1.0","mode":"todo","target":"","start":0.4,"duration":2.0},
  {"i":6,"name":"e","version":"0.1.0","mode":"todo","target":"","start":0.5,"duration":3.0}
];
const x = 1;
</script>"#;

    #[test]
    fn the_five_slowest_units_come_from_the_report_slowest_first() {
        let b = parse(REPORT_HTML, "").unwrap();
        assert_eq!(
            b.slowest_field(),
            "runtime/lib:88.4@4.2,gate/bin-test:31.0@90.0,b/custom-build:9.0@0.2,e/lib:3.0@0.5,d/lib:2.0@0.4"
        );
    }

    #[test]
    fn compiled_and_fresh_counts_are_the_reports_dirty_and_fresh_units() {
        let b = parse(REPORT_HTML, "").unwrap();
        assert_eq!((b.compiled, b.fresh), (7, 3));
    }

    #[test]
    fn admit_wait_is_the_sum_of_every_rustc_wait_in_the_build_output() {
        let out = "spira-admit: admitted to compile slot 1 after 86s\nnoise\nspira-admit: admitted to compile slot 2 after 12s\n";
        assert_eq!(admit_wait_secs(out), 98);
        assert_eq!(admit_wait_secs("Compiling x"), 0);
    }

    #[test]
    fn a_file_that_is_not_a_timing_report_yields_nothing() {
        assert_eq!(parse("<html>hi</html>", ""), None);
    }

    #[test]
    fn the_gate_log_suffix_has_every_field() {
        let b = parse(REPORT_HTML, "admitted to compile slot 1 after 5s").unwrap();
        assert!(b.suffix().starts_with(" compiled=7 fresh=3 admit_wait=5 slowest=runtime/lib:88.4@4.2,"));
    }
}
