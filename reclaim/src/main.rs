//! reclaim pass [--glob PATTERN]... [--require PATTERN]... [--lease-dir DIR] [--idle-days N]
//!              [--min-free-gib N] [--target-free-gib N] [--cache-dir DIR] [--cache-cap-gib N] [--dry-run]

use reclaim::{held_paths, run, Config};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: reclaim pass --glob PATTERN [--glob PATTERN]... [--require PATTERN]... [--lease-dir DIR] [--idle-days N] [--min-free-gib N] [--target-free-gib N] [--cache-dir DIR] [--cache-cap-gib N] [--dry-run]";

fn parse(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config {
        patterns: vec![],
        require: vec![],
        lease_dir: None,
        idle_secs: 2 * 86400,
        min_free_mib: 70 << 10,
        target_free_mib: 100 << 10,
        cache_dir: None,
        cache_cap_bytes: 40 << 30,
        dry: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--dry-run" {
            cfg.dry = true;
            continue;
        }
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        let n = || v.parse::<u64>().map_err(|_| format!("{a} {v:?} is not a number"));
        match a.as_str() {
            "--glob" => cfg.patterns.push(v.clone()),
            "--require" => cfg.require.push(v.clone()),
            "--lease-dir" => cfg.lease_dir = Some(PathBuf::from(v)),
            "--idle-days" => cfg.idle_secs = n()? * 86400,
            "--min-free-gib" => cfg.min_free_mib = n()? << 10,
            "--target-free-gib" => cfg.target_free_mib = n()? << 10,
            "--cache-dir" => cfg.cache_dir = Some(PathBuf::from(v)),
            "--cache-cap-gib" => cfg.cache_cap_bytes = n()? << 30,
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    if cfg.patterns.is_empty() {
        return Err("at least one --glob is required".into());
    }
    Ok(cfg)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("pass") {
        eprintln!("reclaim: unknown command\n{USAGE}");
        return ExitCode::from(2);
    }
    let cfg = match parse(&args[1..]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("reclaim: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let held = held_paths(Path::new("/proc"));
    let probe = cfg.patterns.iter().find_map(|p| p.split('*').next().map(PathBuf::from)).unwrap_or_default();
    let probe = probe.ancestors().find(|a| a.exists()).unwrap_or(Path::new("/")).to_path_buf();
    let free = || spira_config::room::statvfs_mib(&probe);
    match run(&cfg, now, &held, &free, &mut |l| println!("reclaim: {l}")) {
        Ok(b) => {
            println!("reclaim: freed {} MiB", b >> 20);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("reclaim: {e}");
            ExitCode::from(3)
        }
    }
}
