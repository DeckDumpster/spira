//! The `batcher` command line: its flags as one value, parsed from an argv alone so a caller
//! that is not a process (a replay) reads the same flags the production argv carries.

use std::path::PathBuf;

use spira_config::process::cfg;

#[derive(Debug, Default)]
pub struct Opts {
    pub cmd: String,
    pub repo: String,
    pub run: Option<PathBuf>,
    pub db: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub round_vm: Option<PathBuf>,
    pub suites: Option<String>,
    pub members: Option<String>,
    pub evidence: Option<String>,
    pub reds: Option<PathBuf>,
    pub after_pass: Option<PathBuf>,
    pub confirmed: bool,
    pub record: Option<PathBuf>,
    pub wt: Option<PathBuf>,
    pub base: Option<String>,
    pub batch: Option<String>,
    pub by: Option<String>,
    pub reason: Option<String>,
    pub all: bool,
    pub held_since: Option<u64>,
    pub out: Option<PathBuf>,
}

/// `argv` as the process was started: the program name, the command, its repo, then flags.
pub fn parse(argv: &[String]) -> Result<Opts, String> {
    let mut a = argv.iter().skip(1).cloned();
    let cmd = a.next().ok_or("missing command")?;
    let mut a = a.peekable();
    let repo = if a.peek().is_some_and(|f| f.starts_with("--")) { String::new() } else { a.next().unwrap_or_default() };
    let mut o = Opts { cmd, repo, ..Opts::default() };
    while let Some(f) = a.next() {
        let mut val = || a.next().ok_or(format!("{f} needs a value"));
        match f.as_str() {
            "--run" => o.run = Some(val()?.into()),
            "--db" => o.db = Some(val()?.into()),
            "--home" => o.home = Some(val()?.into()),
            "--round-vm" => o.round_vm = Some(val()?.into()),
            "--suites" => o.suites = Some(val()?),
            "--members" => o.members = Some(val()?),
            "--evidence" => o.evidence = Some(val()?),
            "--reds" => o.reds = Some(val()?.into()),
            "--after-pass" => o.after_pass = Some(val()?.into()),
            "--confirmed" => o.confirmed = true,
            "--record" => o.record = Some(val()?.into()),
            "--wt" => o.wt = Some(val()?.into()),
            "--base" => o.base = Some(val()?),
            "--batch" => o.batch = Some(val()?),
            "--repo" => o.repo = val()?,
            "--by" => o.by = Some(val()?),
            "--reason" => o.reason = Some(val()?),
            "--all" => o.all = true,
            "--out" => o.out = Some(val()?.into()),
            "--held-since" => o.held_since = Some(val()?.parse().map_err(|_| "--held-since takes epoch seconds".to_string())?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(o)
}

impl Opts {
    /// What no flag named, from the process's own environment: SPIRA_HOME is a per-copy fact
    /// (which checkout this runs from), SPIRA_RUN and SPIRA_DB are registered keys read through `cfg`.
    pub fn with_ambient(mut self) -> Result<Opts, String> {
        if self.home.is_none() {
            self.home = std::env::var_os("SPIRA_HOME").map(PathBuf::from);
        }
        for (slot, key) in [(&mut self.run, "SPIRA_RUN"), (&mut self.db, "SPIRA_DB")] {
            if slot.is_none() {
                let v = cfg(key)?;
                if !v.trim().is_empty() {
                    *slot = Some(PathBuf::from(v));
                }
            }
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("batcher").chain(rest.iter().copied()).map(String::from).collect()
    }

    #[test_id(ta1cm33)]
    #[test]
    fn an_attribute_argv_reads_back_as_the_flags_it_was_built_from() {
        let o = parse(&argv(&["attribute", "spira", "--members", "sp-a:t1", "--wt", "w", "--base", "b1", "--batch", "r9", "--confirmed", "--after-pass", "/p"])).unwrap();
        assert_eq!((o.cmd.as_str(), o.repo.as_str(), o.members.as_deref(), o.batch.as_deref()), ("attribute", "spira", Some("sp-a:t1"), Some("r9")));
        assert!(o.confirmed);
        assert_eq!(o.after_pass, Some(PathBuf::from("/p")));
    }

    #[test_id(t4ymrb1)]
    #[test]
    fn an_unknown_flag_or_a_flag_with_no_value_is_refused_by_name() {
        assert_eq!(parse(&argv(&["attribute", "spira", "--nope"])).unwrap_err(), "unknown flag --nope");
        assert_eq!(parse(&argv(&["attribute", "spira", "--wt"])).unwrap_err(), "--wt needs a value");
        assert_eq!(parse(&argv(&[])).unwrap_err(), "missing command");
    }
}
