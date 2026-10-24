use std::io::Read;
use std::time::Duration;

fn main() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let endpoint = spira_config::process::metrics_endpoint();
    let sink = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-habits", Duration::from_millis(300));
    let state = std::env::temp_dir().join("spira-habits");
    let out = std::panic::catch_unwind(|| habits::dispatch(&raw, &state, &sink)).ok().flatten();
    if let Some(out) = out {
        println!("{out}");
    }
}
