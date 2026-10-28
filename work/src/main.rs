//! `work` — the dwarf-facing semantic layer client (design §3.5). Bound to one called
//! bead (`SPIRA_WORK_BEAD_ID`), it never takes the bead it acts on as an argument and it
//! never falls back to a direct database connection — see `work::build_request` for the
//! request-shaping and `SPIRA_LC_SOCKET` for the only channel this binary knows how to
//! reach the machine through. If that socket is unreachable this exits 2 ("cannot tell"),
//! the same code spira-lc's own CLI uses for the same reason: a caller that retries a
//! "cannot tell" is safe, one that retries a real refusal is not.
//!
//! There is no lifecycle switch (DESIGN.md §3, sp-v62vn): every verb goes to the socket.
//!
//! Deploys inert: nothing runs a dwarf through this yet (the dwarf.sh cutover bead,
//! sp-xethq, and the restricted unit environment this bead only provides — see
//! dwarf/src/restrict.rs, formerly spira/work-env.sh, retired sp-zpaq0).

use work::help::{refusal, R};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

const CANNOT_TELL: i32 = work::CANNOT_TELL;
const REFUSED: i32 = work::REFUSED;
const TIMEOUT: Duration = Duration::from_secs(5);
// batch-job: a reply under batch load can lag seconds; the read retries in slices up to this total.
const REPLY_WAIT: Duration = Duration::from_secs(30);
const READ_SLICE: Duration = Duration::from_secs(2);
// batch-job: a lane verb's broker-run tool is bounded at 300 s by the broker; wait just past it.
const TOOL_TIMEOUT: Duration = Duration::from_secs(310);

