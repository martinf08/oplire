use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::{
    config::ProxyConfig,
    transform::{
        anthropic_stream_start, anthropic_to_opencode_request, opencode_response_to_anthropic,
        opencode_stream_to_anthropic,
    },
};

pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: reqwest::Client,
}

pub async fn handle_health() -> Response {
    (StatusCode::OK, "proxy process is healthy").into_response()
}

pub async fn handle_models(State(state): State<Arc<ProxyState>>) -> Response {
    let url = format!(
        "{}/v1/models",
        state.config.upstream_base_url.trim_end_matches('/')
    );
    let mut request = state
        .client
        .get(url)
        .header(header::ACCEPT, "application/json");
    if let Some(key) = &state.config.api_key {
        request = request.bearer_auth(key);
    }

    match request.send().await {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(models) => (StatusCode::OK, Json(transform_models(&models))).into_response(),
            Err(error) => error_response(
                StatusCode::BAD_GATEWAY,
                &format!("invalid models response: {error}"),
            ),
        },
        Ok(response) => forward_upstream_response(response).await,
        Err(error) => error_response(
            StatusCode::BAD_GATEWAY,
            &format!("upstream request failed: {error}"),
        ),
    }
}

pub async fn handle_model_detail(
    State(state): State<Arc<ProxyState>>,
    Path(model_id): Path<String>,
) -> Response {
    if model_id.is_empty()
        || !model_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.:".contains(character))
    {
        return error_response(StatusCode::BAD_REQUEST, "invalid model identifier");
    }
    let url = format!(
        "{}/v1/models/{}",
        state.config.upstream_base_url.trim_end_matches('/'),
        model_id
    );
    let mut request = state
        .client
        .get(url)
        .header(header::ACCEPT, "application/json");
    if let Some(key) = &state.config.api_key {
        request = request.bearer_auth(key);
    }

    match request.send().await {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(model) => {
                let id = model.get("id").and_then(Value::as_str).unwrap_or("");
                let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
                (
                    StatusCode::OK,
                    Json(json!({"id": id, "name": name, "display_name": name, "type": "model"})),
                )
                    .into_response()
            }
            Err(error) => error_response(
                StatusCode::BAD_GATEWAY,
                &format!("invalid model response: {error}"),
            ),
        },
        Ok(response) => forward_upstream_response(response).await,
        Err(error) => error_response(
            StatusCode::BAD_GATEWAY,
            &format!("upstream request failed: {error}"),
        ),
    }
}

pub async fn handle_messages(State(state): State<Arc<ProxyState>>, body: Bytes) -> Response {
    let request_body = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {error}"))
        }
    };
    if let Err(message) = validate_anthropic_request(&request_body) {
        return error_response(StatusCode::BAD_REQUEST, message);
    }
    let model = request_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let stream = request_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let opencode_body = anthropic_to_opencode_request(&request_body);
    let url = format!(
        "{}/v1/chat/completions",
        state.config.upstream_base_url.trim_end_matches('/')
    );
    let mut request = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&opencode_body);
    if let Some(key) = &state.config.api_key {
        request = request.bearer_auth(key);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream request failed: {error}"),
            )
        }
    };

    if !response.status().is_success() {
        // In particular, preserve HTTP 429 and Retry-After. Never rotate identity
        // or replay a request to evade a provider quota.
        return forward_upstream_response(response).await;
    }

    if stream {
        forward_streaming(response, &model)
    } else {
        match response.json::<Value>().await {
            Ok(payload) => (
                StatusCode::OK,
                Json(opencode_response_to_anthropic(&payload, &model)),
            )
                .into_response(),
            Err(error) => error_response(
                StatusCode::BAD_GATEWAY,
                &format!("invalid upstream JSON: {error}"),
            ),
        }
    }
}

