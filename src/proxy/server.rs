use axum::{middleware, routing::get, routing::post, Router};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tracing::info;

use crate::{
    config::ProxyConfig,
    proxy::handlers::{
        handle_health, handle_messages, handle_model_detail, handle_models, ProxyState,
    },
};

pub fn build_client(egress_proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        // Do not silently inherit HTTP_PROXY/HTTPS_PROXY from the host. Egress
        // is direct unless OPLIRE_EGRESS_PROXY (or --egress-proxy) is explicit.
        .no_proxy();

    if let Some(proxy_url) = egress_proxy {
        let parsed = reqwest::Url::parse(proxy_url).map_err(|_| {
            anyhow::anyhow!("invalid egress proxy URL; use an HTTP(S) or SOCKS5 URL")
        })?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https" | "socks5")
                && parsed.host_str().is_some()
                && parsed.path() == "/"
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "invalid egress proxy URL; use an HTTP(S) or SOCKS5 URL with no path, query, or fragment"
        );
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|_| anyhow::anyhow!("invalid egress proxy configuration"))?;
        builder = builder.proxy(proxy);
    }

    Ok(builder.build()?)
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
    let client = build_client(config.egress_proxy.as_deref())?;
    if config.egress_proxy.is_some() {
        info!("explicit upstream egress proxy configured");
    }
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

#[cfg(test)]
mod tests {
    use super::build_client;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn rejects_unsupported_or_ambiguous_proxy_urls() {
        assert!(build_client(Some("ftp://proxy.example:21")).is_err());
        assert!(build_client(Some("http://proxy.example:8080/path")).is_err());
        assert!(build_client(Some("not a URL")).is_err());
    }

    #[tokio::test]
    async fn routes_upstream_requests_through_the_explicit_proxy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]).into_owned();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            request
        });

        let client = build_client(Some(&format!("http://{address}"))).unwrap();
        let response = client
            .get("http://upstream.invalid/test")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "ok");
        let request = proxy.await.unwrap();
        assert!(
            request.starts_with("GET http://upstream.invalid/test HTTP/1.1"),
            "{request}"
        );
    }
}
