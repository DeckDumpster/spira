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

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
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
    spira_otel::run::begin_with("work", || spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().cloned() else {
        eprintln!("usage: work <verb> [args...] (verbs: {}, {})", work::VERBS.join(", "), work::LOCAL_VERBS.join(", "));
        { let code = CANNOT_TELL; spira_otel::run::finish(code); std::process::exit(code) };
    };
    let verb_args = args[1..].to_vec();
    spira_otel::run::operation(if work::VERBS.contains(&verb.as_str()) || work::LOCAL_VERBS.contains(&verb.as_str()) { &verb } else { "unknown" });
    if verb == "prime" {
        print!("{}", work::help::prime());
        for w in unresolved_programs() {
            println!("\nwork prime: warning: {w}");
        }
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
        { let code = checks(&verb_args, "checks"); spira_otel::run::finish(code); std::process::exit(code) };
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

    let mut red_tests = Vec::new();
    let base_red = if verb == "submit" { work::unit_red_on_base(&verb_args).is_some() } else { false };
    if verb == "submit" && !checks_passed_at_tip() && {
        let (code, waived) = run_checks(&[], base_red, "submit");
        red_tests = waived;
        code != 0
    } {
        eprintln!("work submit: refused: the repository's done checks failed (output above) — fix them, commit, and run `work submit` again; `work checks` runs them alone");
        { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
    }
    if verb == "submit" {
        if let Err(e) = image_gate(&bound) {
            eprintln!("work submit: refused: {e} — fix the image recipe, commit, and run `work submit` again");
            { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
        }
    }

    if verb == "submit" {
        if let Err(e) = scratch_gate() {
            eprintln!("{}", refusal(R::SubmitScratch, "submit", &[("err", &e)]));
            { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
        }
    }

    if verb == "submit" {
        if let Err(e) = contract_gate() {
            if e.starts_with("not met ") {
                eprintln!("work submit: refused: the branch changes the repository's landing contract and leaves it unmet:\n{e}\nfix the contract files, commit, and run `work submit` again; `repo-contract check <repo> --tree .` shows every clause");
            } else {
                eprintln!("work submit: refused: the branch changes the repository's landing contract and it cannot be judged: {e}");
            }
            { let code = REFUSED; spira_otel::run::finish(code); std::process::exit(code) };
        }
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

    let build_dir = if verb == "submit" { owned_build_dir() } else { None };
    let req = match work::build_request_in(&bound, &verb, &verb_args, tip.as_deref(), &actor, body.as_deref(), build_dir.as_deref()) {
        Ok(mut r) => {
            if base_red {
                r.extend([work::UNIT_RED_TESTS.to_string(), red_tests.join(",")]);
            }
            r
        }
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
    let answer = answered(&send, &req, wait);
    spira_otel::run::phase("reply", asked.elapsed().as_millis() as u64);
    match answer {
        Ok((code, out)) => {
            spira_store::lc_call::print_answer(code, &out);
            { spira_otel::run::finish(code); std::process::exit(code) };
        }
        Err(e) => {
            let code = report_unsent(e);
            spira_otel::run::finish(code);
            std::process::exit(code)
        }
    }
}

type Transport<'a> = &'a dyn Fn(&[String], Duration) -> Result<(i32, String), Unsent>;
type OwnedTransport = Box<dyn Fn(&[String], Duration) -> Result<(i32, String), Unsent>>;

/// Sends `req`; a queue receipt is followed to the recorded request's own answer, each poll a plain
/// read of its record so no request is held open meanwhile. Any other reply is returned as-is.
fn answered(send: Transport, req: &[String], wait: Duration) -> Result<(i32, String), Unsent> {
    use spira_store::lc_call::{queued_id, record_answer, record_timeout, RECORD_POLL, RECORD_WAIT};
    let began = std::time::Instant::now();
    let (code, out) = send(req, wait)?;
    let id = match queued_id(&out) {
        Some(id) if code == 0 => id,
        _ => return Ok((code, out)),
    };
    let show = vec!["work-request".to_string(), "show".to_string(), id.to_string()];
    loop {
        let (code, out) = send(&show, REPLY_WAIT)?;
        if code != 0 {
            return Ok((code, out));
        }
        if let Some(answer) = record_answer(&out).map_err(|e| Unsent::Other(format!("request {id}: {e}")))? {
            return Ok(answer);
        }
        if began.elapsed() >= RECORD_WAIT {
            return Err(Unsent::Other(record_timeout(id)));
        }
        std::thread::sleep(RECORD_POLL); // waits-on: the scheduler finishing the recorded request; cap RECORD_WAIT
    }
}

/// The build directory this session owns: `CARGO_TARGET_DIR` when it is one under the configured base.
fn owned_build_dir() -> Option<String> {
    spira_config::build_dir::owned(std::env::var_os("CARGO_TARGET_DIR"), &spira_config::build_dir::configured_bases()).map(|p| p.to_string_lossy().into_owned())
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
fn send(argv: &[String], wait: Duration) -> Result<(i32, String), Unsent> {
    let socket_path = spira_config::process::cfg("SPIRA_LC_SOCKET")?;
    let began = std::time::Instant::now();
    let stream = connect_waiting(&socket_path, REPLY_WAIT)?;
    send_on(stream, argv, wait.saturating_sub(began.elapsed()).max(Duration::from_secs(1))).map_err(Unsent::Other)
}

/// Why a request got no answer: no store answered the socket (nothing was sent), or anything else.
#[derive(Debug)]
enum Unsent {
    NotRunning { waited_secs: u64 },
    Other(String),
}

impl std::fmt::Display for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsent::NotRunning { waited_secs } => write!(f, "store not running: nothing was sent; waited {waited_secs}s"),
            Unsent::Other(e) => f.write_str(e),
        }
    }
}

impl From<String> for Unsent {
    fn from(e: String) -> Self {
        Unsent::Other(e)
    }
}

/// Connects, trying again while no store answers the socket, for at most `wait`.
fn connect_waiting(socket_path: &str, wait: Duration) -> Result<UnixStream, Unsent> {
    use spira_store::lc_call::{connect_failure, retrying, Fail};
    let path = std::path::Path::new(socket_path);
    let began = std::time::Instant::now();
    retrying(&[], path, wait, wait, |_| {
        let stream = UnixStream::connect(path).map_err(|e| connect_failure(path, &e))?;
        stream.set_read_timeout(Some(TIMEOUT)).and_then(|_| stream.set_write_timeout(Some(TIMEOUT))).map_err(|e| Fail::Final(e.to_string()))?;
        Ok(stream)
    }).map_err(|f| match f {
        Fail::NotRunning(_) => Unsent::NotRunning { waited_secs: began.elapsed().as_secs() },
        f => Unsent::Other(f.into_text()),
    })
}

/// Reports an unanswered request and returns the exit code.
fn report_unsent(e: Unsent) -> i32 {
    match e {
        Unsent::NotRunning { waited_secs } => eprintln!("{}", refusal(R::StoreNotRunning, "", &[("secs", &waited_secs.to_string())])),
        Unsent::Other(e) => eprintln!("{}", refusal(R::SocketUnreachable, "", &[("err", &e)])),
    }
    CANNOT_TELL
}

fn send_on(stream: UnixStream, argv: &[String], wait: Duration) -> Result<(i32, String), String> {
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
        Err(e) => return report_unsent(e),
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
        Err(e) => return report_unsent(e),
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
    let agent = work::trusted::resolve(&agent, &work::trusted::process_dirs(true)).map(|p| p.display().to_string()).unwrap_or(agent);
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

/// `work law [<part> | <part>.<n> | §<part>.<n> | <slug>]`: reads the statute book in the configured
/// wiki checkout; no store, no socket.
fn law(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("{}", refusal(R::LawUsage, "law", &[]));
        return REFUSED;
    }
    let book = match rule::code::book_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", refusal(R::LawNotFound, "law", &[("err", &e)]));
            return REFUSED;
        }
    };
    match work::law_view(&book, args.first().map(String::as_str)) {
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

struct ImagePorts {
    root: std::path::PathBuf,
    tip: String,
    bound: String,
    actor: String,
    started: std::time::Instant,
    transport: OwnedTransport,
}

impl ImagePorts {
    fn git(&self, args: &[&str]) -> Result<String, String> {
        let o = spira_config::bounded::bounded("git").args(args).current_dir(&self.root).output().map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
        }
    }
    /// The session holds no forge credentials: the forge is reached through the broker's `forge` verb.
    fn forge(&self, args: &[&str]) -> Result<String, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let req = work::build_request_in(&self.bound, "forge", &args, None, &self.actor, None, None)?;
        let socket = spira_config::process::cfg("SPIRA_LC_SOCKET").unwrap_or_default();
        let route = format!("`work forge {}` via the broker at {socket}", args[0]);
        match answered(&*self.transport, &req, TOOL_TIMEOUT) {
            Ok((0, out)) => Ok(out),
            Ok((code, out)) => Err(format!("{route} exited {code}: {}", out.trim())),
            Err(e) => Err(format!("{route}: {e}")),
        }
    }
}

impl work::image::Ports for ImagePorts {
    fn has_recipe(&self) -> bool {
        self.git(&["cat-file", "-e", &format!("{}:{}", self.tip, work::image::RECIPE_WORKFLOW)]).is_ok()
    }
    fn landing_ref(&self) -> Option<String> {
        let common = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?;
        let checkout = std::path::Path::new(common.trim()).parent()?.to_path_buf();
        let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), &checkout);
        spira_config::repos::landref(&reg, &checkout.to_string_lossy())
    }
    fn changed_paths(&self, base: &str) -> Option<Vec<String>> {
        let out = self.git(&["diff", "--name-only", "--no-renames", &format!("{base}...{}", self.tip)]).ok()?;
        Some(out.lines().map(String::from).collect())
    }
    fn push(&self, branch: &str) -> Result<(), String> {
        self.forge(&["ami-push", branch, &self.tip]).map(drop)
    }
    fn delete(&self, branch: &str) {
        let _ = self.forge(&["ami-delete", branch]);
    }
    fn dispatch(&self, branch: &str) -> Result<(), String> {
        self.forge(&["ami-dispatch", branch]).map(drop).map_err(|e| format!("the forge refused the dispatch: {e}"))
    }
    fn result(&self, branch: &str) -> Result<work::image::Build, String> {
        let out = self.forge(&["ami-result", branch])?;
        work::image::read_result(&out)
    }
    fn log_tail(&self, run: &str, lines: usize) -> Vec<String> {
        let out = self.forge(&["ami-log", run, &lines.to_string()]).unwrap_or_default();
        out.lines().filter(|l| l.trim() != "?").map(String::from).collect()
    }
    fn now(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs)); // waits-on: the Round AMI run on the candidate branch; cap SPIRA_ROUND_AMI_WAIT_SECS
    }
}