fn forward_streaming(response: reqwest::Response, model: &str) -> Response {
    let model = model.to_string();
    let mut input = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut pending = Vec::new();
        let mut block_closed = false;
        let mut finished = false;
        yield Ok::<_, std::io::Error>(Bytes::from(anthropic_stream_start(&model)));

        while let Some(chunk) = input.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(std::io::Error::other(error));
                    return;
                }
            };
            pending.extend_from_slice(&chunk);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let mut raw_line: Vec<u8> = pending.drain(..=newline).collect();
                raw_line.pop();
                if raw_line.last() == Some(&b'\r') {
                    raw_line.pop();
                }
                let line = String::from_utf8_lossy(&raw_line);
                if let Some(transformed) = transform_stream_line(
                    &line,
                    &model,
                    &mut block_closed,
                    &mut finished,
                ) {
                    yield Ok(Bytes::from(transformed));
                }
            }
        }
        if !pending.is_empty() {
            let line = String::from_utf8_lossy(&pending);
            if let Some(transformed) = transform_stream_line(
                &line,
                &model,
                &mut block_closed,
                &mut finished,
            ) {
                yield Ok(Bytes::from(transformed));
            }
        }
        if !block_closed {
            yield Ok(Bytes::from("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}}\n\n"));
        }
        if !finished {
            yield Ok(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        }
    };

    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

fn validate_anthropic_request(body: &Value) -> Result<(), &'static str> {
    if !body
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| !model.is_empty())
    {
        return Err("a non-empty model is required");
    }
    if !body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|tokens| tokens > 0)
    {
        return Err("max_tokens must be a positive integer");
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("messages must be an array")?;
    if messages.is_empty() {
        return Err("messages must not be empty");
    }
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or("each message needs a role")?;
        if !matches!(role, "user" | "assistant") {
            return Err("only user and assistant message roles are supported");
        }
        validate_text_content(message.get("content").ok_or("each message needs content")?)?;
    }
    if let Some(system) = body.get("system") {
        validate_text_content(system)?;
    }
    match body.get("tools") {
        Some(Value::Array(tools)) if !tools.is_empty() => {
            return Err("tool use is not supported by this text-only bridge");
        }
        Some(Value::Array(_)) | None => {}
        Some(_) => return Err("tools must be an array"),
    }
    if body.get("tool_choice").is_some() {
        return Err("tool choice is not supported by this text-only bridge");
    }
    Ok(())
}

fn validate_text_content(content: &Value) -> Result<(), &'static str> {
    match content {
        Value::String(_) => Ok(()),
        Value::Array(blocks) => {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("text")
                    || block.get("text").and_then(Value::as_str).is_none()
                {
                    return Err("only text content blocks are supported");
                }
            }
            Ok(())
        }
        _ => Err("content must be text or an array of text blocks"),
    }
}

fn transform_stream_line(
    line: &str,
    model: &str,
    block_closed: &mut bool,
    finished: &mut bool,
) -> Option<String> {
    if line.strip_prefix("data:").map(str::trim) == Some("[DONE]") {
        let mut events = String::new();
        if !*block_closed {
            events.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}}\n\n");
            *block_closed = true;
        }
        events.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        *finished = true;
        return Some(events);
    }

    let transformed = opencode_stream_to_anthropic(line, model)?;
    if transformed.contains("event: content_block_stop") {
        *block_closed = true;
    }
    if transformed.contains("event: message_stop") {
        *finished = true;
    }
    Some(transformed)
}

async fn forward_upstream_response(upstream: reqwest::Response) -> Response {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let body = match upstream.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("reading upstream response failed: {error}"),
            )
        }
    };
    let mut response = Response::builder()
        .status(status)
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            error_response(StatusCode::BAD_GATEWAY, "building upstream response failed")
        });

    for name in [
        header::CONTENT_TYPE,
        header::RETRY_AFTER,
        header::WWW_AUTHENTICATE,
        header::HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = headers.get(&name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "type": "error",
            "error": {"type": "api_error", "message": message}
        })),
    )
        .into_response()
}

