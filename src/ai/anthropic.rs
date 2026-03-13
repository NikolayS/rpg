//! Anthropic Claude provider implementation.

use super::{CompletionOptions, CompletionResult, LlmProvider, Message, Role};

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// LLM provider backed by the Anthropic Messages API.
#[derive(Debug)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new `AnthropicProvider`.
    ///
    /// `base_url` defaults to `https://api.anthropic.com` when `None`.
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".to_owned()),
            client: reqwest::Client::new(),
        }
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn default_model(&self) -> &'static str {
        "claude-sonnet-4-6"
    }

    fn complete(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>,
    > {
        let messages = messages.to_vec();
        let options = options.clone();
        Box::pin(async move {
            // Separate system message from the conversation turns.
            let system_msg = messages
                .iter()
                .find(|m| matches!(m.role, Role::System))
                .map(|m| m.content.clone());

            let conv_messages: Vec<serde_json::Value> = messages
                .iter()
                .filter(|m| !matches!(m.role, Role::System))
                .map(|m| {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                    })
                })
                .collect();

            let model = if options.model.is_empty() {
                self.default_model().to_owned()
            } else {
                options.model.clone()
            };

            let mut body = serde_json::json!({
                "model": model,
                "max_tokens": options.max_tokens,
                "messages": conv_messages,
            });

            if let Some(sys) = system_msg {
                body["system"] = serde_json::Value::String(sys);
            }

            let resp = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Anthropic API error: {e}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(format!("Anthropic API {status}: {body_text}"));
            }

            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Anthropic response parse error: {e}"))?;

            let content = json["content"][0]["text"].as_str().unwrap_or("").to_owned();

            let input_tokens = u32::try_from(json["usage"]["input_tokens"].as_u64().unwrap_or(0))
                .unwrap_or(u32::MAX);
            let output_tokens = u32::try_from(json["usage"]["output_tokens"].as_u64().unwrap_or(0))
                .unwrap_or(u32::MAX);

            Ok(CompletionResult {
                content,
                input_tokens,
                output_tokens,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let p = AnthropicProvider::new("key".to_owned(), None);
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn default_model() {
        let p = AnthropicProvider::new("key".to_owned(), None);
        assert_eq!(p.default_model(), "claude-sonnet-4-6");
    }

    #[test]
    fn default_base_url() {
        let p = AnthropicProvider::new("key".to_owned(), None);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn custom_base_url() {
        let p = AnthropicProvider::new(
            "key".to_owned(),
            Some("https://proxy.example.com".to_owned()),
        );
        assert_eq!(p.base_url, "https://proxy.example.com");
    }
}
