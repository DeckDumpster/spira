use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let out = test_registry::cli::run(&args, now, &spira_config::process::cfg, &|p| {
        test_registry::open(p)
    });
    if !out.stdout.is_empty() {
        println!("{}", out.stdout.trim_end_matches('\n'));
    }
    if !out.stderr.is_empty() {
        eprintln!("test-registry: {}", out.stderr);
    }
    std::process::exit(out.code);
}
