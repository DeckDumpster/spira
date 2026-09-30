//! `doctor` — runs every check and prints the report. See DESIGN.md §3.

use doctor::real::Real;
use std::process::ExitCode;

fn main() -> ExitCode {
    let w = Real::new();
    let rc = doctor::run(&w);
    ExitCode::from(rc.clamp(0, 255) as u8)
}
