fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(round_vm::cli::main_with(args));
}
