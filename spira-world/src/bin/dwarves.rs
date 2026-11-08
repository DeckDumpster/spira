//! dwarves.sh — how many dwarves may run at once, and how many are running. Replaces
//! spira/dwarves.sh (bash).
//!
//!   dwarves.sh                  what the ceiling is, what is live, what the real ceiling is
//!   dwarves.sh set <n>          allow at most <n> dwarves at once, across every persona and lane
//!   dwarves.sh unset            remove the fleet ceiling (back to pool + one per lane)
//!   dwarves.sh pool <n>         set the TASK pool (SPIRA_MAX_DWARVES) — the older, narrower knob
//!   dwarves.sh usage            each live dwarf's CPU, IO per disk, memory and disk, now
//!   dwarves.sh usage bead <id>  a bead's sessions and totals, from the metrics store
//!
//! `status` always prints BOTH the task pool and the fleet ceiling and the sum they imply
//! — the whole reason this tool exists rather than a line in a file (bash's own header:
//! "anybody reaching for 'one dwarf' has to know that lanes exist... that is a rung-4
//! problem"). Config reads and writes go straight through the `spira-config` crate — the
//! crate that already owns the box's config file (law-config-through-the-cli-only) — never
//! through conf.sh's bash wrapper. The live-now count and the lane report still need lib.sh (see
//! `spira_world::fleet` and `spira_world::seam` for why, and why it is one seam, not a
//! hand-rolled `.rune` parser).

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
        return Err("dwarves.sh: no config file found — SPIRA_TOML_FILE is unset and nothing on the \
            standard search path exists. Legacy spira.conf auto-conversion (conf.sh's \
            spira_toml_write_target) is out of this wave's scope; if this box genuinely predates \
            the toml cutover, run any conf.sh-sourcing tool once to convert it first."
            .to_string());
    };
    let doc = spira_config::load(&path).map_err(|e| format!("dwarves.sh: {e}"))?;
    Ok((path, doc))
}

fn live_now() -> String {
    sysctl::live_dwarf_units().len().to_string()
}

