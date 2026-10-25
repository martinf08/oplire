use anyhow::{bail, Context, Result};
use std::time::Duration;

/// Check a service endpoint with bounded retries for transient failures only.
/// HTTP 429 is returned immediately; the caller must respect Retry-After.
pub async fn check_url(url: &str, attempts: u8) -> Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .context("building health-check client")?;
    let attempts = attempts.max(1);
    let mut last_error = String::new();

    for attempt in 1..=attempts {
        match client.get(url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) if response.status().as_u16() == 429 => {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("not provided");
                bail!("HTTP 429 from {url}; no retry/reset attempted (Retry-After: {retry_after})");
            }
            Ok(response) if response.status().is_client_error() => {
                bail!("health check {url} returned {}", response.status());
            }
            Ok(response) if response.status().is_server_error() => {
                last_error = format!("health check {url} returned {}", response.status());
            }
            Ok(response) => {
                bail!("health check {url} returned {}", response.status());
            }
            Err(error) => last_error = format!("health check {url} failed: {error}"),
        }

        if attempt < attempts {
            let delay_ms = 250_u64.saturating_mul(1_u64 << u32::from(attempt - 1).min(5));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    bail!("health check failed after {attempts} bounded attempt(s): {last_error}")
}

#[cfg(test)]
mod tests {
    use super::check_url;
    use axum::{
        http::{header::RETRY_AFTER, StatusCode},
        routing::get,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn rate_limit_is_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_route = Arc::clone(&calls);
        let app = Router::new().route(
            "/health",
            get(move || {
                let calls = Arc::clone(&calls_for_route);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(RETRY_AFTER, "30")],
                        "quota",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let error = check_url(&format!("http://{address}/health"), 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("HTTP 429"));
        assert!(error.to_string().contains("Retry-After: 30"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn retries_a_transient_server_error_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_route = Arc::clone(&calls);
        let app = Router::new().route(
            "/health",
            get(move || {
                let calls = Arc::clone(&calls_for_route);
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        check_url(&format!("http://{address}/health"), 3)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn does_not_retry_a_non_server_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_route = Arc::clone(&calls);
        let app = Router::new().route(
            "/health",
            get(move || {
                let calls = Arc::clone(&calls_for_route);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NOT_MODIFIED
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let error = check_url(&format!("http://{address}/health"), 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("304 Not Modified"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        server.abort();
    }
}
