//! The `target-reap` program: argv in, `testenv::target_reap::run` out.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(testenv::target_reap::run(&args, &mut std::io::stdout(), &mut std::io::stderr()))
}
