//! `layout up|down|status|ensure [--window <target>]` — see `src/layout.rs`.

use std::process::ExitCode;

use cockpit_ops::layout::{Conf, Layout};
use cockpit_ops::tmux::Tmux;

fn default_window(tmux: &Tmux) -> String {
    if let Ok(me) = std::env::var("TMUX_PANE") {
        if let Some(w) = tmux.stdout(&["display-message", "-p", "-t", &me, "#{session_name}:#{window_index}"]) {
            if !w.is_empty() {
                return w;
            }
        }
    }
    if let Some(list) = tmux.stdout(&["list-panes", "-a", "-F", "#{session_name}:#{window_index} #{pane_current_command}"]) {
        for line in list.lines() {
            let mut it = line.split_whitespace();
            let (Some(target), Some(cmd)) = (it.next(), it.next()) else { continue };
            if cmd == "claude" {
                return target.to_string();
            }
        }
    }
    "cockpit:2".to_string()
}

/// `tmux-env.sh scrub`, unchanged bash — every pane this binary opens inherits the tmux
/// SERVER's environment, which may still carry a session identity from whoever forked it.
fn scrub_tmux_env(cock: &std::path::Path) {
    let script = cock.join("tmux-env.sh");
    if script.is_file() {
        let _ = spira_config::bounded::bounded("bash").arg(&script).arg("scrub").output();
    }
}

fn main() -> ExitCode {
    cockpit_ops::conf::self_source();

    let mut args = std::env::args().skip(1);
    let action = args.next().unwrap_or_else(|| "status".to_string());
    let mut window: Option<String> = None;
    let rest: Vec<String> = args.collect();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--window" {
            if let Some(w) = rest.get(i + 1) {
                window = Some(w.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    let conf = match Conf::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let tmux = Tmux::new();

    if matches!(action.as_str(), "up" | "ensure") {
        scrub_tmux_env(&conf.cock);
    }

    let window = window.unwrap_or_else(|| default_window(&tmux));
    let layout = Layout { tmux, conf };

    match action.as_str() {
        "up" => match layout.up(&window) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        },
        "down" => match layout.down(&window) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        },
        "status" => {
            println!("{}", layout.status(&window));
            ExitCode::SUCCESS
        }
        "ensure" => {
            let installed = std::path::Path::new(&layout.conf.spira_release).join("bin").join("layout");
            let installed = std::fs::canonicalize(&installed).unwrap_or(installed);
            let self_bin = std::env::current_exe().unwrap_or_else(|_| "layout".into());
            match layout.ensure(&installed, &self_bin) {
                Ok(msg) => {
                    if !msg.is_empty() {
                        println!("{msg}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::SUCCESS // refusal is exit 0, matching layout.sh's own `exit 0`
                }
            }
        }
        _ => {
            eprintln!("usage: layout up [--window <target>] | down | status | ensure");
            ExitCode::from(1)
        }
    }
}