fn lane_caps(lib_sh: Option<&std::path::Path>) -> (u32, String) {
    let Some(lib_sh) = lib_sh else { return (0, "?".to_string()) };
    let (out, ok) = seam::run(lib_sh, "dwarves-lane-caps", seam::LANE_CAPS, &[], "");
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
    eprintln!("dwarves.sh: {msg}");
    spira_otel::oneshot::exit(1);
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
    let ceiling = spira.and_then(|s| spira_config::said(s.get_u32("SPIRA_MAX_LIVE_DWARVES")));
    let pool = spira.and_then(|s| spira_config::said(s.get_u32("SPIRA_MAX_DWARVES")));
    let lanes_cap = spira.and_then(|s| spira_config::said(s.get_u32("SPIRA_LANES_MAX_LIVE")));

    let lib_sh = spira_world::locate_home().map(|h| h.join("lib.sh"));
    let (lt, ld) = lane_caps(lib_sh.as_deref());
    let live = live_now();

    println!("  live now          {live}");
    match ceiling {
        Some(c) => println!("  fleet ceiling     {c}   (SPIRA_MAX_LIVE_DWARVES — counts every dwarf)"),
        None => println!("  fleet ceiling     none  (SPIRA_MAX_LIVE_DWARVES unset)"),
    }
    match pool {
        Some(p) => println!("  task pool         {p}   (SPIRA_MAX_DWARVES — builders and other task runes)"),
        None => println!("  task pool         ?   (SPIRA_MAX_DWARVES — builders and other task runes)"),
    }
    println!("  lanes             {lt}   ({ld} — each draws OUTSIDE the pool)");
    let pool_n = pool.unwrap_or(0);
    if let Some(cap) = lanes_cap {
        println!("  lane cap          {cap}   (SPIRA_LANES_MAX_LIVE — lanes share this many fleet slots)");
        let task_min = pool_n.saturating_sub(cap);
        println!("  effective split   builders {task_min}-{pool_n}, lanes 0-{cap}");
    }
    if let (Some(c), Some(cap)) = (ceiling, lanes_cap) {
        if let Ok(l) = live.parse::<u32>() {
            if l > c {
                println!("  spill             live {l} / ceiling {c} — {} lane over, draining", l - c);
            }
        }
        println!("  lane spill        up to {cap} over the ceiling while a lane has ready work; no refill until live <= {c}");
    }
    if let Some(c) = ceiling {
        println!(
            "  ---\n  at most {c} dwarf(s) at once. Without the ceiling it would be {} ({pool_n} pool + {lt} lanes).",
            pool_n + lt
        );
    } else {
        println!(
            "  ---\n  at most {} dwarf(s) at once ({pool_n} pool + {lt} lanes). `dwarves.sh set <n>` to cap the total.",
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
    let n = parse_n(args.first(), "usage: dwarves.sh set <n>   (a whole number; 0 stops calling entirely)");
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::set_path(&doc, "spira.max_live_dwarves", &n.to_string())
        .unwrap_or_else(|e| die(&format!("could not write the fleet ceiling: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not write the fleet ceiling: {e}"));
    }
    println!("fleet ceiling set to {n} — in force on the next sentinel pass (<=2 min), no restart needed.");
    if n == 0 {
        println!("NOTE: 0 stops every call. `dwarves.sh unset` or `set <n>` to resume; live dwarves are untouched.");
    }
    cmd_status()
}

fn cmd_unset() -> i32 {
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::unset_path(&doc, "spira.max_live_dwarves")
        .unwrap_or_else(|e| die(&format!("could not remove the fleet ceiling: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not remove the fleet ceiling: {e}"));
    }
    println!("fleet ceiling removed — the limit is again the task pool plus one per lane.");
    0
}

fn cmd_pool(args: &[String]) -> i32 {
    let n = parse_n(args.first(), "usage: dwarves.sh pool <n>");
    let (path, doc) = match load_doc() {
        Ok(v) => v,
        Err(e) => die(&e),
    };
    let new = spira_config::set_path(&doc, "spira.max_dwarves", &n.to_string())
        .unwrap_or_else(|e| die(&format!("could not write the task pool: {e}")));
    if let Err(e) = spira_config::serialize_and_write(&path, &new) {
        die(&format!("could not write the task pool: {e}"));
    }
    println!("task pool set to {n}. NOTE: lanes draw outside it — `dwarves.sh set <n>` is the total.");
    0
}

/// `usage [--interval <secs>]`: every live dwarf and hand scope, read twice `<secs>` apart
/// (1 by default) for the current rates. `usage bead <id> [--days <n>]`: the bead's sessions
/// from the metrics store (30 days by default). Read-only.
fn cmd_usage(args: &[String]) -> i32 {
    let flag = |name: &str, default: f64| -> f64 {
        match args.iter().position(|a| a == name) {
            None => default,
            Some(i) => args.get(i + 1).and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0).unwrap_or_else(|| die(&format!("{name} takes a positive number"))),
        }
    };
    if args.first().map(String::as_str) == Some("bead") {
        let Some(bead) = args.get(1).filter(|b| !b.starts_with('-') && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')) else { die("usage: dwarves.sh usage bead <id> [--days <n>]") };
        let days = flag("--days", 30.0);
        let store = spira_otel::query::Store::new(spira_config::process::metrics_endpoint().as_deref()).unwrap_or_else(|e| die(&e.to_string()));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let series = store.export(&spira_world::usage::bead_selector(bead), now.saturating_sub((days * 86_400.0) as u64)).unwrap_or_else(|e| die(&e.to_string()));
        if series.is_empty() {
            die(&format!("no usage series for {bead} in the last {days} day(s)"));
        }
        print!("{}", spira_world::usage::render_bead(bead, &spira_world::usage::bead_totals(&series)));
        return 0;
    }
    let interval = flag("--interval", 1.0);
    let run = spira_world::spira_run().unwrap_or_else(|e| die(&e));
    let roots = runtime::usage::Roots::live(&run);
    let before = runtime::usage::sample(&roots);
    let t0 = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_secs_f64(interval)); // waits-on: nothing — the rate is the movement over a fixed --interval; cap --interval
    let after = runtime::usage::sample(&roots);
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    print!("{}", spira_world::usage::render_live(&before, &after, t0.elapsed().as_secs_f64(), now_ms));
    0
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "status".to_string());
    let rest: Vec<String> = if args.is_empty() { Vec::new() } else { args[1..].to_vec() };
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-dwarves", &cmd);
    let rc = match cmd.as_str() {
        "status" => cmd_status(),
        "set" => cmd_set(&rest),
        "unset" => cmd_unset(),
        "pool" => cmd_pool(&rest),
        "usage" => cmd_usage(&rest),
        "-h" | "--help" | "help" => {
            println!("dwarves.sh — how many dwarves may run at once, and how many are running.\n");
            println!("  dwarves.sh                  what the ceiling is, what is live, what the real ceiling is");
            println!("  dwarves.sh set <n>          allow at most <n> dwarves at once, across every persona and lane");
            println!("  dwarves.sh unset            remove the fleet ceiling (back to pool + one per lane)");
            println!("  dwarves.sh pool <n>         set the TASK pool (SPIRA_MAX_DWARVES) — the older, narrower knob");
            println!("  dwarves.sh usage            each live dwarf's CPU, IO per disk, memory and disk, now");
            println!("  dwarves.sh usage bead <id>  a bead's sessions and totals, from the metrics store");
            0
        }
        other => die(&format!("unknown command '{other}' — try: status | set <n> | unset | pool <n> | usage")),
    };
    spira_otel::oneshot::exit(rc);
}
