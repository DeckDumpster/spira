//! `bdsim <fixture-file> <bd-subcommand> [args...]` — see `bead::bdsim`.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(bead::bdsim::run(&argv));
}
