//! `design-review post | reply | close | status` — see `design-review -h`.

use design_review::{run, Ports};
use runtime::ports::WallClock;
use std::io::Read;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let verb = argv.first().map(String::as_str);
    let metrics = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "design-review", verb.filter(|v| matches!(*v, "post" | "reply" | "close" | "status")).unwrap_or("none"));
    let margin: Box<dyn margin_client::Margin> = match margin_client::HttpMargin::configured(spira_config::process::cfg) {
        Ok(m) => Box::new(m),
        Err(e) => Box::new(margin_client::Unconfigured(e)),
    };
    let ports = (|| -> Result<Ports, String> {
        let stdin = if matches!(verb, Some("post" | "reply")) {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| format!("reading stdin: {e}"))?;
            s
        } else {
            String::new()
        };
        Ok(Ports {
            run_dir: spira_config::resolve::run_dir_for_process()?,
            wiki: spira_config::process::cfg("SPIRA_WIKI")?.into(),
            margin,
            clock: Box::new(WallClock),
            stdin,
        })
    })();
    let out = match ports {
        Ok(p) => run(&argv, &p),
        Err(e) => design_review::Outcome { code: 1, stdout: String::new(), stderr: format!("design-review: {e}\n") },
    };
    metrics.end(spira_otel::metrics::outcome_of_exit(std::process::ExitCode::from(out.code.clamp(0, 255) as u8)));
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    std::process::ExitCode::from(out.code.clamp(0, 255) as u8)
}
