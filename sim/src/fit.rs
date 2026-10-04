//! Duration models fitted from production logs. A series is log-normal in seconds,
//! clamped to what was observed; the fit refuses rather than invent a distribution.

use crate::Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const MIN_SAMPLES: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Series {
    pub n: usize,
    pub mu: f64,
    pub sigma: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Durations {
    pub series: BTreeMap<String, Series>,
}

impl Series {
    pub fn fit(samples: &[f64]) -> Option<Series> {
        let xs: Vec<f64> = samples.iter().copied().filter(|x| *x > 0.0 && x.is_finite()).collect();
        if xs.len() < MIN_SAMPLES {
            return None;
        }
        let logs: Vec<f64> = xs.iter().map(|x| x.ln()).collect();
        let n = logs.len() as f64;
        let mu = logs.iter().sum::<f64>() / n;
        let var = logs.iter().map(|l| (l - mu).powi(2)).sum::<f64>() / (n - 1.0);
        Some(Series {
            n: xs.len(),
            mu,
            sigma: var.sqrt(),
            min: xs.iter().copied().fold(f64::INFINITY, f64::min),
            max: xs.iter().copied().fold(0.0, f64::max),
        })
    }

    /// One duration in milliseconds, drawn from the seeded PRNG.
    pub fn sample_ms(&self, rng: &mut Rng) -> u64 {
        let s = (self.mu + self.sigma * rng.normal()).exp().clamp(self.min, self.max);
        (s * 1000.0).round() as u64
    }
}

impl Durations {
    pub fn parse(text: &str) -> Result<Durations, String> {
        toml::from_str(text).map_err(|e| format!("durations: {e}"))
    }
}

fn read_nonempty(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text.trim().is_empty() {
        return Err(format!("{}: empty", path.display()));
    }
    Ok(text)
}

fn secs(v: &str) -> Option<f64> {
    v.strip_suffix('s').unwrap_or(v).parse().ok()
}

fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// gate.log: `<ts> <repo> <branch> waited=Ns ran=Ns rc=N <verdict>`.
fn gate_samples(text: &str, into: &mut BTreeMap<String, Vec<f64>>) {
    for line in text.lines() {
        if let Some(r) = kv(line, "ran").and_then(secs) {
            into.entry("gate.ran".into()).or_default().push(r);
        }
    }
}

/// land-times.tsv: tab-separated `<ts> <bead> key=value... phases=a:1,b:2`.
fn land_samples(text: &str, into: &mut BTreeMap<String, Vec<f64>>) {
    for line in text.lines() {
        for f in line.split('\t').skip(2) {
            let Some((k, v)) = f.split_once('=') else { continue };
            if k == "phases" {
                for p in v.split(',') {
                    if let Some((n, d)) = p.split_once(':').and_then(|(n, d)| Some((n, d.parse::<f64>().ok()?))) {
                        into.entry(format!("land.phase.{n}")).or_default().push(d);
                    }
                }
            } else if k == "total" || k == "land-local" {
                if let Ok(d) = v.parse::<f64>() {
                    into.entry(format!("land.{k}")).or_default().push(d);
                }
            }
        }
    }
}

/// Fits every series with enough samples. Missing, empty or sample-less logs are errors.
pub fn fit_files(gate_log: &Path, land_times: &Path) -> Result<Durations, String> {
    let mut samples = BTreeMap::new();
    let g = read_nonempty(gate_log)?;
    let l = read_nonempty(land_times)?;
    gate_samples(&g, &mut samples);
    land_samples(&l, &mut samples);
    for required in ["gate.ran", "land.total"] {
        if !samples.contains_key(required) {
            return Err(format!("no {required} samples in the logs"));
        }
    }
    let series: BTreeMap<String, Series> =
        samples.iter().filter_map(|(k, v)| Some((k.clone(), Series::fit(v)?))).collect();
    for required in ["gate.ran", "land.total"] {
        if !series.contains_key(required) {
            return Err(format!("fewer than {MIN_SAMPLES} usable {required} samples"));
        }
    }
    Ok(Durations { series })
}

pub fn render(d: &Durations) -> Result<String, String> {
    toml::to_string_pretty(d).map_err(|e| e.to_string())
}
