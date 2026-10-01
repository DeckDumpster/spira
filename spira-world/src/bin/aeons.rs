//! aeons.sh — how many aeons may run at once, and how many are running. Replaces
//! spira/aeons.sh (bash).
//!
//!   aeons.sh                  what the ceiling is, what is live, what the real ceiling is
//!   aeons.sh set <n>          allow at most <n> aeons at once, across every persona and lane
//!   aeons.sh unset            remove the fleet ceiling (back to pool + one per lane)
//!   aeons.sh pool <n>         set the TASK pool (SPIRA_MAX_AEONS) — the older, narrower knob
//!
//! `status` always prints BOTH the task pool and the fleet ceiling and the sum they imply
//! — the whole reason this tool exists rather than a line in a file (bash's own header:
//! "anybody reaching for 'one aeon' has to know that lanes exist... that is a rung-4
//! problem"). Config reads and writes go straight through the `spira-config` crate — the
//! crate that already owns the box's config file (law-config-through-the-cli-only) — never
//! through conf.sh's bash wrapper. The live-now count and the lane report still need lib.sh (see
//! `spira_world::fleet` and `spira_world::seam` for why, and why it is one seam, not a
//! hand-rolled `.fayth` parser).

use std::env;
use std::path::PathBuf;

use spira_config::SpiraToml;
use spira_world::{seam, sysctl};

fn toml_path() -> Option<PathBuf> {
    env::var_os("SPIRA_TOML_FILE")
        .map(PathBuf::from)
        .or_else(|| spira_config::discover(None))
}

fn load_doc() -> Result<(PathBuf, SpiraToml), String> {
    let Some(path) = toml_path() else {
        return Err("aeons.sh: no config file found — SPIRA_TOML_FILE is unset and nothing on the \
            standard search path exists. Legacy spira.conf auto-conversion (conf.sh's \
            spira_toml_write_target) is out of this wave's scope; if this box genuinely predates \
            the toml cutover, run any conf.sh-sourcing tool once to convert it first."
            .to_string());
    };
    let doc = spira_config::load(&path).map_err(|e| format!("aeons.sh: {e}"))?;
    Ok((path, doc))
}

fn live_now() -> String {
    sysctl::run_lines(&["list-units", "spira-aeon-*", "--no-legend"]).len().to_string()
}

fn lane_caps(lib_sh: Option<&std::path::Path>) -> (u32, String) {
    let Some(lib_sh) = lib_sh else { return (0, "?".to_string()) };
    let (out, ok) = seam::run(lib_sh, "aeons-lane-caps", seam::LANE_CAPS, &[], "");
    if !ok {
        return (0, "?".to_string());
    }
    let pairs = seam::parse_lane_caps(&out);
    if pairs.is_empty() {
        return (0, "none".to_string());
    }
    let total: u32 = pairs.iter().map(|(_, n)| n).sum();
    let detail = pairs.iter().map(|(n, c)| format!("{n}={c}")).collect::<Vec<_>>().join(", ");
    (total, detail)
}

fn die(msg: &str) -> ! {
    eprintln!("aeons.sh: {msg}");
    std::process::exit(1);
}

