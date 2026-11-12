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
    let answer = send(&req, wait);
    spira_otel::run::phase("reply", asked.elapsed().as_millis() as u64);
    match answer {
        Ok((code, out)) => {
            spira_store::lc_call::print_answer(code, &out);
            { spira_otel::run::finish(code); std::process::exit(code) };
        }
        Err(e) => {
            eprintln!("{}", refusal(R::SocketUnreachable, "", &[("err", &e)]));
            { let code = CANNOT_TELL; spira_otel::run::finish(code); std::process::exit(code) };
        }
    }
}

/// The build directory this session owns: `CARGO_TARGET_DIR` when it is one under the configured base.
fn owned_build_dir() -> Option<String> {
    let base = std::env::var_os(spira_config::build_dir::BASE_ENV).filter(|b| !b.is_empty()).map(std::path::PathBuf::from);
    spira_config::build_dir::owned(std::env::var_os("CARGO_TARGET_DIR"), base.as_deref()).map(|p| p.to_string_lossy().into_owned())
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

/// `work law [<part> | <part>.<n> | §<part>.<n> | <slug>]`: reads the tracked `law/` tree of the worktree
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

struct ImagePorts {
    root: std::path::PathBuf,
    tip: String,
    forge: std::path::PathBuf,
    remote: String,
    started: std::time::Instant,
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
    fn forge(&self, verb: &str, args: &[&str]) -> forge::client::Reply {
        forge::client::run(&forge::client::route_of(&self.forge), verb, &self.root, args, None)
    }
}

impl work::image::Ports for ImagePorts {
    fn changed_paths(&self) -> Option<Vec<String>> {
        let out = self.git(&["diff", "--name-only", "--no-renames", &format!("{}...{}", work::CHECKS_BASE, self.tip)]).ok()?;
        Some(out.lines().map(String::from).collect())
    }
    fn push(&self, branch: &str) -> Result<(), String> {
        self.git(&["push", &self.remote, &format!("{}:refs/heads/{branch}", self.tip)]).map(drop)
    }
    fn delete(&self, branch: &str) {
        let _ = self.git(&["push", &self.remote, &format!(":refs/heads/{branch}")]);
    }
    fn dispatch(&self, branch: &str) -> Result<(), String> {
        let r = self.forge("ami-dispatch", &[branch]);
        if r.ok() { Ok(()) } else { Err(format!("the forge refused the dispatch: {}", r.err.trim())) }
    }
    fn result(&self, branch: &str) -> Option<work::image::Build> {
        work::image::Build::parse(&self.forge("ami-result", &[branch]).stdout()?)
    }
    fn log_tail(&self, run: &str, lines: usize) -> Vec<String> {
        let tail = lines.to_string();
        let out = self.forge("ami-log", &[run, &tail]).stdout().unwrap_or_default();
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
/// The diff is read before any image configuration, so a branch that leaves the recipe alone
/// never depends on it; with no landing ref there is no diff to read and nothing to gate.
fn image_gate(bead: &str) -> Result<(), String> {
    let root = git_line(&["rev-parse", "--show-toplevel"]).ok_or("not inside a git worktree")?;
    let tip = git_line(&["rev-parse", "HEAD"]).ok_or("cannot read HEAD")?;
    if git_line(&["rev-parse", "--verify", "--quiet", work::CHECKS_BASE]).is_none() {
        return Ok(());
    }
    let cfg = spira_config::process::cfg;
    let touched = git_line(&["diff", "--name-only", "--no-renames", &format!("{}...{tip}", work::CHECKS_BASE)])
        .map(|o| o.lines().map(String::from).collect::<Vec<_>>());
    if touched.as_deref().is_some_and(|c| !work::image::touches_recipe(c)) {
        return Ok(());
    }
    let ports = ImagePorts {
        root: root.into(),
        tip: tip.clone(),
        forge: cfg("SPIRA_FORGE")?.into(),
        remote: cfg("SPIRA_PUBLISH_REMOTE")?,
        started: std::time::Instant::now(),
    };
    let wait = spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_AMI_WAIT_SECS")?;
    match work::image::gate(&ports, bead, &tip, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), wait)? {
        work::image::Verdict::Skipped => {}
        work::image::Verdict::Built(run) => println!("work submit: the round image recipe builds (run {run})"),
    }
    Ok(())
}

/// `work checks [--list]`: the repository's `[[check]]` rows from `.spira/checks.toml`, run in
/// order from the worktree root. Every row runs; the exit is non-zero if any failed.
fn checks(args: &[String], via: &str) -> i32 {
    run_checks(args, false, via).0
}

/// [`checks`], with the failing test names of every red row that declares `deferred_to` when
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
    let rows = match habits::config::load_checks(root) {
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
        let scope = if row.run.contains(work::AFFECTED) { &*scope.get_or_insert_with(|| affected_scope(work::CHECKS_BASE)) } else { &none };
        let record_file = log.record_file(&row.name);
        let _ = std::fs::remove_file(&record_file);
        let started = std::time::Instant::now();
        let Some(argv) = work::check_argv(&row.run, scope, &branch, &record_file) else {
            println!("-- {}: ok (the diff affects no crate; nothing to check)", row.name);
            continue;
        };
        if argv != work::done_argv(&row.run) || row.run.contains("{branch}") {
            println!("runs: {}", argv.join(" "));
        }
        let Some((prog, rest)) = argv.split_first() else {
            println!("-- {}: FAIL (empty run)", row.name);
            failed.push(row.name.clone());
            log.row(row, None, started.elapsed().as_secs(), &record_file);
            continue;
        };
        let secs = row.budget_ms.div_ceil(1000).max(1).to_string();
        let dirs = work::trusted::process_dirs(false);
        let exe = match &row.build {
            Some(pkg) => match build_from_worktree(root, pkg, &dirs, row.budget_ms) {
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
                }
                found.unwrap_or_else(|| std::path::PathBuf::from(prog))
            }
        };
        // A cargo row waits for its compile slot BEFORE its budget starts: the budget bounds the
        // check, never the queue (sp-8oc3xj).
        let mut bounded = vec!["timeout".to_string(), secs, exe.to_string_lossy().into_owned()];
        bounded.extend(rest.iter().cloned());
        if prog == "cargo" {
            let who = std::env::var("SPIRA_WORK_BEAD_ID").unwrap_or_else(|_| "work-checks".into());
            bounded = work::admitted(bounded, std::env::var("RUSTC_WRAPPER").ok().as_deref(), &who);
        }
        // batch-job: bounded by the row's budget through `timeout`; the admission wait ahead of it is reported below
        let ran_from = std::time::Instant::now();
        let out = Command::new(&bounded[0]).args(&bounded[1..]).env("PATH", work::trusted::join_path(&dirs)).current_dir(root).output();
        match out {
            Ok(o) => {
                print!("{}", String::from_utf8_lossy(&o.stdout));
                let err = String::from_utf8_lossy(&o.stderr);
                eprint!("{err}");
                let waited = work::admission_wait_secs(&err);
                if let Some(w) = waited {
                    println!("{}: waited {w}s for a compile slot before its {} ms budget started", row.name, row.budget_ms);
                }
                log.row(row, o.status.code(), ran_from.elapsed().as_secs().saturating_sub(waited.unwrap_or(0)), &record_file);
                let stdout = String::from_utf8_lossy(&o.stdout);
                match work::row_outcome(o.status.code().unwrap_or(-1), row.deferred_to.as_deref()) {
                    work::Outcome::Ok => println!("-- {}: ok", row.name),
                    work::Outcome::Deferred => {
                        let stage = row.deferred_to.as_deref().unwrap_or_default();
                        println!("-- {}: deferred to the {stage} (cut at its {} ms budget): {}", row.name, row.budget_ms, selected_crates(work::CHECKS_BASE).join(" "));
                        deferred = true;
                    }
                    work::Outcome::Fail => {
                        let tests = work::failing_tests(&stdout);
                        if row.deferred_to.is_some() && base_red && !tests.is_empty() {
                            println!("-- {}: FAIL ({}) — waived: the red is recorded on the base (--unit-red-on-base)", row.name, o.status);
                            waived_tests.extend(tests);
                            deferred = true;
                        } else {
                            let exit = if row.deferred_to.is_some() {
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
        return work::Affected::Workspace;
    };
    work::affected_scope(&diff, &String::from_utf8_lossy(&meta)).unwrap_or_else(|e| {
        println!("work checks: cannot select the affected crates ({e}); checking the whole workspace");
        work::Affected::Workspace
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
    let head = git_line(&["rev-parse", "HEAD"])?;
    let base = git_line(&["rev-parse", "--verify", "--quiet", work::CHECKS_BASE]).unwrap_or_default();
    let toml = std::fs::read_to_string(std::path::Path::new(&root).join(".spira/checks.toml")).unwrap_or_default();
    let file = std::path::Path::new(&root).join(git_line(&["rev-parse", "--git-path", work::CHECKS_PASS_FILE])?);
    let clean = git_line(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| s.is_empty());
    Some((work::checks_pass_key(&head, &base, &toml), file, clean))
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
fn build_from_worktree(root: &std::path::Path, package: &str, dirs: &[std::path::PathBuf], budget_ms: u64) -> Result<std::path::PathBuf, String> {
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
        for row in habits::config::load_checks(root).unwrap_or_default().into_iter().filter(|r| r.build.is_none()) {
            let argv = work::done_argv(&row.run);
            if let Some(prog) = argv.first().filter(|p| work::trusted::resolve(p, &dirs).is_none()) {
                out.push(format!("check {:?} runs {prog:?}, which resolves nowhere; `work checks` will fail it", row.name));
            }
        }
    }
    out
}
