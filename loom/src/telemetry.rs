//! Per-request OTLP metrics: every request is one run of component `loom`, its operation the
//! page it asked for. The push runs off the request path.

use axum::extract::{Request, State};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::Response;
use axum::Router;
use spira_otel::metrics::Run;
use std::sync::Arc;

const PAGES: [&str; 10] = ["shots", "lifecycle", "rounds", "stuck", "beads", "units", "states", "dwarves", "ops", "now"];

/// A bounded operation label: the first path segment (after `/api/`) when it is a page this server has, else `other`.
pub fn operation(path: &str) -> &'static str {
    let rest = path.trim_start_matches('/');
    let rest = rest.strip_prefix("api/").unwrap_or(rest);
    match rest.split('/').next().unwrap_or("") {
        "" => "now",
        seg => PAGES.iter().find(|p| **p == seg).copied().unwrap_or("other"),
    }
}

fn outcome(status: u16) -> &'static str {
    match status {
        0..=399 => "ok",
        400..=499 => "client_error",
        _ => "error",
    }
}

async fn meter(State(endpoint): State<Arc<Option<String>>>, req: Request, next: Next) -> Response {
    let run = Run::begin(endpoint.as_deref(), "loom", operation(req.uri().path()));
    let resp = next.run(req).await;
    let outcome = outcome(resp.status().as_u16());
    tokio::task::spawn_blocking(move || run.end(outcome));
    resp
}

/// `app` with every request measured; `endpoint` is the metrics store, `None` disables the push.
pub fn metered(app: Router, endpoint: Option<String>) -> Router {
    app.layer(from_fn_with_state(Arc::new(endpoint), meter))
}
