//! `watchd` — the watcher manifest, and the face over the logs and cursors systemd fills.
//! See `DESIGN.md` for the contract. Replaces `spira/watchd.sh` (sp-48f6g).

mod commands;
mod context;
mod cursor;
mod filter;
mod fs_ops;
mod health;
mod iso8601;
mod lock;
mod manifest;
mod notify;
mod ops;
mod paths;
mod rows;
mod tail;

use manifest::Row;
use ops::Real;

fn usage() -> &'static str {
    "usage: watchd manifest|units|keys|exec <name>|status|drain [name] [--all]|peek [name] [--all] [--limit N]|tail <name> [--all] [--takeover] [--from-start]|tailers|restart [name]|notify|prune|health-ids <file>|health-view <program> <session>"
}

fn load_context() -> context::Context {
    match context::load(&context::home_dir()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn need_rows(ctx: &context::Context) -> Vec<Row> {
    match commands::load_rows_or_report(ctx) {
        Ok(rows) => rows,
        Err(code) => std::process::exit(code),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
    let rest = &args[args.len().min(2)..];

    let code = match sub {
        "manifest" => {
            let ctx = load_context();
            let rows = need_rows(&ctx);
            commands::cmd_manifest(&rows);
            0
        }
        "units" => {
            let ctx = load_context();
            let rows = need_rows(&ctx);
            commands::cmd_units(&rows, &ctx.instance);
            0
        }
        "keys" => {
            commands::cmd_keys();
            0
        }
        "exec" => {
            let Some(name) = rest.first() else {
                eprintln!("usage: watchd exec <name>");
                std::process::exit(2);
            };
            let ctx = load_context();
            let rows = need_rows(&ctx);
            commands::cmd_exec(&rows, name, &ctx.run)
        }
        "status" => {
            let ctx = load_context();
            let real = Real::new(ctx.systemctl.clone());
            let rows = need_rows(&ctx);
            let halted = std::path::Path::new(&ctx.run).join("world.halted").exists();
            print!("{}", commands::cmd_status(&rows, &real, &ctx, halted));
            0
        }
        "drain" | "peek" => {
            let ctx = load_context();
            let rows = need_rows(&ctx);
            let peek_forced = sub == "peek";
            let parsed = parse_reader_args(rest, peek_forced);
            let args = match parsed {
                Ok(a) => a,
                Err((msg, code)) => {
                    eprintln!("watchd: {msg}");
                    std::process::exit(code);
                }
            };
            let filter = if args.all {
                None
            } else {
                match filter::Filter::compile(&ctx.actionable) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        eprintln!("watchd: {e}");
                        std::process::exit(2);
                    }
                }
            };
            match commands::cmd_drain(
                &rows,
                &ctx,
                filter.as_ref(),
                &commands::DrainArgs { name: args.name.as_deref(), all: args.all, peek: args.peek, limit: args.limit },
            ) {
                Ok(out) => {
                    print!("{out}");
                    0
                }
                Err(code) => code,
            }
        }
        "tail" => {
            let ctx = load_context();
            let rows = need_rows(&ctx);
            let parsed = parse_reader_args(rest, false);
            let args = match parsed {
                Ok(a) => a,
                Err((msg, code)) => {
                    eprintln!("watchd: {msg}");
                    std::process::exit(code);
                }
            };
            let Some(name) = args.name else {
                eprintln!("usage: watchd tail <name> [--all] [--from-start]");
                std::process::exit(2);
            };
            if args.limit != 0 {
                eprintln!("watchd: --limit only applies to 'peek' — a capped read that marks the capped lines read would lose them");
                std::process::exit(2);
            }
            tail::install_signal_handlers();
            tail::cmd_tail(
                &rows,
                &ctx.run,
                &ctx.conf_file,
                &ctx.actionable,
                ctx.now,
                &tail::TailArgs { name: &name, all: args.all, takeover: args.takeover, from_start: args.from_start },
            )
        }
        "tailers" => {
            let ctx = load_context();
            let rows = need_rows(&ctx);
            print!("{}", commands::cmd_tailers(&rows, &ctx.run));
            0
        }
        "restart" => {
            let ctx = load_context();
            let real = Real::new(ctx.systemctl.clone());
            let rows = need_rows(&ctx);
            match commands::cmd_restart(&rows, &real, &ctx, rest.first().map(|s| s.as_str())) {
                Ok(out) => {
                    print!("{out}");
                    0
                }
                Err(code) => code,
            }
        }
        "notify" => {
            if !rest.is_empty() {
                eprintln!("usage: watchd notify");
                std::process::exit(3);
            }
            let ctx = load_context();
            let real = Real::new(ctx.systemctl.clone());
            let rows = need_rows(&ctx);
            match notify::cmd_notify(&rows, &real, &ctx) {
                Ok(out) => {
                    print!("{}", out.report);
                    out.code
                }
                Err(e) => {
                    eprintln!("{e}");
                    3
                }
            }
        }
        "prune" => {
            let ctx = load_context();
            let real = Real::new(ctx.systemctl.clone());
            let rows = need_rows(&ctx);
            let unit_dir = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".config/systemd/user");
            print!("{}", commands::cmd_prune(&rows, &real, &ctx, &unit_dir));
            0
        }
        "health-ids" => {
            let Some(file) = rest.first() else {
                eprintln!("usage: watchd health-ids <file>");
                std::process::exit(2);
            };
            let ctx = load_context();
            commands::cmd_health_ids(&ctx, file)
        }
        "health-view" => {
            let (Some(prog), Some(sess)) = (rest.first(), rest.get(1)) else {
                eprintln!("usage: watchd health-view <program> <session>");
                std::process::exit(2);
            };
            commands::cmd_health_view(prog, sess)
        }
        _ => {
            eprintln!("{}", usage());
            2
        }
    };
    std::process::exit(code);
}

