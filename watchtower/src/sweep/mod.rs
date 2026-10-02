//! The sweep — default mode gathers, decides nominal/not, writes the prompt file and (when
//! not nominal) files the nine threshold escalations; `--show` gathers and prints, touching
//! nothing. Field-by-field source map lives on `SweepData` and in `render::render`'s doc
//! comment; the full contract is DESIGN.md §5.

pub mod cfg;
pub mod collect;
pub mod escalate;
pub mod render;

pub use cfg::Cfg;
pub use collect::collect;

use crate::log::log;

/// `--show`: gather and print, touch nothing.
pub fn run_show(now: i64, cfg: &Cfg) {
    let data = collect(now, cfg);
    print!("{}", render::render(&data));
}

/// Default mode: gather; if halted, skip entirely; if nominal, write the marker and return;
/// otherwise write the prompt file, advance the lapse marker, run the nine escalations and
/// the moot-sweep.
pub fn run_sweep(now: i64, cfg: &Cfg) {
    let data = collect(now, cfg);

    // A DELIBERATELY STOPPED WORLD MUST NOT FILE. Every pipeline metric grows
    // monotonically while nothing is wrong, so a sweep against a halted world describes a
    // system that looks broken when it is not. Gathered above (so `--show` still renders the
    // halt banner); only the sweep's write-and-file path skips.
    if let Some(halt) = &data.halt {
        log(&format!(
            "watchtower: halted since {} ({}) — sweep skipped",
            halt.since,
            halt.why.as_deref().unwrap_or("why unstated")
        ));
        return;
    }

    if !data.slow_probes.is_empty() {
        log(&format!("watchtower: slow probe(s) rendered ? — {}", data.slow_probes.join("; ")));
    }

    let prompt_file = cfg.prompt_file();
    let lapsed_marker = cfg.lapsed_marker();

    if render::is_nominal(&data, cfg.snap_stale_s) {
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
        return;
    }

    let rendered = render::render(&data);
    if atomic_write(&prompt_file, &rendered) {
        let _ = std::fs::write(&lapsed_marker, format!("{}\n", data.lapsed_now));
        log(&format!(
            "watchtower: swept — {}m since the last landing, {} unlanded, {} aeons, {} lapsed",
            data.since_land_disp, data.env.g("SP_UNLANDED_N"), data.aeons_live_disp, data.lapsed_count_disp
        ));
    } else {
        log(&format!(
            "watchtower: could not write the prompt file ({})",
            prompt_file.display()
        ));
        return;
    }

    escalate::run(&data, cfg);
}

fn atomic_write(path: &std::path::Path, body: &str) -> bool {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, body).is_err() {
        return false;
    }
    std::fs::rename(&tmp, path).is_ok()
}
