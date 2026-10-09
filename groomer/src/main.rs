//! groomer — graph hygiene operations for the Spira DAG. Replaces spira/groomer.sh and
//! spira/groomer-litter-predicate.py (DESIGN.md).
//!
//!   groomer split-piece    <original-id> [bd create args...]              file one piece of a split, on its own branch
//!   groomer supersede      <id> --with <successor>                        mark a bead superseded by another
//!   groomer close          <id> --evidence <text>                         close a bead whose premise is gone
//!   groomer correct-lane   <id> --lane <lane>                             correct a mislabelled lane label
//!   groomer depends-on-fix <bug-id> --fix <id> --evidence <text>         link bug to in-flight fix, order accordingly
//!   groomer unpoison       <id> --cause <c> --evidence <text>            credit a harness-caused attempt, lift spira-poison
//!   groomer triage-poison  <id> --verdict <work-fault|drop> --evidence <text>  close out a work-caused poison charge
//!   groomer deadlocked     [--apply]                                      lift poison from finished, landable work (spira-claim/DESIGN.md §9)
//!   groomer unwanted       ...                                            REFUSED — exits 2 always
//!
//! EXIT: 0 success, 1 usage error / missing required argument, 2 refused (groomer policy).

use groomer::bd::RealBd;
use groomer::seam::{locate_home, LibSeam};
use groomer::{cmds, deadlocked, litter, unpoison};

fn usage() -> ! {
    eprintln!("usage: groomer split-piece|supersede|close|correct-lane|depends-on-fix|unpoison|triage-poison|deadlocked|unwanted ...");
    std::process::exit(1);
}

/// `--flag <value>` pairs mixed with positionals, in the order `groomer.sh`'s own
/// per-subcommand loops read them. Returns `Err` with the same "unknown option" /
/// "requires a value" wording the bash used.
struct Flags {
    values: std::collections::HashMap<String, String>,
}

fn parse_flags(sub: &str, args: &[String], known: &[&str]) -> Result<Flags, String> {
    let mut values = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if known.contains(&a.as_str()) {
            if i + 1 >= args.len() {
                return Err(format!("groomer: {a} requires a value"));
            }
            values.insert(a.clone(), args[i + 1].clone());
            i += 2;
        } else {
            return Err(format!("groomer: {sub}: unknown option: {a}"));
        }
    }
    Ok(Flags { values })
}

fn die(msg: &str) -> ! {
    eprintln!("groomer: {msg}");
    std::process::exit(1);
}