/// Refuses a submit whose branch changes the round image recipe and does not build the image.
/// The decision is `image::gate`'s: a repository without the image workflow, or a diff against its
/// registered landing ref that leaves the recipe alone, never reaches the forge.
fn image_gate(bead: &str) -> Result<(), String> {
    let root = git_line(&["rev-parse", "--show-toplevel"]).ok_or("not inside a git worktree")?;
    let tip = git_line(&["rev-parse", "HEAD"]).ok_or("cannot read HEAD")?;
    let ports = ImagePorts {
        root: root.into(),
        tip: tip.clone(),
        bound: bead.to_string(),
        actor: std::env::var("SPIRA_RUNE").or_else(|_| std::env::var("SPIRA_WORK_ACTOR")).unwrap_or_else(|_| "dwarf".to_string()),
        started: std::time::Instant::now(),
        transport: Box::new(send),
    };
    let wait = spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_AMI_WAIT_SECS")?;
    match work::image::gate(&ports, bead, &tip, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), wait)? {
        work::image::Verdict::Skipped => {}
        work::image::Verdict::Built(run) => println!("work submit: the round image recipe builds (run {run})"),
    }
    Ok(())
}

/// Refuses a submit whose branch adds a session-scratch path against its landing ref, in any
/// repository; an unresolvable landing ref or unreadable diff refuses.
fn scratch_gate() -> Result<(), String> {
    let common = git_line(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok_or("cannot read the git dir")?;
    let checkout = std::path::Path::new(&common).parent().ok_or("cannot find the checkout")?;
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), checkout);
    let land = spira_config::repos::landref(&reg, &checkout.to_string_lossy()).ok_or("the landing ref cannot be resolved, so the branch cannot be checked for session scratch")?;
    let root = git_line(&["rev-parse", "--show-toplevel"]).ok_or("not inside a git worktree")?;
    work::scratch::gate(std::path::Path::new(&root), &land)
}

