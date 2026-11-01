//! `sim-replay`: every whole-slice replay check (sim::replay::checks), each named as it runs.
//! Exit 0 when all pass; a failing check panics with its assertion and the process exits 101.

fn main() {
    for (name, check) in sim::replay::checks::CHECKS {
        let t0 = std::time::Instant::now(); // clock-safe: reported, never asserted
        check();
        println!("ok {name} ({} ms)", t0.elapsed().as_millis()); // clock-safe
    }
    println!("sim-replay: {} checks passed", sim::replay::checks::CHECKS.len());
}
