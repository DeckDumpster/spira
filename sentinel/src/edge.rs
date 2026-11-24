//! The IO the scheduler's `sentinel-pass` reaches for through the sentinel library: CHECK 2b's
//! strand classification and its act (the strand library, in-process), and the incident event
//! line lib.sh's `spira_event` wrote (strand's in-process port of it).

/// strand's configuration from the one config door.
fn strand_cfg() -> Result<strand::config::Config, String> {
    strand::config::Config::resolve(&strand::config::Live::load())
}

/// The claimable set under one partition's predicate, from spira-claim in this process; a non-zero code is `Err`.
fn ready_in_process(labels: &str, exclude: &[String]) -> Result<std::collections::HashSet<String>, String> {
    let argv = ["ready-count".to_string(), labels.to_string(), exclude.join(","), "--json".to_string()];
    let o = crate::claim::dispatch_in(&argv, &mut std::io::empty(), &|k| std::env::var(k).ok());
    if o.code != 0 {
        return Err(format!("spira-claim ready-count {labels}: exit {}: {}", o.code, o.err.trim()));
    }
    strand::probe::ready_ids(&o.out).map_err(|e| format!("spira-claim ready-count {labels}: {e}"))
}

/// CHECK 2b's read: every stranded item strand would act on or escalate, `<part>\t<kind>\t<id>\t<detail>`.
/// `Err` is an input strand could not read: no "nothing stranded" is concluded from it.
pub fn strand_observe() -> Result<Vec<String>, String> {
    let cfg = strand_cfg()?;
    let c = strand::check::classify_live(&cfg, &ready_in_process)?;
    Ok(c.rows.iter().filter(|r| r.row.disp != strand::model::Disposition::Info).map(|r| format!("{}\t{}\t{}\t{}", r.part, r.row.kind, r.row.id, r.row.detail)).collect())
}

/// CHECK 2b's act: `strand check`, in-process — the episode state, the mechanical fixes and the
/// escalations, under strand's own lock.
pub fn strand_act() -> Result<String, String> {
    let cfg = strand_cfg()?;
    let c = strand::check::classify_live(&cfg, &ready_in_process)?;
    match strand::check::check(&cfg, c, false) {
        0 => Ok("strand check ran".into()),
        rc => Err(format!("strand check exited {rc}")),
    }
}

/// lib.sh `spira_event <kind> <target> <title> <detail>`: one cooled-down `events.log` line.
pub fn incident_event(kind: &str, target: &str, title: &str, detail: &str) -> Result<String, String> {
    let cfg = strand_cfg()?;
    strand::check::spira_event(&cfg, kind, target, title, detail);
    Ok(format!("{kind} {target}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tixs07u)]
    #[test]
    fn a_refused_ready_read_is_an_error_never_an_empty_set() {
        let t = testkit::TempDir::new("edge-ready");
        let lc = t.join("spira-lc");
        testkit::write_exe(&lc, "#!/bin/sh\necho refused >&2\nexit 1\n");
        let toml = spira_config::process::fixture_toml(t.path(), &[]);
        let _env = testkit::env(&[("SPIRA_TOML", toml.to_str()), ("SPIRA_LC_BIN", lc.to_str()), ("SPIRA_LC_SOCKET", None)]);
        let e = ready_in_process("spira,plan", &["x".to_string()]).unwrap_err();
        assert!(e.contains("ready-count spira,plan"), "{e}");
    }
}
