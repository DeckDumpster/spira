use sccache_dav::{config_from_env, router, AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let cfg = match config_from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sccache-dav: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cfg.root) {
        eprintln!("sccache-dav: creating store root {}: {e}", cfg.root.display());
        std::process::exit(2);
    }
    let listener = match tokio::net::TcpListener::bind(&cfg.addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sccache-dav: bind {}: {e}", cfg.addr);
            std::process::exit(2);
        }
    };
    eprintln!(
        "sccache-dav: serving {} from {} (bearer auth {})",
        cfg.addr,
        cfg.root.display(),
        if cfg.token.is_some() { "required" } else { "OFF" }
    );
    let state = Arc::new(AppState { root: cfg.root, token: cfg.token });
    let app = router(state);
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("sccache-dav: serve: {e}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
