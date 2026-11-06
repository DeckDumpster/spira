//! spira-claim — bead attempt accounting, the CHECK 4 poison decision, and epic-first claim
//! selection. The verbs live in the library ([`spira_claim::cli`]); this is the process.

fn main() {
    spira_otel::run::begin("spira-claim", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let args: Vec<String> = std::env::args().skip(1).collect();
    spira_otel::run::operation(args.first().map_or("none", String::as_str));
    let o = spira_claim::cli::dispatch(&args, &mut std::io::stdin());
    print!("{}", o.out);
    if !o.err.is_empty() {
        eprintln!("{}", o.err.trim_end());
    }
    let code = o.code;
    spira_otel::run::finish(code);
    std::process::exit(code)
}
