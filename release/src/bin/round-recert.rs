//! `round-recert` — the bare name a watcher's `RUN:` can name; `release round-recert` is the same verb.

fn main() -> std::process::ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("round-recert") {
        argv.remove(0);
    }
    std::process::ExitCode::from(release::round_recert::main(&argv))
}
