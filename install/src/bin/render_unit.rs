//! `render-unit` — a drop-in replacement for `systemd/render.py`'s CLI (sp-31dm0: render.py
//! is retired; `install::values::render` does the actual work, shared with `units-install`
//! and `unit-ensure` via `install::bootstrap`). Renders one template against an explicit,
//! fully-named set of values — no manifest, no environment reads — the ad-hoc/test use
//! render.py itself served.
//!
//! usage: render-unit <template> --home H --repo R --run RUN --db DB --cockpit C
//!            --dolt-data D --testdb-data T --dolt DOLT --prod P --instance I
//!            --testdb-port PORT --snap-stale-s S [--watcher-name W] [--path-tail TAIL] [--lc-password-file F]

use install::values::{render_file, HostValues};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(template) = args.next() else {
        eprintln!("usage: render-unit <template> --home H --repo R ...");
        return ExitCode::from(2);
    };
    let mut host = HostValues::default();
    let mut watcher: Option<String> = None;
    let mut rest: Vec<String> = args.collect();
    let mut i = 0;
    while i < rest.len() {
        let key = rest[i].clone();
        let val = rest.get(i + 1).cloned().unwrap_or_default();
        match key.as_str() {
            "--home" => host.home = val,
            "--repo" => host.repo = val,
            "--run" => host.run = val,
            "--db" => host.db = val,
            "--cockpit" => host.cockpit = val,
            "--dolt-data" => host.dolt_data = val,
            "--testdb-data" => host.testdb_data = val,
            "--dolt" => host.dolt = val,
            "--prod" => host.prod = val,
            "--instance" => host.instance = val,
            "--testdb-port" => host.testdb_port = val,
            "--snap-stale-s" => host.snap_stale_s = val,
            "--watchtower-start-timeout-s" => host.watchtower_start_timeout_s = val,
            "--watcher-name" => watcher = if val.is_empty() { None } else { Some(val) },
            "--path-tail" => host.path_tail = val,
            "--lc-password-file" => host.lc_password_file = val,
            _ => {
                eprintln!("render-unit: unknown flag: {key}");
                return ExitCode::from(2);
            }
        }
        i += 2;
    }
    rest.clear();

    match render_file(&PathBuf::from(&template), &host, watcher.as_deref()) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render: {e}");
            ExitCode::from(1)
        }
    }
}
