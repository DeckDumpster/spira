//! The sweep — default mode gathers, decides nominal/not, writes the prompt file and (when
//! not nominal) files the nine threshold escalations; `--show` gathers and prints, touching
//! nothing. Field-by-field source map lives on `SweepData` and in `render::render`'s doc
//! comment; the full contract is DESIGN.md §5.

pub mod cfg;
pub mod collect;
pub mod escalate;
pub mod moot;
pub mod render;

pub use cfg::Cfg;
pub use collect::{collect, SweepData};

use crate::log::log;

/// `--show`: gather and print, touch nothing.
pub fn run_show(now: i64, cfg: &Cfg) {
    let data = collect(now, cfg);
    print!("{}", render::render(&data));
}

/// Default mode: gather, write the report, and when the report asked for them run the escalations.
pub fn run_sweep(now: i64, cfg: &Cfg) {
    let data = collect(now, cfg);
    if write_report(&data, cfg) {
        escalate::run(&data, cfg);
    }
}

/// What the sweep writes for `data`: nothing when halted; the one-line `SWEEP:NOMINAL` when
/// every field is green; otherwise the rendered snapshot. The lapse marker advances only after a
/// write that landed. True when the snapshot was written and the escalations should follow.
pub fn write_report(data: &SweepData, cfg: &Cfg) -> bool {
    // A DELIBERATELY STOPPED WORLD MUST NOT FILE: every pipeline metric grows while nothing is
    // wrong, so a sweep against a halted world describes a system that looks broken when it is not.
    if let Some(halt) = &data.halt {
        log(&format!(
            "watchtower: halted since {} ({}) — sweep skipped",
            halt.since,
            halt.why.as_deref().unwrap_or("why unstated")
        ));
        return false;
    }

    if !data.slow_probes.is_empty() {
        log(&format!("watchtower: slow probe(s) rendered ? — {}", data.slow_probes.join("; ")));
    }

    let prompt_file = cfg.prompt_file();
    let lapsed_marker = cfg.lapsed_marker();

    if render::is_nominal(data, cfg.snap_stale_s) {
        let body = format!(
            "SWEEP:NOMINAL lapsed={} snap_age={}s nv_worst={}\n",
            data.lapsed_count_disp, data.snap_age_disp_raw, data.nv_worst
        );
        atomic_write(&prompt_file, &body);
        let _ = std::fs::write(&lapsed_marker, format!("{}\n", data.lapsed_now));
        log(&format!(
            "watchtower: nominal — no sweep needed (lapsed={} snap_age={}s nv_worst={})",
            data.lapsed_count_disp, data.snap_age_disp_raw, data.nv_worst
        ));
        return false;
    }

    let rendered = render::render(data);
    if !atomic_write(&prompt_file, &rendered) {
        log(&format!(
            "watchtower: could not write the prompt file ({})",
            prompt_file.display()
        ));
        return false;
    }
    let _ = std::fs::write(&lapsed_marker, format!("{}\n", data.lapsed_now));
    log(&format!(
        "watchtower: swept — {}m since the last landing, {} unlanded, {} dwarves, {} lapsed{}",
        data.since_land_disp,
        data.env.g("SP_UNLANDED_N"),
        data.dwarves_live_disp,
        data.lapsed_count_disp,
        slow_suffix(&data.slow_probes)
    ));
    true
}

fn slow_suffix(slow: &[String]) -> String {
    if slow.is_empty() {
        String::new()
    } else {
        format!(" (timed out: {})", slow.join("; "))
    }
}

fn atomic_write(path: &std::path::Path, body: &str) -> bool {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, body).is_err() {
        return false;
    }
    std::fs::rename(&tmp, path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{slow_suffix, write_report, SweepData};
    use crate::sweep::collect::{Drain, Halt};
    use crate::sweep::Cfg;

    #[test]
    fn the_sweep_line_names_the_probes_that_timed_out() {
        assert_eq!(slow_suffix(&[]), "");
        assert_eq!(slow_suffix(&["yield (>30s)".into(), "suites status (>30s)".into()]), " (timed out: yield (>30s); suites status (>30s))");
    }

    fn world(tag: &str) -> (testkit::TempDir, Cfg) {
        let d = testkit::TempDir::new(tag);
        let cfg = Cfg::fixture(d.path(), "inc".into());
        (d, cfg)
    }

    fn written(cfg: &Cfg) -> Option<String> {
        std::fs::read_to_string(cfg.prompt_file()).ok()
    }

    fn marker(cfg: &Cfg) -> Option<String> {
        std::fs::read_to_string(cfg.lapsed_marker()).ok()
    }

    fn red() -> SweepData {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.snap_age = None;
        d
    }

    // covers: UC-ops-detection-remediation-27
    #[test]
    fn nominal_is_written_only_when_every_field_is_green_and_asks_for_no_escalation() {
        let (_d, cfg) = world("wt-sweep-nominal");
        assert!(!write_report(&SweepData::fixture_nominal(1_700_000_000), &cfg));
        assert!(written(&cfg).unwrap().starts_with("SWEEP:NOMINAL"));
        let (_d, cfg) = world("wt-sweep-red");
        assert!(write_report(&red(), &cfg), "a red field writes the snapshot and asks for the escalations");
        let body = written(&cfg).unwrap();
        assert!(!body.contains("SWEEP:NOMINAL") && body.contains("N workers pull"), "{body}");
    }

    // covers: UC-ops-detection-remediation-27
    #[test]
    fn a_halted_world_writes_nothing_and_asks_for_nothing() {
        let (_d, cfg) = world("wt-sweep-halted");
        let mut data = red();
        data.halt = Some(Halt { since: "2026-09-08T01:23:45Z".into(), why: Some("deliberate".into()) });
        assert!(!write_report(&data, &cfg));
        assert_eq!((written(&cfg), marker(&cfg)), (None, None));
    }

    // covers: UC-ops-detection-remediation-27
    #[test]
    fn a_draining_world_still_writes_the_prompt() {
        let (_d, cfg) = world("wt-sweep-draining");
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.drain = Drain::Draining { since: "2026-09-08 20:02:00 UTC".into(), mins: Some(5) };
        assert!(write_report(&data, &cfg));
        assert!(written(&cfg).unwrap().contains("DRAINING"));
    }

    // covers: UC-ops-detection-remediation-27
    #[test]
    fn the_lapse_marker_advances_with_a_write_that_landed_and_not_with_one_that_failed() {
        let (_d, cfg) = world("wt-sweep-marker");
        assert_eq!(marker(&cfg), None);
        write_report(&red(), &cfg);
        assert_eq!(marker(&cfg).as_deref(), Some("20260930T000000Z\n"));

        let (d, mut cfg) = world("wt-sweep-marker-fail");
        cfg.prompt_file = Some(d.join("no-such-dir/prompt"));
        assert!(!write_report(&red(), &cfg), "an unwritable prompt file asks for no escalation");
        assert_eq!(marker(&cfg), None, "so its lapses are still new at the next sweep");
    }
}
