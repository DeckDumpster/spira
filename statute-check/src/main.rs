use statute_check::doors::{self, Registry};
use statute_check::hook::{self, Inputs};
use statute_check::{load_shelf, rank, render, Vocab};
use std::io::Read;
use std::path::PathBuf;

const USAGE: &str = "usage: statute-check [opts] <description...>\n       statute-check [opts] --command <cmdline> [--context <text>]\n       statute-check hook [opts]    (PreToolUse payload on stdin)\n\nopts: --dir <shelf>  --vocab <file>  --max <n>  --state <dir>  --log <file>  --doors <file>  --doors-log <file>\nPrints the statutes in force that govern the text, best first, each in full. Exit 1 when none\ngovern it; exit 2 when the shelf cannot be read.";

struct Opts {
    dir: Option<PathBuf>,
    vocab: Option<PathBuf>,
    state: Option<PathBuf>,
    log: Option<PathBuf>,
    doors: Option<PathBuf>,
    doors_log: Option<PathBuf>,
    max: usize,
    command: Option<String>,
    context: String,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { dir: None, vocab: None, state: None, log: None, doors: None, doors_log: None, max: 5, command: None, context: String::new(), words: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--dir" => o.dir = Some(val("--dir")?.into()),
            "--vocab" => o.vocab = Some(val("--vocab")?.into()),
            "--state" => o.state = Some(val("--state")?.into()),
            "--log" => o.log = Some(val("--log")?.into()),
            "--doors" => o.doors = Some(val("--doors")?.into()),
            "--doors-log" => o.doors_log = Some(val("--doors-log")?.into()),
            "--max" => o.max = val("--max")?.parse().map_err(|_| "--max needs a number".to_string())?,
            "--command" => o.command = Some(val("--command")?),
            "--context" => o.context = val("--context")?,
            "-h" | "--help" => return Err(String::new()),
            _ => o.words.push(a.clone()),
        }
    }
    Ok(o)
}

fn cfg_path(explicit: &Option<PathBuf>, key: &str, tail: &str) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    let v = spira_config::process::cfg(key)?;
    Ok(if tail.is_empty() { PathBuf::from(v) } else { PathBuf::from(v).join(tail) })
}

fn vocab_of(o: &Opts) -> Vocab {
    o.vocab
        .clone()
        .map(Ok)
        .unwrap_or_else(|| spira_config::resolve::locate_home_for_process().map(|h| h.join("statute-vocab")))
        .and_then(|p| Vocab::load(&p)).unwrap_or_else(|e| {
        eprintln!("statute-check: {e}; ranking without synonyms");
        Vocab::default()
    })
}

fn doors_of(o: &Opts) -> Option<Registry> {
    let loaded = o
        .doors
        .clone()
        .map(Ok)
        .unwrap_or_else(|| spira_config::resolve::locate_home_for_process().map(|h| h.join("doors.toml")))
        .and_then(|p| std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display())))
        .and_then(|t| doors::parse(&t));
    loaded.map_err(|e| eprintln!("statute-check: {e}; resources are not being matched to their tools")).ok()
}

fn run_hook(o: &Opts) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&raw) else { return 0 };
    if payload["tool_name"] != "Bash" {
        return 0;
    }
    let command = payload["tool_input"]["command"].as_str().unwrap_or("");
    let doors = doors_of(o);
    let found = hook::find(command, doors.as_ref());
    if found.classes.is_empty() && found.used_doors.is_empty() {
        return 0;
    }
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "statute-check", "hook");
    let shelf = cfg_path(&o.dir, "SPIRA_STATUTE_DIR", "").and_then(|d| load_shelf(&d));
    let state = cfg_path(&o.state, "SPIRA_RUN", "statute-check");
    let log = cfg_path(&o.log, "SPIRA_RUN", "statute-check/overrides.log");
    let doors_log = cfg_path(&o.doors_log, "SPIRA_RUN", "doors.log");
    let (Ok(state), Ok(log), Ok(doors_log)) = (state, log, doors_log) else {
        println!("{}", hook::response(&hook::Outcome::Block("statute-check: SPIRA_RUN is not resolvable, so the window cannot be kept; a decision is not made unread.".into())));
        return 0;
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let env_override = std::env::var(hook::OVERRIDE).ok();
    let persona = std::env::var("SPIRA_RUNE").ok().filter(|r| !r.is_empty());
    let session = payload["session_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| "session".into());
    let outcome = hook::decide(
        &Inputs {
            command,
            description: payload["tool_input"]["description"].as_str().unwrap_or(""),
            session: &session,
            persona: persona.as_deref().unwrap_or("unknown"),
            env_override: env_override.as_deref(),
            now,
            state_dir: &state,
            log: &log,
            doors_log: &doors_log,
        },
        &shelf,
        &vocab_of(o),
        doors.as_ref(),
    );
    print!("{}", hook::response(&outcome));
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (hook_mode, rest) = match args.first().map(String::as_str) {
        Some("hook") => (true, &args[1..]),
        _ => (false, &args[..]),
    };
    let o = match parse(rest) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("statute-check: {e}");
            }
            eprintln!("{USAGE}");
            spira_otel::oneshot::exit(2);
        }
    };
    if hook_mode {
        spira_otel::oneshot::exit(run_hook(&o));
    }
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "statute-check", "rank");
    let query = match &o.command {
        Some(c) => format!("{c} {}", o.context),
        None => o.words.join(" "),
    };
    if query.trim().is_empty() {
        eprintln!("{USAGE}");
        spira_otel::oneshot::exit(2);
    }
    let shelf = match cfg_path(&o.dir, "SPIRA_STATUTE_DIR", "").and_then(|d| load_shelf(&d)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("statute-check: refusing: {e}");
            spira_otel::oneshot::exit(2);
        }
    };
    let hits = rank(&shelf, &vocab_of(&o), &query, o.max);
    if hits.is_empty() {
        eprintln!("statute-check: no statute in force governs this text");
        spira_otel::oneshot::exit(1);
    }
    print!("{}", render(&hits));
    spira_otel::oneshot::exit(0);
}
