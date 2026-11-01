//! `sim-replay [--fixtures <dir>]`: every whole-slice replay check (sim::replay::checks), each
//! named as it runs, over the slices in `<dir>` (default: the tree it was built from). Exit 0
//! when all pass; a failing check panics with its assertion and the process exits 101.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [flag, dir] if flag == "--fixtures" => sim::replay::checks::use_fixtures(dir.into()),
        _ => {
            eprintln!("usage: sim-replay [--fixtures <dir>]");
            std::process::exit(2);
        }
    }
    for (name, check) in sim::replay::checks::CHECKS {
        let t0 = std::time::Instant::now(); // clock-safe: reported, never asserted
        check();
        println!("ok {name} ({} ms)", t0.elapsed().as_millis()); // clock-safe
    }
    println!("sim-replay: {} checks passed", sim::replay::checks::CHECKS.len());
}
