use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub habit: Vec<Habit>,
    #[serde(default)]
    pub stop: Vec<Stop>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    #[serde(default)]
    pub check: Vec<Check>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub name: String,
    pub run: String,
    #[serde(default = "default_stop_budget")]
    pub budget_ms: u64,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Habit {
    pub name: String,
    pub glob: String,
    #[serde(default)]
    pub run: String,
    #[serde(default = "default_budget")]
    pub budget_ms: u64,
    pub threshold: Option<String>,
    pub format: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub name: String,
    pub run: String,
    #[serde(default = "default_stop_budget")]
    pub budget_ms: u64,
    pub format: Option<String>,
}

fn default_budget() -> u64 {
    2000
}

fn default_stop_budget() -> u64 {
    150_000
}

impl Stop {
    pub fn is_unit_tests(&self) -> bool {
        self.format.as_deref() == Some("unit-tests")
    }
}

const DEFAULT_LIMIT: u32 = 20;

impl Habit {
    pub fn format(&self) -> &str {
        match self.format.as_deref() {
            Some(f) => f,
            None if matches!(self.name.as_str(), "rustfmt" | "complexity" | "traceability") => &self.name,
            None => "gcc",
        }
    }

    /// The first integer after `>` in `threshold = "cognitive > 20 per function"`.
    pub fn limit(&self) -> u32 {
        let Some(t) = self.threshold.as_deref() else { return DEFAULT_LIMIT };
        let after = t.split_once('>').map_or("", |(_, r)| r);
        after.split(|c: char| !c.is_ascii_digit()).find(|d| !d.is_empty()).and_then(|d| d.parse().ok()).unwrap_or(DEFAULT_LIMIT)
    }
}

/// `Ok(None)`: the repo declares no habits.
pub fn load(root: &Path) -> Result<Option<Config>, String> {
    let path = root.join(".spira").join("habits.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    spira_config::parse_foreign(&text).map(Some)
}

pub fn load_checks(root: &Path) -> Result<Vec<Check>, String> {
    let path = root.join(".spira").join("checks.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    spira_config::parse_foreign::<Checks>(&text).map(|c| c.check)
}

/// `run` split on whitespace, `{file}` and `{crate}` substituted; no shell is involved.
pub fn argv(run: &str, subst: &[(&str, &str)]) -> Vec<String> {
    run.split_whitespace()
        .map(|tok| subst.iter().fold(tok.to_string(), |t, (k, v)| t.replace(&format!("{{{k}}}"), v)))
        .collect()
}
