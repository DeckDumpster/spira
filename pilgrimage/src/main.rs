//! `pilgrimage <open|walk|defects|wait|clean|handoff|done|status> ...` — drives and reads a
//! pilgrimage's recorded state. Usage is printed on a bad call.

use std::path::PathBuf;
use std::process::ExitCode;

use pilgrimage::machine::Event;
use pilgrimage::{close_clean, hand_off, record, FileStore, Mail};
use serde_json::json;

const USAGE: &str = "usage: pilgrimage open <bead>
       pilgrimage walk <bead> --landed-at <epoch>
       pilgrimage defects <bead> --count <n>
       pilgrimage wait <bead>
       pilgrimage clean <bead>
       pilgrimage handoff <bead>
       pilgrimage done <bead>
       pilgrimage status [--json] [<bead>]
   (state lives in $SPIRA_RUN/pilgrimage; --run DIR overrides)";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn run(args: Vec<String>) -> Result<String, (u8, String)> {
    let usage = |m: &str| (2u8, format!("{m}\n{USAGE}"));
    let verb = args.first().ok_or_else(|| usage("no verb"))?.clone();
    let run_dir = flag(&args, "--run").map(PathBuf::from).or_else(|| spira_config::process::cfg("SPIRA_RUN").ok().filter(|v| !v.is_empty()).map(PathBuf::from));
    let store = FileStore::new(run_dir.ok_or_else(|| usage("no run directory: pass --run or set SPIRA_RUN"))?.join("pilgrimage"));
    let sink = spira_config::process::metrics_sink("spira-pilgrimage");
    let now = spira_config::vtime::now_epoch();
    let mut id = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--run" | "--landed-at" | "--count" => {
                it.next();
            }
            s if s.starts_with('-') => {}
            _ => id = id.or_else(|| Some(a.clone())),
        }
    }
    let need = || id.clone().ok_or_else(|| usage("no bead id"));
    let num = |name: &str| flag(&args, name).and_then(|v| v.parse::<u64>().ok()).ok_or_else(|| usage(&format!("{name} <n> is required")));
    let step = |event: Event| record(&store, &sink, &need()?, event, now).map(|p| format!("{} {}", p.id, p.state().map_or("unrecorded", |s| s.name()))).map_err(|e| (1u8, e));
    match verb.as_str() {
        "open" => step(Event::Opened),
        "walk" => {
            let n = store.load(&need()?).map_or(0, |p| p.walks()) + 1;
            step(Event::WalkStarted { n, landed_at: num("--landed-at")? })
        }
        "defects" => step(Event::DefectsFiled { count: num("--count")? as u32 }),
        "wait" => step(Event::Waiting),
        "done" => step(Event::Done),
        "clean" => close_clean(&store, &sink, &Mail, &need()?, now).map(|ask| format!("{} concierge-walk ask {ask}", id.clone().unwrap_or_default())).map_err(|e| (1, e)),
        "handoff" => hand_off(&store, &sink, &Mail, &need()?, now).map(|ask| format!("ask {ask}")).map_err(|e| (1, e)),
        "status" => {
            let found: Vec<_> = match &id {
                Some(i) => store.load(i).into_iter().collect(),
                None => store.all(),
            };
            if let (Some(i), true) = (&id, found.is_empty()) {
                return Err((1, format!("pilgrimage: {i} has no recorded events")));
            }
            if args.iter().any(|a| a == "--json") {
                return Ok(json!({"pilgrimages": found.iter().map(|p| p.status_json()).collect::<Vec<_>>()}).to_string());
            }
            Ok(found.iter().map(|p| format!("{} {} walks={} defects={}", p.id, p.state().map_or("unrecorded", |s| s.name()), p.walks(), p.defects())).collect::<Vec<_>>().join("\n"))
        }
        _ => Err(usage("unknown verb")),
    }
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err((code, msg)) => {
            eprintln!("{msg}");
            ExitCode::from(code)
        }
    }
}
