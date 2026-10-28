//! `round-recert` — the bare name a watcher's `RUN:` can name; `release round-recert` is the same verb.

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(release::round_recert::main(&argv))
}
