use serde_json::{json, Value};

pub fn anthropic_to_opencode_request(body: &Value) -> Value {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let max_tokens = body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut messages = Vec::new();

    if let Some(system) = body.get("system") {
        let text = content_text(system);
        if !text.is_empty() {
            messages.push(json!({"role":"system", "content":text}));
        }
    }

    if let Some(input) = body.get("messages").and_then(Value::as_array) {
        for message in input {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            messages.push(json!({
                "role": if role == "assistant" { "assistant" } else { "user" },
                "content": content_text(message.get("content").unwrap_or(&Value::Null))
            }));
        }
    }

    let mut request = json!({
        "model": model,
        "messages": messages,
        "stream": stream,
        "max_tokens": max_tokens
    });
    if let Some(object) = request.as_object_mut() {
        for key in ["temperature", "top_p"] {
            if let Some(value) = body.get(key) {
                object.insert(key.to_string(), value.clone());
            }
        }
        if let Some(stop) = body.get("stop_sequences") {
            object.insert("stop".to_string(), stop.clone());
        }
    }
    request
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn opencode_response_to_anthropic(body: &Value, model: &str) -> Value {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|v| v.first());
    let content = choice
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stop_reason = match choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
    {
        Some("length") => "max_tokens",
        _ => "end_turn",
    };
    let source_usage = body.get("usage").unwrap_or(&Value::Null);
    let usage = json!({
        "input_tokens": source_usage
            .get("prompt_tokens")
            .or_else(|| source_usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "output_tokens": source_usage
            .get("completion_tokens")
            .or_else(|| source_usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });

    json!({
        "id": format!("msg_{}", short_id()),
        "type": "message",
        "role": "assistant",
        "content": [{"type":"text", "text":content}],
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage
    })
}

pub fn opencode_stream_to_anthropic(line: &str, _model: &str) -> Option<String> {
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    let data = line.strip_prefix("data:")?.trim_start();
    if data == "[DONE]" {
        return Some("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
    }
    let value: Value = serde_json::from_str(data).ok()?;
    let choice = value.get("choices")?.as_array()?.first()?;
    let mut events = String::new();
    if let Some(text) = choice
        .get("delta")
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
    {
        if !text.is_empty() {
            let payload = json!({
                "type":"content_block_delta",
                "index":0,
                "delta":{"type":"text_delta", "text":text}
            });
            events.push_str(&format!(
                "event: content_block_delta\ndata: {}\n\n",
                payload
            ));
        }
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        let stop = if reason == "length" {
            "max_tokens"
        } else {
            "end_turn"
        };
        let block_stop = json!({"type":"content_block_stop", "index":0});
        let delta = json!({
            "type":"message_delta",
            "delta":{"stop_reason":stop, "stop_sequence":null}
        });
        events.push_str(&format!(
            "event: content_block_stop\ndata: {}\n\nevent: message_delta\ndata: {}\n\n",
            block_stop, delta
        ));
    }
    (!events.is_empty()).then_some(events)
}

pub fn anthropic_stream_start(model: &str) -> String {
    let message = json!({
        "id": format!("msg_{}", short_id()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [],
        "stop_reason": null,
        "stop_sequence": null,
        "usage": {"input_tokens": 0, "output_tokens": 0}
    });
    let block = json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "text", "text": ""}
    });
    format!(
        "event: message_start\ndata: {}\n\nevent: content_block_start\ndata: {}\n\n",
        json!({"type":"message_start", "message":message}),
        block
    )
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:x}", elapsed.as_secs(), elapsed.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::{
        anthropic_stream_start, anthropic_to_opencode_request, opencode_response_to_anthropic,
        opencode_stream_to_anthropic,
    };
    use serde_json::json;

    #[test]
    fn maps_anthropic_text_messages_and_generation_options() {
        let mapped = anthropic_to_opencode_request(&json!({
            "model":"test-model",
            "system":"be concise",
            "max_tokens":50,
            "temperature":0.2,
            "messages":[{"role":"user", "content":[{"type":"text", "text":"hello"}]}]
        }));
        assert_eq!(mapped["model"], "test-model");
        assert_eq!(mapped["messages"][0]["role"], "system");
        assert_eq!(mapped["messages"][1]["content"], "hello");
        assert_eq!(mapped["temperature"], 0.2);
    }

    #[test]
    fn maps_non_streaming_response() {
        let mapped = opencode_response_to_anthropic(
            &json!({"choices":[{"message":{"content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4}}),
            "test-model",
        );
        assert_eq!(mapped["content"][0]["text"], "hello");
        assert_eq!(mapped["stop_reason"], "end_turn");
        assert_eq!(mapped["usage"]["input_tokens"], 3);
        assert_eq!(mapped["usage"]["output_tokens"], 4);
    }

    #[test]
    fn emits_anthropic_sse_lifecycle_and_safe_json_deltas() {
        let start = anthropic_stream_start("test-model");
        assert!(start.contains("event: message_start\n"));
        assert!(start.contains("event: content_block_start\n"));

        let delta = opencode_stream_to_anthropic(
            r#"data: {"choices":[{"delta":{"content":"hello\n\""}}]}"#,
            "test-model",
        )
        .unwrap();
        assert!(delta.contains("event: content_block_delta\n"));
        assert!(delta.contains(r#""text":"hello\n\""#));

        let finish = opencode_stream_to_anthropic(
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "test-model",
        )
        .unwrap();
        assert!(finish.contains("event: content_block_stop\n"));
        assert!(finish.contains("event: message_delta\n"));
    }

    #[test]
    fn maps_anthropic_stop_sequences_to_openai_stop() {
        let mapped = anthropic_to_opencode_request(&json!({
            "model":"test-model",
            "max_tokens":20,
            "stop_sequences":["END"],
            "messages":[{"role":"user", "content":"hello"}]
        }));
        assert_eq!(mapped["stop"], json!(["END"]));
    }
}
