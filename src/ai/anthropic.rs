//! Anthropic Claude provider implementation.

use super::{CompletionOptions, CompletionResult, LlmProvider, Message, Role};
use futures::StreamExt;

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// LLM provider backed by the Anthropic Messages API.
#[derive(Debug)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
    /// Timeout in seconds for each HTTP request.
    timeout_secs: u64,
}

impl AnthropicProvider {
    /// Create a new `AnthropicProvider`.
    ///
    /// `base_url` defaults to `https://api.anthropic.com` when `None`.
    /// `timeout_secs` sets the HTTP request timeout; must be > 0.
    pub fn new(api_key: String, base_url: Option<String>, timeout_secs: u64) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".to_owned()),
            client,
            timeout_secs,
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

            let timeout_secs = self.timeout_secs;
            let resp = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        format!("AI request timed out after {timeout_secs}s")
                    } else {
                        format!("Anthropic API error: {e}")
                    }
                })?;

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

    fn complete_streaming(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        on_token: Box<dyn Fn(&str) + Send>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>,
    > {
        let messages = messages.to_vec();
        let options = options.clone();
        Box::pin(async move {
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
                "stream": true,
            });

            if let Some(sys) = system_msg {
                body["system"] = serde_json::Value::String(sys);
            }

            let timeout_secs = self.timeout_secs;
            let resp = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        format!("AI request timed out after {timeout_secs}s")
                    } else {
                        format!("Anthropic API error: {e}")
                    }
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(format!("Anthropic API {status}: {body_text}"));
            }

            let mut full_content = String::new();
            let mut input_tokens: u32 = 0;
            let mut output_tokens: u32 = 0;

            let mut stream = resp.bytes_stream();
            let mut buf = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
                buf.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines.
                while let Some(newline_pos) = buf.find('\n') {
                    let line = buf[..newline_pos].trim_end().to_owned();
                    buf = buf[newline_pos + 1..].to_owned();

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            continue;
                        }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                            // Content delta — stream the text chunk.
                            if json["type"] == "content_block_delta" {
                                if let Some(text) = json["delta"]["text"].as_str() {
                                    on_token(text);
                                    full_content.push_str(text);
                                }
                            }
                            // Final message — extract token usage.
                            if json["type"] == "message_delta" {
                                if let Some(u) = json["usage"]["output_tokens"].as_u64() {
                                    output_tokens = u32::try_from(u).unwrap_or(u32::MAX);
                                }
                            }
                            if json["type"] == "message_start" {
                                if let Some(u) = json["message"]["usage"]["input_tokens"].as_u64() {
                                    input_tokens = u32::try_from(u).unwrap_or(u32::MAX);
                                }
                            }
                        }
                    }
                }
            }

            Ok(CompletionResult {
                content: full_content,
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
        let p = AnthropicProvider::new("key".to_owned(), None, 30);
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn default_model() {
        let p = AnthropicProvider::new("key".to_owned(), None, 30);
        assert_eq!(p.default_model(), "claude-sonnet-4-6");
    }

    #[test]
    fn default_base_url() {
        let p = AnthropicProvider::new("key".to_owned(), None, 30);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn custom_base_url() {
        let p = AnthropicProvider::new(
            "key".to_owned(),
            Some("https://proxy.example.com".to_owned()),
            30,
        );
        assert_eq!(p.base_url, "https://proxy.example.com");
    }

    #[test]
    fn timeout_stored() {
        let p = AnthropicProvider::new("key".to_owned(), None, 60);
        assert_eq!(p.timeout_secs, 60);
    }
}
