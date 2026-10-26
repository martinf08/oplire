use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub listen_addr: String,
    pub upstream_base_url: String,
    pub api_key: Option<String>,
    /// Explicit proxy used only for Oplire's outbound upstream requests.
    pub egress_proxy: Option<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8080".to_string(),
            upstream_base_url: "http://127.0.0.1:3000".to_string(),
            api_key: None,
            egress_proxy: None,
        }
    }
}

impl ProxyConfig {
    pub fn free_models() -> Vec<(&'static str, &'static str)> {
        vec![
            ("glm-4.7-free", "GLM 4.7 Free"),
            ("minimax-m2.1-free", "MiniMax M2.1 Free"),
            ("kimi-k2.5-free", "Kimi K2.5 Free"),
            ("qwen-2.5-72b-free", "Qwen 2.5 72B Free"),
            ("llama-3.3-70b-free", "Llama 3.3 70B Free"),
        ]
    }

    pub fn models_response() -> Value {
        let data: Vec<Value> = Self::free_models()
            .into_iter()
            .map(|(id, name)| {
                json!({
                    "id": id,
                    "name": name,
                    "type": "model",
                    "display_name": name
                })
            })
            .collect();
        json!({"data": data, "has_more": false})
    }
}