fn conf_env() -> (String, String) {
    let bd = spira_config::process::cfg("SPIRA_BD").unwrap_or_else(|e| die(&e));
    let db = spira_config::process::cfg("SPIRA_DB").unwrap_or_else(|e| die(&e));
    (bd, db)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.clone(), r.to_vec()),
        None => usage(),
    };

    let (bd_bin, db) = conf_env();
    let bd = RealBd { bd: bd_bin, db };

    let exe = std::env::current_exe().unwrap_or_else(|_| "groomer".into());
    let home = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe);

    match cmd.as_str() {
        "split-piece" => {
            let Some((id, extra)) = rest.split_first() else {
                die("split-piece: original bead id required");
            };
            let Some(home) = home else {
                die("cannot find lib.sh (set SPIRA_HOME)");
            };
            let seam = LibSeam::new(home.join("lib.sh"));
            match cmds::split_piece(&bd, &seam, id, extra) {
                Ok(out) => print!("{out}"),
                Err((code, msg)) => {
                    eprintln!("groomer: {msg}");
                    std::process::exit(code);
                }
            }
        }
        "supersede" => {
            let Some((id, tail)) = rest.split_first() else {
                die("supersede: bead id required");
            };
            let flags = parse_flags("supersede", tail, &["--with"]).unwrap_or_else(|e| die_str(&e));
            let with = flags.values.get("--with").cloned().unwrap_or_default();
            run_cmd(cmds::supersede(&bd, id, &with));
        }
        "close" => {
            let Some((id, tail)) = rest.split_first() else {
                die("close: bead id required");
            };
            let flags = parse_flags("close", tail, &["--evidence"]).unwrap_or_else(|e| die_str(&e));
            let evidence = flags.values.get("--evidence").cloned().unwrap_or_default();
            run_cmd(cmds::close(&bd, id, &evidence));
        }
        "correct-lane" => {
            let Some((id, tail)) = rest.split_first() else {
                die("correct-lane: bead id required");
            };
            let flags = parse_flags("correct-lane", tail, &["--lane"]).unwrap_or_else(|e| die_str(&e));
            let lane = flags.values.get("--lane").cloned().unwrap_or_default();
            run_cmd(cmds::correct_lane(&bd, id, &lane));
        }
        "depends-on-fix" => {
            let Some((bug_id, tail)) = rest.split_first() else {
                die("depends-on-fix: bug id required");
            };
            let flags = parse_flags("depends-on-fix", tail, &["--fix", "--evidence"]).unwrap_or_else(|e| die_str(&e));
            let fix_id = flags.values.get("--fix").cloned().unwrap_or_default();
            let evidence = flags.values.get("--evidence").cloned().unwrap_or_default();
            run_cmd(cmds::depends_on_fix(&bd, bug_id, &fix_id, &evidence));
        }
        "unpoison" => {
            let Some((id, tail)) = rest.split_first() else {
                die("unpoison: bead id required");
            };
            let flags = parse_flags("unpoison", tail, &["--cause", "--evidence"]).unwrap_or_else(|e| die_str(&e));
            let opts = unpoison::parse(id, flags.values.get("--cause").map(String::as_str), flags.values.get("--evidence").map(String::as_str)).unwrap_or_else(|e| die(&e));
            let (code, out) = unpoison::run("spira-claim", &opts);
            print!("{out}");
            std::process::exit(code);
        }
        "triage-poison" => {
            let Some((id, tail)) = rest.split_first() else {
                die("triage-poison: bead id required");
            };
            let flags = parse_flags("triage-poison", tail, &["--verdict", "--evidence"]).unwrap_or_else(|e| die_str(&e));
            let verdict = flags.values.get("--verdict").cloned().unwrap_or_default();
            let evidence = flags.values.get("--evidence").cloned().unwrap_or_default();
            let Some(home) = home else {
                die("cannot find lib.sh (set SPIRA_HOME)");
            };
            let seam = LibSeam::new(home.join("lib.sh"));
            match cmds::triage_poison(&bd, &seam, id, &verdict, &evidence) {
                Ok(out) => print!("{out}"),
                Err((code, msg)) => {
                    eprintln!("groomer: {msg}");
                    std::process::exit(code);
                }
            }
        }
        "deadlocked" => {
            let apply = rest.iter().any(|a| a == "--apply");
            for a in &rest {
                if a != "--apply" {
                    eprintln!("groomer: deadlocked: unknown option: {a}");
                    std::process::exit(1);
                }
            }
            let Some(home) = home else {
                die("cannot find lib.sh (set SPIRA_HOME)");
            };
            let seam = LibSeam::new(home.join("lib.sh"));
            let (code, out) = deadlocked::run(&seam, &bd.db, apply, "spira-claim");
            print!("{out}");
            std::process::exit(code);
        }
        "unwanted" => match cmds::unwanted() {
            Err((code, msg)) => {
                eprintln!("{msg}");
                std::process::exit(code);
            }
            Ok(_) => unreachable!("unwanted always refuses"),
        },
        "__litter" => {
            // Internal: `bd show <id> --json | groomer __litter` — the litter predicate,
            // for a caller (or a test) that wants it standalone rather than through sweep.
            let mut raw = String::new();
            use std::io::Read;
            let _ = std::io::stdin().read_to_string(&mut raw);
            let v = litter::judge_text(&raw);
            println!("HAS_CONTENT {}", i32::from(v.has_content));
            println!("META {}", v.meta);
        }
        _ => usage(),
    }
}

fn die_str(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn run_cmd(r: cmds::CmdResult) {
    match r {
        Ok(out) => print!("{out}"),
        Err((code, msg)) => {
            eprintln!("groomer: {msg}");
            std::process::exit(code);
        }
    }
}