/// Refuses a submit to a non-home repository whose diff against its landing ref edits a contract
/// file and leaves the environment or deploy clause not met on this worktree.
fn contract_gate() -> Result<(), String> {
    let root = git_line(&["rev-parse", "--show-toplevel"]).ok_or("not inside a git worktree")?;
    let common = git_line(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok_or("cannot read the git dir")?;
    let Some(checkout) = std::path::Path::new(&common).parent() else { return Ok(()) };
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), checkout);
    let checkout = checkout.to_string_lossy();
    let (Some(name), Some(land)) = (reg.name_at(&checkout), spira_config::repos::landref(&reg, &checkout)) else { return Ok(()) };
    if name == reg.home_repo() {
        return Ok(());
    }
    let changed: Vec<String> = git_line(&["diff", "--name-only", "--no-renames", &format!("{land}...HEAD")]).map(|o| o.lines().map(String::from).collect()).unwrap_or_default();
    if !work::contract::touches_contract(&changed) {
        return Ok(());
    }
    let dirs = work::trusted::process_dirs(false);
    let exe = work::trusted::resolve("repo-contract", &dirs).ok_or("repo-contract is not on the release's bin/; cannot judge the contract")?;
    let out = spira_config::bounded::bounded_for(exe, "3600")
        .args(["check", &name, "--tree", &root, "--clauses", work::contract::FENCE_CLAUSES])
        .output()
        .map_err(|e| format!("cannot run repo-contract: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match work::contract::refusal(out.status.code(), &stdout) {
        Some(e) => Err(e),
        None => {
            if out.status.code() != Some(0) {
                eprintln!("work submit: warning: repo-contract could not tell: {}{}", stdout.trim(), String::from_utf8_lossy(&out.stderr).trim());
            }
            Ok(())
        }
    }
}