fn main() {
    spira_otel::run::begin("work", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().cloned() else {
        eprintln!("usage: work <verb> [args...] (verbs: {}, {})", work::VERBS.join(", "), work::LOCAL_VERBS.join(", "));
        { let code = CANNOT_TELL; spira_otel::run::finish(code); std::process::exit(code) };
    };
    let verb_args = args[1..].to_vec();
    spira_otel::run::operation(if work::VERBS.contains(&verb.as_str()) || work::LOCAL_VERBS.contains(&verb.as_str()) { &verb } else { "unknown" });
    if verb == "prime" {
        print!("{}", work::help::prime());
        { spira_otel::run::finish(0); std::process::exit(0) };
    }
    if verb == "help" {
        let code = match verb_args.as_slice() {
            [v] => match work::help::help(v) {
                Ok(t) => {
                    print!("{t}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    REFUSED
                }
            },
            _ => {
                eprintln!("{}", work::help::refusal(R::HelpUsage, "help", &[]));
                REFUSED
            }
        };
        { spira_otel::run::finish(code); std::process::exit(code) };
    }
    if verb == "statute-check" {
        { let code = statute_check(&verb_args); spira_otel::run::finish(code); std::process::exit(code) };
    }
    if verb == "review" {
        { let code = review(&verb_args); spira_otel::run::finish(code); std::process::exit(code) };
    }
    if verb == "law" {
        { let code = law(&verb_args); spira_otel::run::finish(code); std::process::exit(code) };
    }
    if verb == "checks" {
        { let code = checks(&verb_args); spira_otel::run::finish(code); std::process::exit(code) };
    }
    if work::LOCAL_VERBS.contains(&verb.as_str()) {
        { let code = crates(&verb_args); spira_otel::run::finish(code); std::process::exit(code) };
    }

    // A lane verb (sp-st0mm) runs unbound too — the archivist has no bead of its own — and
    // sends `-` in the bound slot; a bound verb refuses without a binding.
    let bound = match std::env::var("SPIRA_WORK_BEAD_ID") {
        Ok(b) if !b.is_empty() => b,
        _ if !work::needs_binding(&verb) => "-".to_string(),
        _ => {
            eprintln!("{}", refusal(R::NoBinding, "", &[]));
            { let code = CANNOT_TELL; spira_otel::run::finish(code); std::process::exit(code) };
        }
    };
    let actor = std::env::var("SPIRA_RUNE").or_else(|_| std::env::var("SPIRA_WORK_ACTOR")).unwrap_or_else(|_| "dwarf".to_string());

    if verb == "submit" && checks(&[]) != 0 {
        eprintln!("work submit: refused: the repository's done checks failed (output above) — fix them, commit, and run `work submit` again; `work checks` runs them alone");
        { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
    }

    let tip = if verb == "submit" { read_tip(&bound) } else { None };

    let read_stdin = || {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map(|_| s).map_err(|e| e.to_string())
    };
    let collected = if verb == "mender" {
        work::collect_mender_body(&verb_args, read_stdin)
    } else {
        work::collect_body(&verb_args, |p| std::fs::read_to_string(p).map_err(|e| e.to_string()), read_stdin)
    };
    let (verb_args, body) = match collected {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
        }
    };

    let req = match work::build_request_with_body(&bound, &verb, &verb_args, tip.as_deref(), &actor, body.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
        }
    };

    // A harness tool (a groomer sweep, a queue step) reads the whole graph, and a filing verb
    // runs bead.sh; the broker bounds both at 300 s, so this client waits that long and no longer.
    let runs_tool = work::LANE_VERBS.contains(&verb.as_str()) || matches!(verb.as_str(), "file-followup" | "split" | "submit" | "ack");
    let wait = if runs_tool { TOOL_TIMEOUT } else { REPLY_WAIT };
    let asked = std::time::Instant::now();
    let answer = send(&req, wait);
    spira_otel::run::phase("reply", asked.elapsed().as_millis() as u64);
    match answer {
        Ok((code, out)) => {
            spira_config::lc_call::print_answer(code, &out);
            { spira_otel::run::finish(code); std::process::exit(code) };
        }
        Err(e) => {
            eprintln!("{}", refusal(R::SocketUnreachable, "", &[("err", &e)]));
            { let code = CANNOT_TELL; spira_otel::run::finish(code); std::process::exit(code) };
        }
    }
}

/// `submit` never takes a tip argument (design §3.5): it is read here, the only place this
/// binary touches git — from the bead's own branch `spira/<id>`, which every worktree of the
/// repo shares, so the answer does not depend on the caller's cwd. `HEAD` read from the
/// wrong directory recorded local/main as the tip and the bead could never certify.
fn read_tip(bead: &str) -> Option<String> {
    let branch = format!("refs/heads/spira/{bead}");
    if let Ok(out) = Command::new("timeout").args(["5", "git", "rev-parse", "--verify", "-q", &branch]).output() {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    let out = spira_config::bounded::bounded("git").args(["rev-parse", "HEAD"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The exact wire protocol spira-lc's own CLI speaks to its socket (client.rs): one JSON
/// line in (the argv array), one header line out (`{exit_code, len}`) then `len` raw stdout bytes. No same-user
/// fallback exists here — see this file's module doc for why that absence is the point.
fn send(argv: &[String], wait: Duration) -> Result<(i32, String), String> {
    let socket_path = spira_config::process::cfg("SPIRA_LC_SOCKET")?;
    let stream = UnixStream::connect(&socket_path).map_err(|e| format!("connecting to {socket_path}: {e}"))?;
    stream.set_read_timeout(Some(READ_SLICE.min(wait))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;

    let request = serde_json::to_string(argv).map_err(|e| e.to_string())?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    writeln!(writer, "{request}").map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    // The request is already sent and may mutate, so only the read is retried: a slow socket
    // under load answers late (EAGAIN on the slice), it must not be resent.
    let deadline = std::time::Instant::now() + wait;
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => return Err("spira-lc closed the connection with no reply".to_string()),
            Ok(_) => break,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!("no reply from spira-lc within {}s: {e}", wait.as_secs()));
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let resp: serde_json::Value = serde_json::from_str(line.trim()).map_err(|e| format!("malformed reply: {e}"))?;
    let code = resp.get("exit_code").and_then(|v| v.as_i64()).ok_or("reply had no exit_code")? as i32;
    let len = resp.get("len").and_then(|v| v.as_u64()).ok_or("reply had no len")? as usize;
    let mut body = vec![0u8; len];
    let mut at = 0;
    while at < len {
        match reader.read(&mut body[at..]) {
            Ok(0) => return Err("spira-lc closed the connection mid-reply".to_string()),
            Ok(n) => at += n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!("no reply from spira-lc within {}s: {e}", wait.as_secs()));
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = String::from_utf8(body).map_err(|e| format!("reply was not UTF-8: {e}"))?;
    Ok((code, out))
}

/// `work statute-check -` (plan on stdin) or `work statute-check "<plan>"`: runs the statute
/// check on a builder's plan. A plan with no governed decision records nothing; otherwise the
/// full text of every statute returned is printed and the plan and slugs are recorded on the
/// bound bead as a note.
fn statute_check(args: &[String]) -> i32 {
    let plan = match args {
        [a] if a == "-" => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("{}", refusal(R::StatuteStdin, "statute-check", &[("err", &e.to_string())]));
                return REFUSED;
            }
            s
        }
        [a] => a.clone(),
        _ => {
            eprintln!("{}", refusal(R::StatuteUsage, "statute-check", &[]));
            return REFUSED;
        }
    };
    let bound = match std::env::var("SPIRA_WORK_BEAD_ID") {
        Ok(b) if !b.is_empty() => b,
        _ => {
            eprintln!("{}", refusal(R::NoBinding, "", &[]));
            return CANNOT_TELL;
        }
    };
    let cfg = |k: &str| spira_config::process::cfg(k);
    let (guards, dir) = match (cfg("SPIRA_STATUTE_GUARDS"), cfg("SPIRA_STATUTE_DIR")) {
        (Ok(g), Ok(d)) if !g.is_empty() && !d.is_empty() => (g, d),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{}", refusal(R::StatuteConfig, "statute-check", &[("err", &e)]));
            return REFUSED;
        }
        _ => {
            eprintln!("{}", refusal(R::StatuteConfigUnset, "statute-check", &[]));
            return REFUSED;
        }
    };
    let registry = match std::fs::read_to_string(&guards) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", refusal(R::StatuteRegistry, "statute-check", &[("path", &guards), ("err", &e.to_string())]));
            return REFUSED;
        }
    };
    let text_of = |slug: &str| std::fs::read_to_string(std::path::Path::new(&dir).join(format!("{slug}.txt"))).ok();
    let (out, note) = match work::statute_check_report(&registry, &plan, text_of) {
        Ok(None) => {
            println!("work statute-check: the plan makes no governed decision; no check owed");
            return 0;
        }
        Ok(Some(r)) => r,
        Err(e) => {
            eprintln!("{}", refusal(R::StatuteReport, "statute-check", &[("err", &e)]));
            return REFUSED;
        }
    };
    let actor = std::env::var("SPIRA_RUNE").or_else(|_| std::env::var("SPIRA_WORK_ACTOR")).unwrap_or_else(|_| "dwarf".to_string());
    let req = match work::build_request_with_body(&bound, "note", &["-".to_string()], None, &actor, Some(&note)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return REFUSED;
        }
    };
    match send(&req, REPLY_WAIT) {
        Ok((0, _)) => {}
        Ok((code, o)) => {
            eprintln!("{}", refusal(R::StatuteNoteNotRecorded, "statute-check", &[("code", &code.to_string()), ("err", o.trim())]));
            return code;
        }
        Err(e) => {
            eprintln!("{}", refusal(R::SocketUnreachable, "", &[("err", &e)]));
            return CANNOT_TELL;
        }
    }
    print!("{out}");
    println!("Read every statute above in full before writing code. If one forbids the plan, change the plan or `work ask` with a default. The check is recorded on the bead.");
    0
}

