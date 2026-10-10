use std::future::IntoFuture;
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
    let tailnet = match sccache_dav::tailnet_addr_from_config().and_then(|t| sccache_dav::listen_addrs(&cfg.addr, &t)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sccache-dav: {e}");
            std::process::exit(2);
        }
    };
    let mut listeners = Vec::new();
    for addr in &tailnet {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => listeners.push(l),
            Err(e) => {
                eprintln!("sccache-dav: bind {addr}: {e}");
                std::process::exit(2);
            }
        }
    }
    eprintln!(
        "sccache-dav: serving {} from {} (bearer auth {})",
        tailnet.join(" + "),
        cfg.root.display(),
        if cfg.token.is_some() { "required" } else { "OFF" }
    );
    let cap = match sccache_dav::cap_bytes_from_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sccache-dav: {e}");
            std::process::exit(2);
        }
    };
    let spec = std::env::var("SCCACHE_DAV_ADDR").unwrap_or_default();
    if spec.trim().starts_with(spira_config::hostaddr::AUTO) {
        let bound = cfg.addr.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                let (s, b) = (spec.clone(), bound.clone());
                if tokio::task::spawn_blocking(move || sccache_dav::spec_drifted(&s, &b)).await.unwrap_or(false) {
                    eprintln!("sccache-dav: this host's address moved off {bound}; exiting to rebind");
                    std::process::exit(75);
                }
            }
        });
    }
    let sweep_root = cfg.root.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(sccache_dav::SWEEP_EVERY);
        loop {
            tick.tick().await;
            let root = sweep_root.clone();
            if let Ok(ev) = tokio::task::spawn_blocking(move || sccache_dav::evict_to_cap(&root, cap)).await {
                if ev.files > 0 {
                    eprintln!("sccache-dav: evicted {} files ({} bytes); {} bytes remain (cap {cap})", ev.files, ev.bytes, ev.remaining);
                }
            }
        }
    });
    let state = Arc::new(AppState { root: cfg.root, token: cfg.token });
    let app = router(state);
    let mut servers = tokio::task::JoinSet::new();
    for listener in listeners {
        servers.spawn(axum::serve(listener, app.clone()).with_graceful_shutdown(shutdown_signal()).into_future());
    }
    while let Some(done) = servers.join_next().await {
        if let Err(e) = done.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())) {
            eprintln!("sccache-dav: serve: {e}");
            std::process::exit(1);
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