/// `work checks [--list]`: the repository's `[[check]]` rows from `.spira/checks.toml`, run in
/// order from the worktree root. Every row runs; the exit is non-zero if any failed.
fn checks(args: &[String], via: &str) -> i32 {
    run_checks(args, false, via).0
}

/// [`checks`], with the failing test names of every red row deferred to the `gate` when
/// `base_red` waives it (a submit naming the bead that records the base red).
fn run_checks(args: &[String], base_red: bool, via: &str) -> (i32, Vec<String>) {
    let list_only = match args {
        [] => false,
        [a] if a == "--list" => true,
        _ => {
            eprintln!("{}", refusal(R::ChecksUsage, "checks", &[]));
            return (REFUSED, Vec::new());
        }
    };
    let root = match spira_config::bounded::bounded("git").args(["rev-parse", "--show-toplevel"]).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            eprintln!("{}", refusal(R::ChecksGit, "checks", &[]));
            return (REFUSED, Vec::new());
        }
    };
    let root = std::path::Path::new(&root);
    let rows = match work::declared_rows(root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", refusal(R::ChecksConfig, "checks", &[("err", &e.to_string())]));
            return (REFUSED, Vec::new());
        }
    };
    if rows.is_empty() {
        println!("work checks: this repository declares no [[check]] rows");
        return (0, Vec::new());
    }
    let shell = if list_only { Ok(None) } else { flake_shell(root) };
    let mut failed = Vec::new();
    let mut deferred = false;
    let mut waived_tests = Vec::new();
    let branch = git_line(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let mut scope: Option<work::Affected> = None;
    let log = RowLog::open(root, via);
    for row in &rows {
        if list_only {
            println!("{}: {} (budget {} ms)", row.name, row.run, row.budget_ms);
            continue;
        }
        println!("== {}: {}", row.name, row.run);
        let none = work::Affected::Crates(Vec::new());
        let scope = if row.run.contains(work::AFFECTED) || work::is_unit_build(&row.run) { &*scope.get_or_insert_with(|| affected_scope(work::CHECKS_BASE)) } else { &none };
        let record_file = log.record_file(&row.name);
        let _ = std::fs::remove_file(&record_file);
        let started = std::time::Instant::now();
        let Some(argv) = work::check_argv(&row.run, scope, &branch, &record_file) else {
            println!("-- {}: ok (the diff affects no crate; nothing to check)", row.name);
            continue;
        };
        let shell = match &shell {
            Ok(s) => s.as_ref(),
            Err(e) => {
                println!("-- {}: FAIL (cannot enter the repository's dev shell: {e})", row.name);
                failed.push(row.name.clone());
                log.row(row, None, started.elapsed().as_secs(), &record_file);
                continue;
            }
        };
        if shell.is_some() || argv != work::done_argv(&row.run) || row.run.contains("{branch}") {
            let within = shell.map(|s| format!("   in: flake dev shell ({})", s.key)).unwrap_or_default();
            println!("runs: {}{within}", argv.join(" "));
        }
        let Some((prog, rest)) = argv.split_first() else {
            println!("-- {}: FAIL (empty run)", row.name);
            failed.push(row.name.clone());
            log.row(row, None, started.elapsed().as_secs(), &record_file);
            continue;
        };
        let secs = row.budget_ms.div_ceil(1000).max(1).to_string();
        let dirs = work::row_dirs(shell, work::trusted::process_dirs(false));
        let exe = match &row.build {
            Some(pkg) => match build_from_worktree(root, pkg, &dirs, shell, row.budget_ms) {
                Ok(path) => {
                    println!("built from worktree: {}", path.display());
                    path
                }
                Err(e) => {
                    println!("-- {}: FAIL (cannot build {pkg} from this worktree: {e})", row.name);
                    failed.push(row.name.clone());
                    log.row(row, None, started.elapsed().as_secs(), &record_file);
                    continue;
                }
            },
            None => {
                let found = work::trusted::resolve(prog, &dirs);
                if let Some(p) = &found {
                    println!("resolved: {}", p.display());
                } else if let Some(s) = shell {
                    println!("-- {}: FAIL ({prog} is not supplied by the flake dev shell of {} ({}))", row.name, root.join("flake.nix").display(), s.key);
                    failed.push(row.name.clone());
                    log.row(row, None, started.elapsed().as_secs(), &record_file);
                    continue;
                }
                found.unwrap_or_else(|| std::path::PathBuf::from(prog))
            }
        };
        let ran_from = std::time::Instant::now();
        let path_env = work::trusted::join_path(&dirs);
        let shell_vars: Vec<(String, String)> = shell.map(|s| s.row_vars().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        let out = if prog == "cargo" {
            let timeout = work::trusted::resolve("timeout", &work::trusted::process_dirs(false)).unwrap_or_else(|| std::path::PathBuf::from("timeout"));
            let mut bounded = vec![timeout.to_string_lossy().into_owned(), secs, exe.to_string_lossy().into_owned()];
            bounded.extend(rest.iter().cloned());
            // batch-job: bounded by the row's budget through `timeout`
            Command::new(&bounded[0]).args(&bounded[1..]).envs(shell_vars.iter().map(|(k, v)| (k, v))).env("PATH", &path_env).current_dir(root).output()
        } else {
            work::run_row(&exe, rest, root, std::ffi::OsStr::new(&path_env), &shell_vars, row.budget_ms)
        };
        match out {
            Ok(o) => {
                print!("{}", String::from_utf8_lossy(&o.stdout));
                let err = String::from_utf8_lossy(&o.stderr);
                eprint!("{err}");
                log.row(row, o.status.code(), ran_from.elapsed().as_secs(), &record_file);
                let stdout = String::from_utf8_lossy(&o.stdout);
                match work::row_outcome(o.status.code().unwrap_or(-1), row.deferred_to.as_deref()) {
                    work::Outcome::Ok => println!("-- {}: ok", row.name),
                    work::Outcome::Deferred => {
                        let stage = row.deferred_to.as_deref().unwrap_or_default();
                        let crates = if stage == "gate" { format!(": {}", selected_crates(work::CHECKS_BASE).join(" ")) } else { String::new() };
                        println!("-- {}: deferred to the {stage} (cut at its {} ms budget){crates}", row.name, row.budget_ms);
                        deferred = true;
                    }
                    work::Outcome::Fail => {
                        let tests = work::failing_tests(&stdout);
                        if row.deferred_to.as_deref() == Some("gate") && base_red && !tests.is_empty() {
                            println!("-- {}: FAIL ({}) — waived: the red is recorded on the base (--unit-red-on-base)", row.name, o.status);
                            waived_tests.extend(tests);
                            deferred = true;
                        } else {
                            let exit = if row.deferred_to.as_deref() == Some("gate") {
                                " — if the same tests are red on the base, file a bead recording it and run `work submit --unit-red-on-base <bead-id>`"
                            } else {
                                ""
                            };
                            println!("-- {}: FAIL ({}){exit}", row.name, o.status);
                            failed.push(row.name.clone());
                        }
                    }
                }
            }
            Err(e) => {
                println!("-- {}: FAIL (could not run: {e})", row.name);
                failed.push(row.name.clone());
                log.row(row, None, started.elapsed().as_secs(), &record_file);
            }
        }
    }
    if failed.is_empty() {
        if !list_only && !deferred {
            record_checks_pass();
        }
        (0, waived_tests)
    } else {
        eprintln!("work checks: failed: {}", failed.join(", "));
        (REFUSED, Vec::new())
    }
}

/// Where a `work checks` run leaves what each row cost and decided: a line in `<run>/checks.log`
/// and a few metrics. Neither can change the run's output or exit; a missing run directory or
/// metrics endpoint leaves that half unwritten.
struct RowLog {
    via: String,
    git_dir: std::path::PathBuf,
    run: Option<std::path::PathBuf>,
    repo: String,
    branch: String,
    bead: String,
    sink: spira_otel::metrics::MetricsSink,
}

impl RowLog {
    fn open(root: &std::path::Path, via: &str) -> RowLog {
        let git_dir = std::path::Path::new(root).join(git_line(&["rev-parse", "--git-dir"]).unwrap_or_else(|| ".git".into()));
        let repo = git_line(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .and_then(|d| std::path::Path::new(&d).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "-".into());
        RowLog {
            via: via.into(),
            git_dir,
            run: spira_config::resolve::run_dir_for_process().ok(),
            repo,
            branch: git_line(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "-".into()),
            bead: std::env::var("SPIRA_WORK_BEAD_ID").unwrap_or_default(),
            sink: spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "work"),
        }
    }

    fn record_file(&self, row: &str) -> std::path::PathBuf {
        self.git_dir.join(format!("{}.{row}", work::CHECKS_RECORD_FILE))
    }

    fn row(&self, row: &habits::config::Check, rc: Option<i32>, ran_s: u64, record_file: &std::path::Path) {
        let record: Option<gate::unitrecord::UnitRecord> = std::fs::read_to_string(record_file).ok().and_then(|t| serde_json::from_str(&t).ok());
        let staged = row.deferred_to.is_some();
        let load1 = work::load1_now();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let ts = tsd::iso_utc(now);
        let line = work::checks_log_line(&work::CheckLine {
            ts: &ts,
            repo: &self.repo,
            branch: &self.branch,
            bead: &self.bead,
            row: &row.name,
            via: &self.via,
            rc,
            staged,
            ran_s,
            budget_s: row.budget_ms.div_ceil(1000),
            load1,
            record: record.as_ref(),
            deferred_to: row.deferred_to.as_deref(),
        });
        if let Some(run) = &self.run {
            use std::io::Write;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(run.join(work::CHECKS_LOG)).and_then(|mut f| writeln!(f, "{line}"));
        }
        self.push(row, rc, staged, ran_s, record.as_ref());
    }

    fn push(&self, row: &habits::config::Check, rc: Option<i32>, staged: bool, ran_s: u64, record: Option<&gate::unitrecord::UnitRecord>) {
        use spira_otel::metrics::Point;
        let verdict = work::check_outcome(rc, staged);
        let labels = [("repo", self.repo.as_str()), ("check", row.name.as_str())];
        let mut points = vec![Point::Seconds { name: "check_seconds", value: ran_s as f64 }];
        if let Some(b) = record.and_then(|r| r.build_s) {
            points.push(Point::Seconds { name: "check_build_seconds", value: b });
        }
        if let Some(t) = record.and_then(|r| r.test_s) {
            points.push(Point::Seconds { name: "check_test_seconds", value: t });
        }
        self.sink.push(&points, &labels);
        self.sink.push(&[Point::Count { name: "check_verdict", value: 1 }], &[labels[0], labels[1], ("verdict", verdict)]);
        if row.name == "unit-tests" && verdict == "red" && self.via == "submit" {
            self.sink.push(&[Point::Count { name: "unit_red_at_submit", value: 1 }], &[labels[0], ("bead", self.bead.as_str())]);
        }
    }
}

/// What `{affected}` expands to for this worktree's diff against `base`. A diff or a workspace
/// that cannot be read checks the whole workspace: the safe direction.
fn affected_scope(base: &str) -> work::Affected {
    let run = |argv: &[&str]| spira_config::bounded::bounded(argv[0]).args(&argv[1..]).output().ok().filter(|o| o.status.success()).map(|o| o.stdout);
    let diff = run(&["git", "diff", "--raw", "-z", "--no-renames", &format!("{base}...HEAD")]);
    let meta = run(&["cargo", "metadata", "--format-version", "1", "--no-deps"]);
    let (Some(diff), Some(meta)) = (diff, meta) else {
        println!("work checks: cannot read the diff against {base} or the cargo workspace; checking the whole workspace");
        return work::Affected::Workspace(Vec::new());
    };
    work::affected_scope(&diff, &String::from_utf8_lossy(&meta)).unwrap_or_else(|e| {
        println!("work checks: cannot select the affected crates ({e}); checking the whole workspace");
        work::Affected::Workspace(Vec::new())
    })
}

/// The crates `work crates` selects for this worktree's diff against `base`; empty when the diff
/// or the workspace cannot be read.
fn selected_crates(base: &str) -> Vec<String> {
    let run = |argv: &[&str]| spira_config::bounded::bounded(argv[0]).args(&argv[1..]).output().ok().filter(|o| o.status.success()).map(|o| o.stdout);
    let diff = run(&["git", "diff", "--raw", "-z", "--no-renames", &format!("{base}...HEAD")]);
    let meta = run(&["cargo", "metadata", "--format-version", "1", "--no-deps"]);
    let (Some(diff), Some(meta)) = (diff, meta) else { return Vec::new() };
    work::crates_selection(&diff, &String::from_utf8_lossy(&meta)).unwrap_or_default()
}

fn git_line(args: &[&str]) -> Option<String> {
    let o = spira_config::bounded::bounded("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The tip, the base and the checks file a pass is recorded against, the per-worktree file it
/// is recorded in, and whether the tree is the tip (no tracked change).
fn checks_pass_state() -> Option<(String, std::path::PathBuf, bool)> {
    let root = git_line(&["rev-parse", "--show-toplevel"])?;
    let toml = std::fs::read_to_string(std::path::Path::new(&root).join(".spira/checks.toml")).unwrap_or_default();
    let file = std::path::Path::new(&root).join(git_line(&["rev-parse", "--git-path", work::CHECKS_PASS_FILE])?);
    let clean = git_line(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| s.is_empty());
    Some((work::checks_pass_key_from(&|a| git_line(a), &toml)?, file, clean))
}

fn record_checks_pass() {
    if let Some((key, file, true)) = checks_pass_state() {
        let _ = std::fs::write(file, format!("{key}\n"));
    }
}

/// `work submit` after a passing `work checks` at the same tip runs them once, not twice.
fn checks_passed_at_tip() -> bool {
    let Some((key, file, clean)) = checks_pass_state() else { return false };
    let recorded = std::fs::read_to_string(&file).ok();
    let passed = work::checks_already_passed(recorded.as_deref(), &key, clean);
    if passed {
        println!("work submit: the repository's checks already passed at this tip (`work checks`); not run again");
    }
    passed
}

/// Build `package` from the worktree and return its binary. Inherits `RUSTC_WRAPPER`, the
/// `SPIRA_ADMIT_*` and `SCCACHE_*` vars, so the build is admitted and warm.
fn build_from_worktree(root: &std::path::Path, package: &str, dirs: &[std::path::PathBuf], shell: Option<&repo_contract::shell::ShellEnv>, budget_ms: u64) -> Result<std::path::PathBuf, String> {
    let target = work::check_target(std::env::var_os("CARGO_TARGET_DIR"), root);
    let secs = budget_ms.div_ceil(1000).max(900).to_string();
    let path = target.join("debug").join(package);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove the stale {}: {e}", path.display())),
    }
    let built = spira_config::bounded::bounded_for("cargo", &secs)
        .args(["build", "-p", package, "--bin", package])
        .envs(shell.into_iter().flat_map(|s| s.row_vars()))
        .env("PATH", work::trusted::join_path(dirs))
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !built.status.success() {
        let err = String::from_utf8_lossy(&built.stderr);
        return Err(format!("`cargo build -p {package}` failed ({}): {}", built.status, err.trim()));
    }
    work::trusted::resolve(&path.to_string_lossy(), &[]).ok_or_else(|| format!("{} is not an executable after the build", path.display()))
}

/// The worktree's flake dev shell environment, or `None` when it has no `flake.nix`.
fn flake_shell(root: &std::path::Path) -> Result<Option<repo_contract::shell::ShellEnv>, String> {
    if !std::fs::read(root.join("flake.nix")).is_ok_and(|f| repo_contract::shell::declares_dev_shell(&f)) {
        return Ok(None);
    }
    use repo_contract::shell::DevShell;
    match repo_contract::shell::shell_env(root, &repo_contract::shell::nix_bin()?, &repo_contract::shell::cache_dir()?)? {
        DevShell::Entered(env) => Ok(Some(env)),
        DevShell::Undeclared => Ok(None),
        DevShell::DoesNotEnter(e) => Err(e),
    }
}

/// Programs `work review` and `work checks` will exec that resolve nowhere, named so a missing
/// one is seen on turn one. A row that declares `build` is built by `work checks`, so it is not one.
fn unresolved_programs() -> Vec<String> {
    let dirs = work::trusted::process_dirs(true);
    let mut out = Vec::new();
    match spira_config::process::cfg("SPIRA_AGENT") {
        Ok(a) if !a.is_empty() && work::trusted::resolve(&a, &dirs).is_none() => out.push(format!("the reviewer {a:?} (SPIRA_AGENT) does not resolve; `work review` cannot launch it")),
        _ => {}
    }
    let root = spira_config::bounded::bounded("git").args(["rev-parse", "--show-toplevel"]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(root) = root.as_deref().map(std::path::Path::new) {
        let shell = flake_shell(root);
        let row_dirs = match &shell {
            Ok(s) => work::row_dirs(s.as_ref(), work::trusted::process_dirs(false)),
            Err(e) => {
                out.push(format!("`work checks` cannot enter the repository's dev shell: {e}"));
                Vec::new()
            }
        };
        for row in work::declared_rows(root).unwrap_or_default().into_iter().filter(|r| r.build.is_none()) {
            let argv = work::done_argv(&row.run);
            if shell.is_err() {
                break;
            }
            if let Some(prog) = argv.first().filter(|p| work::trusted::resolve(p, &row_dirs).is_none()) {
                let within = if matches!(shell, Ok(Some(_))) { " in the repository's flake dev shell" } else { "" };
                out.push(format!("check {:?} runs {prog:?}, which resolves nowhere{within}; `work checks` will fail it", row.name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t8iwu51)]
    #[test]
    fn an_absent_store_socket_is_not_running_after_the_wait_and_nothing_was_sent() {
        let dir = testkit::TempDir::new("work-not-running");
        let e = connect_waiting(dir.join("sock").to_str().unwrap(), Duration::from_millis(200)).unwrap_err();
        assert!(matches!(e, Unsent::NotRunning { .. }), "{e:?}");
    }

    #[test_id(taeb2w3)]
    #[test]
    fn a_socket_that_cannot_be_a_store_stays_a_fault_not_a_wait() {
        let dir = testkit::TempDir::new("work-not-a-store");
        std::fs::write(dir.join("a-file"), "").unwrap();
        let e = connect_waiting(dir.join("a-file").join("sock").to_str().unwrap(), Duration::from_secs(30)).unwrap_err();
        assert!(matches!(e, Unsent::Other(_)), "{e:?}");
    }

    #[test_id(t3czz4v)]
    #[test]
    fn the_refusal_reports_the_wait() {
        assert!(refusal(R::StoreNotRunning, "", &[("secs", "30")]).contains("store not running: nothing was sent; waited 30s"));
    }

    fn ports(transport: impl Fn(&[String], Duration) -> Result<(i32, String), Unsent> + 'static) -> ImagePorts {
        ImagePorts {
            root: ".".into(),
            tip: "abc".into(),
            bound: "sp-x".into(),
            actor: "dwarf".into(),
            started: std::time::Instant::now(),
            transport: Box::new(transport),
        }
    }

    fn recorded(forge_code: i32, output: &str) -> impl Fn(&[String], Duration) -> Result<(i32, String), Unsent> + 'static {
        let record = format!(r#"{{"state":"done","code":{forge_code},"output":{output:?}}}"#);
        move |req, _| {
            if req.iter().any(|a| a == "show") {
                assert_eq!(req.last().unwrap(), "wr-7");
                Ok((0, record.clone()))
            } else {
                Ok((0, "queued wr-7".into()))
            }
        }
    }

    #[test_id(t00wo9z)]
    #[test]
    fn a_recorded_ami_result_is_read_from_its_record_not_its_receipt() {
        use work::image::{Build, Ports};
        let p = ports(recorded(0, "success 123\n"));
        assert_eq!(p.result("b").unwrap(), Build::Success("123".into()));
    }

    #[test_id(t4kmipg)]
    #[test]
    fn a_recorded_ami_push_that_exits_nonzero_is_refused_naming_route_and_code() {
        use work::image::Ports;
        let e = ports(recorded(1, "push rejected")).push("b").unwrap_err();
        assert!(e.contains("`work forge ami-push`") && e.contains("exited 1") && e.contains("push rejected"), "{e}");
    }

    #[test_id(tjgjdqb)]
    #[test]
    fn a_reply_that_is_not_a_receipt_is_returned_without_polling() {
        let calls = std::cell::Cell::new(0);
        let send = |_: &[String], _: Duration| {
            calls.set(calls.get() + 1);
            Ok((2, "queued wr-7".to_string()))
        };
        assert_eq!(answered(&send, &["x".into()], Duration::from_secs(1)).unwrap(), (2, "queued wr-7".to_string()));
        let plain = |_: &[String], _: Duration| {
            calls.set(calls.get() + 1);
            Ok((0, "hello".to_string()))
        };
        assert_eq!(answered(&plain, &["x".into()], Duration::from_secs(1)).unwrap(), (0, "hello".to_string()));
        assert_eq!(calls.get(), 2);
    }

    #[test_id(txq65iv)]
    #[test]
    fn a_recorded_verb_prints_the_answer_of_its_record() {
        let send = recorded(0, "done it");
        assert_eq!(answered(&send, &["x".into()], Duration::from_secs(1)).unwrap(), (0, "done it".to_string()));
    }

    #[test_id(tx3ko1x)]
    #[test]
    fn dispatch_is_sent_only_after_the_push_record_is_read_and_each_answer_is_the_record() {
        use work::image::Ports;
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let seen = log.clone();
        let p = ports(move |req, _| {
            seen.borrow_mut().push(req.join(" "));
            if req.iter().any(|a| a == "show") {
                let n = req.last().unwrap();
                let (code, out) = if n == "wr-1" { (0, "pushed") } else if n == "wr-2" { (1, "no workflow") } else { (0, "line one\n?\nline two") };
                Ok((0, format!(r#"{{"state":"done","code":{code},"output":{out:?}}}"#)))
            } else {
                let n = seen.borrow().iter().filter(|r| !r.contains("show")).count();
                Ok((0, format!("queued wr-{n}")))
            }
        });
        p.push("b").unwrap();
        let e = p.dispatch("b").unwrap_err();
        assert!(e.contains("the forge refused the dispatch") && e.contains("exited 1") && e.contains("no workflow"), "{e}");
        assert_eq!(p.log_tail("9", 5), vec!["line one".to_string(), "line two".to_string()]);
        let log = log.borrow();
        let at = |needle: &str| log.iter().position(|r| r.contains(needle)).unwrap();
        assert!(at("ami-push") < at("show wr-1") && at("show wr-1") < at("ami-dispatch"), "{log:?}");
    }
}