/// `work crates [<base>]`: the `-p` words for the worktree's diff against `<base>` (default
/// `local/main`) — touched crates plus direct dependents. Append them to `cargo build`,
/// `check` or `test`; empty output means nothing buildable changed.
fn crates(args: &[String]) -> i32 {
    let base = args.first().map(String::as_str).unwrap_or("local/main");
    let run = |argv: &[&str]| spira_config::bounded::bounded(argv[0]).args(&argv[1..]).output();
    let diff = match run(&["git", "diff", "--raw", "-z", "--no-renames", &format!("{base}...HEAD")]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => {
            eprintln!("{}", refusal(R::CratesGit, "crates", &[("base", base)]));
            return REFUSED;
        }
    };
    let meta = match run(&["cargo", "metadata", "--format-version", "1", "--no-deps"]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            eprintln!("{}", refusal(R::CratesCargo, "crates", &[]));
            return REFUSED;
        }
    };
    match work::crates_selection(&diff, &meta) {
        Ok(c) => {
            println!("{}", work::crates_words(&c));
            0
        }
        Err(e) => {
            eprintln!("{}", refusal(R::CratesSelect, "crates", &[("err", &e)]));
            REFUSED
        }
    }
}

/// `work review`: the bound bead's text and the branch diff, read by one reviewer session.
fn review(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", work::review::HELP);
        return 0;
    }
    let opts = match work::review::parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{}", refusal(R::ReviewArgs, "review", &[("err", &e)]));
            return REFUSED;
        }
    };
    let bound = match std::env::var("SPIRA_WORK_BEAD_ID") {
        Ok(b) if !b.is_empty() => b,
        _ => {
            eprintln!("{}", refusal(R::NoBinding, "", &[]));
            return CANNOT_TELL;
        }
    };
    let actor = std::env::var("SPIRA_RUNE").or_else(|_| std::env::var("SPIRA_WORK_ACTOR")).unwrap_or_else(|_| "dwarf".to_string());
    let req = match work::build_request_with_body(&bound, "show", &[], None, &actor, None) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return REFUSED;
        }
    };
    let item = match send(&req, REPLY_WAIT) {
        Ok((0, o)) => o,
        Ok((code, o)) => {
            eprintln!("{}", refusal(R::ReviewShow, "review", &[("code", &code.to_string()), ("err", o.trim())]));
            return code;
        }
        Err(e) => {
            eprintln!("{}", refusal(R::SocketUnreachable, "", &[("err", &e)]));
            return CANNOT_TELL;
        }
    };
    let diff = match spira_config::bounded::bounded("git").args(["diff", &format!("{}...HEAD", opts.base)]).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            eprintln!("{}", refusal(R::ReviewGit, "review", &[("base", &opts.base)]));
            return REFUSED;
        }
    };
    let cfg = |k: &str| spira_config::process::cfg(k);
    let persona = std::env::var("SPIRA_RUNE").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| "builder".to_string());
    let (agent, model) = match (cfg("SPIRA_AGENT"), spira_config::process::spec().and_then(|t| work::review::persona_model(&persona, &t))) {
        (Ok(a), Ok(m)) if !a.is_empty() => (a, m),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{}", refusal(R::ReviewConfig, "review", &[("err", &e)]));
            return REFUSED;
        }
        _ => {
            eprintln!("{}", refusal(R::ReviewAgentUnset, "review", &[]));
            return REFUSED;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let secs = opts.timeout.as_secs();
    match work::review::run(&agent, &work::review::agent_argv(&model), &work::review::input(&item, &diff), &cwd, opts.timeout) {
        Ok((outcome, transcript)) => {
            let verdict = match &outcome {
                work::review::Outcome::Findings(f) => f.as_str(),
                _ => "",
            };
            match cfg("SPIRA_RUN").and_then(|r| work::review::keep(std::path::Path::new(&r), &bound, &transcript, verdict)) {
                Ok(p) => eprintln!("work review: transcript kept at {}", p.display()),
                Err(e) => eprintln!("work review: {e}"),
            }
            match outcome {
                work::review::Outcome::Findings(f) => {
                    println!("{}", f.trim_end());
                    0
                }
                work::review::Outcome::TimedOut => {
                    eprintln!("{}", refusal(R::ReviewTimedOut, "review", &[("secs", &secs.to_string())]));
                    work::review::TIMED_OUT
                }
                work::review::Outcome::Failed(e) => {
                    eprintln!("{}", refusal(R::ReviewFailed, "review", &[("err", &e)]));
                    REFUSED
                }
            }
        }
        Err(e) => {
            eprintln!("{}", refusal(R::ReviewFailed, "review", &[("err", &e)]));
            REFUSED
        }
    }
}

