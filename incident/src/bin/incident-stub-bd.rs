//! The stateful stand-in bd for incident's suites — see `incident::stub_bd`.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match incident::stub_bd::run(&argv, &mut std::io::stdin()) {
        Ok(out) => print!("{out}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