struct ReaderArgs {
    name: Option<String>,
    all: bool,
    peek: bool,
    takeover: bool,
    from_start: bool,
    limit: u64,
}

/// The one option parser `drain`, `peek` and `tail` share (`_wd_args`), so `--all` cannot
/// mean one thing to one and another to another.
fn parse_reader_args(args: &[String], peek_forced: bool) -> Result<ReaderArgs, (String, i32)> {
    let mut name = None;
    let mut all = false;
    let mut peek = peek_forced;
    let mut takeover = false;
    let mut from_start = false;
    let mut limit = 0u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--peek" => peek = true,
            "--takeover" => takeover = true,
            "--from-start" => from_start = true,
            "--limit" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| ("--limit needs a whole number of lines, got ''".to_string(), 2))?;
                limit = v.parse().map_err(|_| (format!("--limit needs a whole number of lines, got '{v}'"), 2))?;
            }
            s if s.starts_with("--limit=") => {
                let v = &s["--limit=".len()..];
                limit = v.parse().map_err(|_| (format!("--limit needs a whole number of lines, got '{v}'"), 2))?;
            }
            s if s.starts_with('-') => return Err((format!("unknown option '{s}'"), 2)),
            s => {
                if name.is_some() {
                    return Err((format!("one watcher at a time, got '{}' and '{s}'", name.unwrap()), 2));
                }
                name = Some(s.to_string());
            }
        }
        i += 1;
    }
    if limit != 0 && !peek {
        return Err(("--limit only applies to 'peek' — a capped read that marks the capped lines read would lose them".to_string(), 2));
    }
    if takeover && peek {
        return Err(("--takeover only applies to 'tail' — nothing else holds a reader lock".to_string(), 2));
    }
    if from_start && peek {
        return Err(("--from-start only applies to 'tail' — peek already reads from the cursor without moving it".to_string(), 2));
    }
    Ok(ReaderArgs { name, all, peek, takeover, from_start, limit })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_bare_name_parses() {
        let r = parse_reader_args(&a(&["pool"]), false).unwrap();
        assert_eq!(r.name, Some("pool".to_string()));
        assert!(!r.all && !r.peek);
    }

    #[test]
    fn two_names_is_refused() {
        assert!(parse_reader_args(&a(&["pool", "mail"]), false).is_err());
    }

    #[test]
    fn an_unknown_option_is_refused() {
        assert!(parse_reader_args(&a(&["--nope"]), false).is_err());
    }

    #[test]
    fn limit_without_peek_is_refused() {
        assert!(parse_reader_args(&a(&["--limit", "5"]), false).is_err());
    }

    #[test]
    fn limit_with_peek_is_accepted() {
        let r = parse_reader_args(&a(&["--limit", "5"]), true).unwrap();
        assert_eq!(r.limit, 5);
    }

    #[test]
    fn takeover_with_peek_is_refused() {
        assert!(parse_reader_args(&a(&["--takeover"]), true).is_err());
    }

    #[test]
    fn from_start_with_peek_is_refused() {
        assert!(parse_reader_args(&a(&["--from-start"]), true).is_err());
    }

    #[test]
    fn limit_equals_form_parses() {
        let r = parse_reader_args(&a(&["--limit=3"]), true).unwrap();
        assert_eq!(r.limit, 3);
    }
}