fn transform_models(upstream: &Value) -> Value {
    let data = upstream
        .get("data")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let id = model.get("id")?.as_str()?;
                    let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
                    let mut entry = json!({
                        "id": id,
                        "name": name,
                        "display_name": name,
                        "type": "model"
                    });
                    for field in ["context_window", "input_price", "output_price"] {
                        if let Some(value) = model.get(field) {
                            entry[field] = value.clone();
                        }
                    }
                    Some(entry)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            ProxyConfig::models_response()["data"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        });
    json!({"data": data, "has_more": false})
}

#[cfg(test)]
mod tests {
    use super::{handle_messages, transform_models, validate_anthropic_request};
    use crate::{config::ProxyConfig, proxy::handlers::ProxyState};
    use axum::{
        body::{to_bytes, Body},
        http::{
            header::{CONTENT_TYPE, RETRY_AFTER},
            Request, StatusCode,
        },
        routing::post,
        Router,
    };
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[test]
    fn model_catalogue_does_not_invent_pricing_or_context_limits() {
        let models = transform_models(&json!({"data": [{"id": "example"}]}));
        assert!(models["data"][0].get("input_price").is_none());
        assert!(models["data"][0].get("context_window").is_none());
        let models = transform_models(&json!({"data": [{"id": "example", "input_price": 1.5}]}));
        assert_eq!(models["data"][0]["input_price"], 1.5);
    }

    #[test]
    fn rejects_images_and_tool_calls_instead_of_dropping_them() {
        let image_request = json!({
            "model":"test",
            "max_tokens":10,
            "messages":[{"role":"user", "content":[{"type":"image", "source":{}}]}]
        });
        assert_eq!(
            validate_anthropic_request(&image_request),
            Err("only text content blocks are supported")
        );

        let tool_request = json!({
            "model":"test",
            "max_tokens":10,
            "messages":[{"role":"user", "content":"hello"}],
            "tools":[{"name":"tool"}]
        });
        assert_eq!(
            validate_anthropic_request(&tool_request),
            Err("tool use is not supported by this text-only bridge")
        );

        let tool_choice_request = json!({
            "model":"test",
            "max_tokens":10,
            "messages":[{"role":"user", "content":"hello"}],
            "tool_choice":{"type":"auto"}
        });
        assert_eq!(
            validate_anthropic_request(&tool_choice_request),
            Err("tool choice is not supported by this text-only bridge")
        );
    }

    #[tokio::test]
    async fn proxy_forwards_rate_limit_and_retry_after_without_replaying() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route_calls = Arc::clone(&calls);
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let calls = Arc::clone(&route_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(RETRY_AFTER, "45")],
                        r#"{"error":{"message":"quota exhausted"}}"#,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let state = Arc::new(ProxyState {
            config: ProxyConfig {
                upstream_base_url: format!("http://{address}"),
                ..ProxyConfig::default()
            },
            client: reqwest::Client::new(),
        });
        let app = Router::new()
            .route("/v1/messages", post(handle_messages))
            .with_state(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model":"test", "max_tokens":8, "messages":[{"role":"user","content":"hello"}]}).to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "45");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"error":{"message":"quota exhausted"}}"#);
        server.abort();
    }

    #[tokio::test]
    async fn proxy_emits_a_complete_anthropic_sse_lifecycle() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    [(CONTENT_TYPE, "text/event-stream")],
                    Body::from(concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let state = Arc::new(ProxyState {
            config: ProxyConfig {
                upstream_base_url: format!("http://{address}"),
                ..ProxyConfig::default()
            },
            client: reqwest::Client::new(),
        });
        let app = Router::new()
            .route("/v1/messages", post(handle_messages))
            .with_state(state);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "model":"test",
                    "max_tokens":8,
                    "stream":true,
                    "messages":[{"role":"user", "content":"hello"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"));
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("event: message_start\n"));
        assert!(body.contains("event: content_block_start\n"));
        assert!(body.contains("event: content_block_delta\n"));
        assert!(body.contains("event: content_block_stop\n"));
        assert!(body.contains("event: message_delta\n"));
        assert!(body.contains("event: message_stop\n"));
        assert_eq!(body.matches("event: message_stop\n").count(), 1);
        server.abort();
    }
}
