//! `mail` — CLI shell. Every subcommand's actual behaviour lives in `cmds`, `tidy` or
//! `sendmail`; this file only parses argv, wires up the real `Bd`, and maps results to
//! stdout/stderr/exit code. Replaces spira/mail.sh (sp-ooh1k).
//!
//!   mail send <mailbox> --from "<s>" --subject "<s>" [--kind K] [--urgent]
//!                       [--default D] [--class permissions|policy|destructive] [--bead ID] [--digest] [--dry-run] < body
//!   mail template <kind>
//!   mail list <mailbox> [--unread]
//!   mail read <mailbox> [<message>]
//!   mail count <mailbox>
//!   mail unread-age <mailbox>
//!   mail sendmail                     RFC 5322 on stdin
//!   mail tidy <mailbox> [--dry-run]
//!   mail ensure <mailbox>
//!   mail sweep-dismissed [mailbox]

use std::io::{self, Read, Write};
use std::process::ExitCode;

use mail::bead::BdCli;
use mail::cmds::{self, SendArgs};
use mail::env::Env;
use mail::lc::LcCli;
use mail::{sendmail, tidy};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    let cmd = it.next().map(String::as_str).unwrap_or("");
    let rest: Vec<String> = it.cloned().collect();

    match cmd {
        "send" => run_send(&rest),
        "template" => run_template(&rest),
        "list" => run_list(&rest),
        "read" => run_read(&rest),
        "count" => run_count(&rest),
        "unread-age" => run_unread_age(&rest),
        "done" => run_done(&rest),
        "sendmail" => run_sendmail(&rest),
        "tidy" => run_tidy(&rest),
        "ensure" => run_ensure(&rest),
        "sweep-dismissed" => run_sweep_dismissed(&rest),
        other => {
            eprintln!("mail: unknown command: {other}");
            ExitCode::FAILURE
        }
    }
}

fn bd_cli(env: &Env) -> BdCli {
    BdCli { bin: env.bd_bin.clone(), db: env.db.clone(), conn_retries: env.bd_conn_retries }
}

fn fail(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("mail: {}", msg.as_ref());
    ExitCode::FAILURE
}

fn run_send(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let mut from: Option<String> = None;
    let mut subject = String::new();
    let mut kind = String::new();
    let mut default = String::new();
    let mut class = String::new();
    let mut bead = String::new();
    let mut urgent = false;
    let mut digest = false;
    let mut dry_run = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from = args.get(i + 1).cloned();
                i += 2;
            }
            "--subject" => {
                subject = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--kind" => {
                kind = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--default" => {
                default = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--class" => {
                class = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--bead" => {
                bead = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--urgent" => {
                urgent = true;
                i += 1;
            }
            "--digest" => {
                digest = true;
                i += 1;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            other => {
                eprintln!("mail send: unknown option: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    if let Err(e) = mail::maildir::mailbox_valid(&mailbox) {
        return fail(format!("send: {e}"));
    }

    let budget = env.loom_budget_ms;
    let body = match cmds::read_body_deadline(budget) {
        Ok(b) => b,
        Err(e) => return fail(format!("send: {e}")),
    };

    let bd = bd_cli(&env);
    let send_args = SendArgs { mailbox: &mailbox, from: from.as_deref(), subject: &subject, kind: &kind, default: &default, class: &class, bead: &bead, urgent, digest, dry_run };
    match cmds::send(&bd, &env, &send_args, body) {
        Ok(out) => {
            if dry_run {
                for step in &out.steps {
                    println!("mail send --dry-run: {step}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("send: {e}")),
    }
}

fn run_template(args: &[String]) -> ExitCode {
    let kind = args.first().cloned().unwrap_or_default();
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::template(&env.kinds_dir, &kind) {
        Ok(t) => {
            println!("{t}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("template: {e}")),
    }
}

fn run_list(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let unread_only = args.get(1).map(String::as_str) == Some("--unread");
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::list(&env, &mailbox, unread_only) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("list: {e}")),
    }
}

fn run_read(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let msg = args.get(1).map(String::as_str);
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::read_cmd(&env, &mailbox, msg) {
        Ok(content) => {
            print!("{content}");
            let _ = io::stdout().flush();
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("read: {e}")),
    }
}

fn run_count(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::count(&env, &mailbox) {
        Ok(n) => {
            println!("{n}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("count: {e}")),
    }
}

fn run_unread_age(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::unread_age(&env, &mailbox) {
        Ok(Some(secs)) => {
            println!("{secs}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => fail(format!("unread-age: {e}")),
    }
}

fn run_done(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let msgid = args.get(1).cloned().unwrap_or_default();
    let note = if args.len() > 2 { Some(args[2..].join(" ")) } else { None };
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    match cmds::done(&env, &mailbox, &msgid, note.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(format!("done: {e}")),
    }
}

fn run_sendmail(_args: &[String]) -> ExitCode {
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return fail("sendmail: could not read stdin");
    }
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    let bd = bd_cli(&env);
    let db_configured = !env.db.is_empty();
    let spool = sendmail::spool_message(&env.run_dir, &raw);
    match sendmail::sendmail(&bd, &LcCli, db_configured, &env.home, &env.mail_root, env.mute, &raw) {
        Ok(_) => {
            if let Some(p) = spool {
                let _ = std::fs::remove_file(p);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let kept = spool.map(|p| format!(" (message kept at {})", p.display())).unwrap_or_default();
            fail(format!("sendmail: {e}{kept}"))
        }
    }
}

fn run_tidy(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let mut dry_run = false;
    for a in &args[1.min(args.len())..] {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            other => {
                eprintln!("mail tidy: unknown option: {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    if let Err(e) = mail::maildir::mailbox_valid(&mailbox) {
        return fail(format!("tidy: {e}"));
    }
    if env.ask_label.is_empty() {
        return fail("tidy: ask label does not resolve — refusing to guess one".to_string());
    }
    let bd = bd_cli(&env);
    let db_configured = !env.db.is_empty();
    match tidy::tidy(&bd, db_configured, &env.mail_root, &mailbox, &env.ask_label, env.tidy_fresh_s, env.id_prefix_for_regex(), dry_run) {
        Ok(r) => {
            println!("tidy: archived {}, kept {}", r.archived, r.kept);
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("tidy: {e}")),
    }
}

fn run_ensure(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_default();
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    if let Err(e) = mail::maildir::mailbox_valid(&mailbox) {
        eprintln!("mail ensure: {e}");
        return ExitCode::from(2);
    }
    match cmds::ensure(&env, &mailbox) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(format!("ensure: {e}")),
    }
}

fn run_sweep_dismissed(args: &[String]) -> ExitCode {
    let mailbox = args.first().cloned().unwrap_or_else(|| "operator".to_string());
    let env = match Env::load() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    if let Err(e) = mail::maildir::mailbox_valid(&mailbox) {
        return fail(format!("sweep-dismissed: {e}"));
    }
    let bd = bd_cli(&env);
    let db_configured = !env.db.is_empty();
    match cmds::sweep_dismissed(&bd, &LcCli, db_configured, &env.mail_root, &env.index_file, &mailbox, &env.operator_actor) {
        Ok(cmds::SweepOutcome::NothingToDismiss) => {
            println!("sweep-dismissed: nothing to dismiss");
            ExitCode::SUCCESS
        }
        Ok(cmds::SweepOutcome::Report(r)) => {
            println!("sweep-dismissed: dismissed {}, kept {}", r.dismissed, r.kept);
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("sweep-dismissed: {e}")),
    }
}
