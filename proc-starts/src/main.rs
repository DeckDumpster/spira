//! `proc-starts | check-config` — runs until the active release changes, pushing the process-start
//! series every 20 s. Refuses to start without exactly CAP_BPF and CAP_PERFMON.

use ports::WallClock;
use proc_starts::{bpf::Kernel, caps, run};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("check-config") {
        std::process::exit(spira_config::process::check_config_verb("proc-starts", &[]));
    }
    if std::env::args().len() > 1 {
        eprintln!("usage: proc-starts | check-config");
        std::process::exit(2);
    }
    if let Err(e) = start() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn status() -> Result<String, String> {
    let path = format!("{}/self/status", system_paths::PROC);
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))
}

fn start() -> Result<(), String> {
    spira_config::process::check_config(&[])?;
    let release = spira_config::release_env::release_root_for_process()?.display().to_string();
    caps::check(&status()?, &release)?;
    let mut kernel = Kernel::load(release.clone())?;
    caps::drop_all()?;
    caps::check_dropped(&status()?)?;
    let started_on = std::fs::read_link(&release).ok().map(|p| p.display().to_string());
    run::run(&mut kernel, &WallClock, started_on);
    Ok(())
}