/// `work law [<part> | <part>.<n> | <slug>]`: reads the tracked `law/` tree of the worktree
/// it runs in; no store, no socket.
fn law(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("{}", refusal(R::LawUsage, "law", &[]));
        return REFUSED;
    }
    let root = spira_config::bounded::bounded("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| ".".to_string());
    match work::law_view(std::path::Path::new(&root), args.first().map(String::as_str)) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err((msg, list)) => {
            eprintln!("{}", refusal(R::LawNotFound, "law", &[("err", &msg)]));
            print!("{list}");
            REFUSED
        }
    }
}

/// `work checks [--list]`: the repository's `[[check]]` rows from `.spira/checks.toml`, run in
/// order from the worktree root. Every row runs; the exit is non-zero if any failed.
fn checks(args: &[String]) -> i32 {
    let list_only = match args {
        [] => false,
        [a] if a == "--list" => true,
        _ => {
            eprintln!("{}", refusal(R::ChecksUsage, "checks", &[]));
            return REFUSED;
        }
    };
    let root = match spira_config::bounded::bounded("git").args(["rev-parse", "--show-toplevel"]).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            eprintln!("{}", refusal(R::ChecksGit, "checks", &[]));
            return REFUSED;
        }
    };
    let root = std::path::Path::new(&root);
    let rows = match habits::config::load_checks(root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", refusal(R::ChecksConfig, "checks", &[("err", &e.to_string())]));
            return REFUSED;
        }
    };
    if rows.is_empty() {
        println!("work checks: this repository declares no [[check]] rows");
        return 0;
    }
    let mut failed = Vec::new();
    for row in &rows {
        if list_only {
            println!("{}: {} (budget {} ms)", row.name, row.run, row.budget_ms);
            continue;
        }
        println!("== {}: {}", row.name, row.run);
        let argv = work::done_argv(&row.run);
        let Some((prog, rest)) = argv.split_first() else {
            println!("-- {}: FAIL (empty run)", row.name);
            failed.push(row.name.clone());
            continue;
        };
        let secs = row.budget_ms.div_ceil(1000).max(1).to_string();
        let out = spira_config::bounded::bounded_for(prog, &secs).args(rest).current_dir(root).output();
        match out {
            Ok(o) => {
                print!("{}", String::from_utf8_lossy(&o.stdout));
                eprint!("{}", String::from_utf8_lossy(&o.stderr));
                if o.status.success() {
                    println!("-- {}: ok", row.name);
                } else {
                    println!("-- {}: FAIL ({})", row.name, o.status);
                    failed.push(row.name.clone());
                }
            }
            Err(e) => {
                println!("-- {}: FAIL (could not run: {e})", row.name);
                failed.push(row.name.clone());
            }
        }
    }
    if failed.is_empty() {
        0
    } else {
        eprintln!("work checks: failed: {}", failed.join(", "));
        REFUSED
    }
}
