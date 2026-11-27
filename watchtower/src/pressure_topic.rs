//! `host/pressure`: the host's PSI cpu `some avg300` and io `full avg300` against their
//! configured lines, published when either crosses its line. The band last published is kept in one file, so a
//! pass on the same side says nothing and a reconnect republishes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use spira_pub::Publisher;

pub const TOPIC: &str = "host/pressure";
pub const CPU_LINE_KEY: &str = "SPIRA_PRESSURE_CPU_AVG300";
pub const IO_LINE_KEY: &str = "SPIRA_PRESSURE_IO_AVG300";
const FLUSH: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lines {
    pub cpu: f64,
    pub io: f64,
}

/// One published payload.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub cpu: f64,
    pub io: f64,
    pub cpu_over: bool,
    pub io_over: bool,
    pub at: u64,
}

/// Both lines from `cfg`; refused, naming the key, when either is absent or not a number.
pub fn lines(cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Lines, String> {
    let line = |key: &str| {
        let text = cfg(key).map_err(|e| format!("{key}: {e}"))?;
        text.trim().parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("{key}: {text:?} is not a number"))
    };
    Ok(Lines { cpu: line(CPU_LINE_KEY)?, io: line(IO_LINE_KEY)? })
}

/// The payload to publish, when the side either reading is on differs from `last`'s. Nothing
/// published yet counts as both under.
pub fn crossing(last: Option<&Band>, cpu: f64, io: f64, lines: &Lines, at: u64) -> Option<Band> {
    let now = Band { cpu, io, cpu_over: cpu >= lines.cpu, io_over: io >= lines.io, at };
    let was = last.map_or((false, false), |b| (b.cpu_over, b.io_over));
    (was != (now.cpu_over, now.io_over)).then_some(now)
}

pub fn record_path(run: &Path) -> PathBuf {
    run.join("conditions").join("pressure-band.json")
}

fn recorded(path: &Path) -> Option<Band> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn record(path: &Path, band: &Band) -> Result<(), String> {
    let text = serde_json::to_string(band).map_err(|e| e.to_string())?;
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| spira_config::write_atomic(path, &text))
        .map_err(|e| format!("cannot record the band in {}: {e}", path.display()))
}

pub struct Pressure {
    publisher: Publisher,
    record: PathBuf,
}

impl Pressure {
    pub fn new(publisher: Publisher, run: &Path) -> Pressure {
        let path = record_path(run);
        let held = path.clone();
        publisher.on_connect(move |again| {
            if let Some(band) = recorded(&held) {
                // A refusal is the publisher's to log; nothing here can mend it.
                let _ = again.publish(TOPIC, &serde_json::json!(band));
            }
        });
        Pressure { publisher, record: path }
    }

    /// On the broker the configuration names.
    pub fn configured(run: &Path) -> Result<Pressure, String> {
        Publisher::new("watchtower").map(|p| Pressure::new(p, run)).map_err(|e| e.to_string())
    }

    /// Connects now when a band is recorded, so a long-lived process republishes it rather than
    /// waiting for the next crossing.
    pub fn reconnect_recorded(&self) {
        if recorded(&self.record).is_some() {
            // A failure is logged by the publisher, which connects when the broker returns.
            let _ = self.publisher.connect();
        }
    }

    /// One pass over `psi_dir`'s `cpu` and `io` files. The band is recorded only once the
    /// broker has taken it, so an undelivered crossing is published by a later pass.
    pub fn pass(&self, psi_dir: &Path, lines: &Lines, at: u64) -> Result<String, String> {
        let read = |resource: &str, line: &str, parse: fn(&str) -> Option<f64>| {
            let path = psi_dir.join(resource);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            parse(&text).ok_or_else(|| format!("{} has no {line} avg300", path.display()))
        };
        // The kernel reports system-wide cpu `full` as zero, so cpu is judged on `some`.
        let cpu = read("cpu", "some", crate::probes::psi_some_avg300)?;
        let io = read("io", "full", crate::probes::psi_full_avg300)?;
        let Some(band) = crossing(recorded(&self.record).as_ref(), cpu, io, lines, at) else {
            return Ok(format!("no crossing (cpu {cpu}, io {io})"));
        };
        self.publisher.publish(TOPIC, &serde_json::json!(band)).and_then(|()| self.publisher.flush(FLUSH)).map_err(|e| format!("{TOPIC} not published: {e}"))?;
        record(&self.record, &band)?;
        Ok(format!("published {TOPIC} (cpu {cpu} over={}, io {io} over={})", band.cpu_over, band.io_over))
    }
}

/// A conditions pass's `host/pressure` step, as its log line. It never fails the pass.
pub fn run_pass(pressure: Result<&Pressure, &String>, psi_dir: &Path, cfg: &dyn Fn(&str) -> Result<String, String>, at: u64) -> String {
    let said = pressure.map_err(String::clone).and_then(|p| lines(cfg).and_then(|l| p.pass(psi_dir, &l, at)));
    format!("watchtower: pressure topic — {}", said.unwrap_or_else(|e| e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: Lines = Lines { cpu: 20.0, io: 30.0 };

    #[test_id(t5h6t06)]
    #[test]
    fn a_rise_over_a_line_and_a_fall_back_under_it_are_each_published() {
        let rose = crossing(None, 1.0, 31.0, &LINES, 5).expect("a rise");
        assert_eq!(rose, Band { cpu: 1.0, io: 31.0, cpu_over: false, io_over: true, at: 5 });
        let fell = crossing(Some(&rose), 1.0, 29.0, &LINES, 6).expect("a fall");
        assert_eq!((fell.io_over, fell.at), (false, 6));
        let cpu = crossing(Some(&fell), 20.0, 29.0, &LINES, 7).expect("reaching the line is over it");
        assert!(cpu.cpu_over && !cpu.io_over);
    }

    #[test_id(ts805mg)]
    #[test]
    fn two_samples_on_the_same_side_publish_nothing() {
        assert_eq!(crossing(None, 19.0, 29.0, &LINES, 5), None, "nothing published yet is under");
        let over = crossing(None, 50.0, 50.0, &LINES, 5).expect("a rise");
        assert_eq!(crossing(Some(&over), 99.0, 31.0, &LINES, 6), None);
    }

    #[test_id(tddoio2)]
    #[test]
    fn a_missing_line_is_refused_naming_its_key() {
        for missing in [CPU_LINE_KEY, IO_LINE_KEY] {
            let e = lines(&|k| if k == missing { Err("not declared".into()) } else { Ok("10".into()) }).unwrap_err();
            assert!(e.contains(missing), "{e}");
        }
        let e = lines(&|_| Ok("lots".into())).unwrap_err();
        assert!(e.contains(CPU_LINE_KEY), "{e}");
        assert_eq!(lines(&|k| Ok(if k == CPU_LINE_KEY { "20".into() } else { "30".into() })).unwrap(), LINES);
    }
}