fn cmd_status() -> i32 {
    let doc = match load_doc() {
        Ok((_, d)) => Some(d),
        Err(e) => {
            eprintln!("{e}");
            None
        }
    };
    let spira = doc.as_ref().and_then(|d| d.spira.as_ref());
    let ceiling = spira.and_then(|s| s.max_live_aeons);
    let pool = spira.and_then(|s| s.max_aeons);
    let lanes_cap = spira.and_then(|s| s.lanes_max_live);

    let exe = env::current_exe().ok();
    let lib_sh = exe.as_deref().and_then(|e| spira_world::locate_home(e)).map(|h| h.join("lib.sh"));
    let (lt, ld) = lane_caps(lib_sh.as_deref());
    let live = live_now();

    println!("  live now          {live}");
    match ceiling {
        Some(c) => println!("  fleet ceiling     {c}   (SPIRA_MAX_LIVE_AEONS — counts every aeon)"),
        None => println!("  fleet ceiling     none  (SPIRA_MAX_LIVE_AEONS unset)"),
    }
    match pool {
        Some(p) => println!("  task pool         {p}   (SPIRA_MAX_AEONS — builders and other task fayths)"),
        None => println!("  task pool         ?   (SPIRA_MAX_AEONS — builders and other task fayths)"),
    }
    println!("  lanes             {lt}   ({ld} — each draws OUTSIDE the pool)");
    let pool_n = pool.unwrap_or(0);
    if let Some(cap) = lanes_cap {
        println!("  lane cap          {cap}   (SPIRA_LANES_MAX_LIVE — lanes share this many fleet slots)");
        let task_min = pool_n.saturating_sub(cap);
        println!("  effective split   builders {task_min}-{pool_n}, lanes 0-{cap}");
    }
    if let Some(c) = ceiling {
        println!(
            "  ---\n  at most {c} aeon(s) at once. Without the ceiling it would be {} ({pool_n} pool + {lt} lanes).",
            pool_n + lt
        );
    } else {
        println!(
            "  ---\n  at most {} aeon(s) at once ({pool_n} pool + {lt} lanes). `aeons.sh set <n>` to cap the total.",
            pool_n + lt
        );
    }
    if let Ok((path, _)) = load_doc() {
        println!("  config            {}", path.display());
    }
    0
}

fn parse_n(s: Option<&String>, usage: &str) -> u32 {
    match s.and_then(|v| v.parse::<u32>().ok()) {
        Some(n) => n,
        None => die(usage),
    }
}

fn cmd_set(args: &[String]) -> i32 {
    let n = parse_n(args.first(), "usage: aeons.sh set <n>   (a whole number; 0 stops summoning entirely)");
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::set_path(&doc, "spira.max_live_aeons", &n.to_string())
        .unwrap_or_else(|e| die(&format!("could not write the fleet ceiling: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not write the fleet ceiling: {e}"));
    }
    println!("fleet ceiling set to {n} — in force on the next sentinel pass (<=2 min), no restart needed.");
    if n == 0 {
        println!("NOTE: 0 stops every summon. `aeons.sh unset` or `set <n>` to resume; live aeons are untouched.");
    }
    cmd_status()
}

fn cmd_unset() -> i32 {
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::unset_path(&doc, "spira.max_live_aeons")
        .unwrap_or_else(|e| die(&format!("could not remove the fleet ceiling: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not remove the fleet ceiling: {e}"));
    }
    println!("fleet ceiling removed — the limit is again the task pool plus one per lane.");
    0
}

fn cmd_pool(args: &[String]) -> i32 {
    let n = parse_n(args.first(), "usage: aeons.sh pool <n>");
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::set_path(&doc, "spira.max_aeons", &n.to_string())
        .unwrap_or_else(|e| die(&format!("could not write the task pool: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not write the task pool: {e}"));
    }
    println!("task pool set to {n}. NOTE: lanes draw outside it — `aeons.sh set <n>` is the total.");
    0
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "status".to_string());
    let rest: Vec<String> = if args.is_empty() { Vec::new() } else { args[1..].to_vec() };
    let rc = match cmd.as_str() {
        "status" => cmd_status(),
        "set" => cmd_set(&rest),
        "unset" => cmd_unset(),
        "pool" => cmd_pool(&rest),
        "-h" | "--help" | "help" => {
            println!("aeons.sh — how many aeons may run at once, and how many are running.\n");
            println!("  aeons.sh                  what the ceiling is, what is live, what the real ceiling is");
            println!("  aeons.sh set <n>          allow at most <n> aeons at once, across every persona and lane");
            println!("  aeons.sh unset            remove the fleet ceiling (back to pool + one per lane)");
            println!("  aeons.sh pool <n>         set the TASK pool (SPIRA_MAX_AEONS) — the older, narrower knob");
            0
        }
        other => die(&format!("unknown command '{other}' — try: status | set <n> | unset | pool <n>")),
    };
    std::process::exit(rc);
}
