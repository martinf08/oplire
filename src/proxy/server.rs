use axum::{middleware, routing::get, routing::post, Router};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tracing::info;

use crate::{
    config::ProxyConfig,
    proxy::handlers::{
        handle_health, handle_messages, handle_model_detail, handle_models, ProxyState,
    },
};

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
}

pub async fn start_proxy_server(config: ProxyConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = config.listen_addr.parse()?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "Oplire binds to loopback only; put an authenticated reverse proxy in front if remote access is required"
    );
    let upstream = reqwest::Url::parse(&config.upstream_base_url)?;
    anyhow::ensure!(
        matches!(upstream.scheme(), "http" | "https")
            && upstream.username().is_empty()
            && upstream.password().is_none()
            && upstream.query().is_none()
            && upstream.fragment().is_none(),
        "upstream URL must be HTTP(S) without embedded credentials, query, or fragment"
    );
    let client = build_client()?;
    let state = Arc::new(ProxyState { config, client });
    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/v1/models/:model_id", get(handle_model_detail))
        .route("/v1/messages", post(handle_messages))
        .route("/v1/chat/completions", post(handle_messages))
        .layer(middleware::from_fn(log_requests))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("proxy listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn log_requests(
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    info!("{} {} -> {}", method, path, response.status());
    response
}
