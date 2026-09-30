//! `rebuild [probe|--probe] [--force] [-h|--help]` — see `src/rebuild.rs`.

use std::process::ExitCode;

use cockpit_ops::rebuild::{from_env, RunOutcome};
use cockpit_ops::tmux::Tmux;

const HELP: &str = "\
  rebuild probe        say what is wrong and change nothing
  rebuild              do it: clear a dead server, make the sessions, build the cockpit
  rebuild --force      also clear a wedged server that still holds live panes
";

fn main() -> ExitCode {
    cockpit_ops::conf::self_source();

    let mut probe_only = false;
    let mut force = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "probe" | "--probe" => probe_only = true,
            "--force" => force = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("rebuild: unknown argument '{other}'");
                return ExitCode::from(64);
            }
        }
    }

    let rebuild = from_env(Tmux::new(), force);
    match rebuild.run(probe_only) {
        RunOutcome::Probe(lines) => {
            for l in lines {
                println!("{l}");
            }
            ExitCode::SUCCESS
        }
        RunOutcome::RefusedUnusableSocket => {
            eprintln!("rebuild: the socket path is unusable — 'tmux list-sessions' says the file name is too long.");
            eprintln!("rebuild: that is TMUX_TMPDIR, not the server. Nothing here can fix it; shorten the path.");
            ExitCode::from(2)
        }
        RunOutcome::RefusedLiveDescendants { pid, live } => {
            eprintln!("rebuild: REFUSING to kill pid {pid} — it still holds {live} live process(es).");
            eprintln!("rebuild: that is somebody's unsaved work, not a corpse. Look first:");
            eprintln!("rebuild:     ps --ppid {pid} -o pid,etime,args");
            eprintln!("rebuild: Then re-run with --force if you are sure.");
            ExitCode::from(3)
        }
        RunOutcome::KillSurvived { pid } => {
            eprintln!("rebuild: pid {pid} survived KILL — cannot continue");
            ExitCode::from(1)
        }
        RunOutcome::SessionCreateFailed { session } => {
            eprintln!("rebuild: could not create session '{session}'");
            ExitCode::from(1)
        }
        RunOutcome::LayoutFailed => {
            eprintln!("rebuild: layout up failed");
            ExitCode::from(1)
        }
        RunOutcome::Ok { verify_fail, output } => {
            for l in output {
                println!("{l}");
            }
            println!();
            if verify_fail == 0 {
                println!("cockpit rebuilt. Attach with:  tmux attach -t cockpit");
                println!("A watcher's Monitor cannot be started by a script — re-attach them in the session:");
                println!("    Monitor: watchd tail answers");
                println!("    Monitor: watchd tail view");
                ExitCode::SUCCESS
            } else {
                eprintln!("rebuild: {verify_fail} check(s) failed — the cockpit is NOT fully rebuilt");
                ExitCode::from(1)
            }
        }
    }
}
