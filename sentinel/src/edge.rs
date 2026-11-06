//! The IO the scheduler's `sentinel-pass` reaches for through the sentinel library: CHECK 2b's
//! strand classification and its act (the strand library, in-process), and the incident event
//! line lib.sh's `spira_event` wrote (strand's in-process port of it).

/// strand's configuration from the one config door.
fn strand_cfg() -> Result<strand::config::Config, String> {
    strand::config::Config::resolve(&strand::config::Live::load())
}

/// CHECK 2b's read: every stranded item strand would act on or escalate, `<part>\t<kind>\t<id>\t<detail>`.
/// `Err` is an input strand could not read: no "nothing stranded" is concluded from it.
pub fn strand_observe() -> Result<Vec<String>, String> {
    let cfg = strand_cfg()?;
    let c = strand::check::classify_live(&cfg)?;
    Ok(c.rows.iter().filter(|r| r.row.disp != strand::model::Disposition::Info).map(|r| format!("{}\t{}\t{}\t{}", r.part, r.row.kind, r.row.id, r.row.detail)).collect())
}

/// CHECK 2b's act: `strand check`, in-process — the episode state, the mechanical fixes and the
/// escalations, under strand's own lock.
pub fn strand_act() -> Result<String, String> {
    let cfg = strand_cfg()?;
    let c = strand::check::classify_live(&cfg)?;
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
