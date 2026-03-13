//! `OpenAI`-compatible provider implementation.
//!
//! Works with the official `OpenAI` API and any compatible endpoint (e.g.
//! Azure `OpenAI`, local `vLLM`, `LM Studio`, etc.).

use super::{CompletionOptions, CompletionResult, LlmProvider, Message};

// ---------------------------------------------------------------------------
// OpenAiProvider
// ---------------------------------------------------------------------------

/// LLM provider backed by the `OpenAI` Chat Completions API.
#[derive(Debug)]
pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a new `OpenAiProvider`.
    ///
    /// `base_url` defaults to `https://api.openai.com` when `None`.
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_owned()),
            client: reqwest::Client::new(),
        }
    }
}

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn default_model(&self) -> &'static str {
        "gpt-4o"
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
            let model = if options.model.is_empty() {
                self.default_model().to_owned()
            } else {
                options.model.clone()
            };

            // OpenAI messages format: all roles including system go in the
            // messages array.
            let conv_messages: Vec<serde_json::Value> = messages
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                    })
                })
                .collect();

            let body = serde_json::json!({
                "model": model,
                "max_tokens": options.max_tokens,
                "temperature": options.temperature,
                "messages": conv_messages,
            });

            let resp = self
                .client
                .post(format!("{}/v1/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("OpenAI API error: {e}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(format!("OpenAI API {status}: {body_text}"));
            }

            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("OpenAI response parse error: {e}"))?;

            let content = json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_owned();

            let input_tokens = u32::try_from(json["usage"]["prompt_tokens"].as_u64().unwrap_or(0))
                .unwrap_or(u32::MAX);
            let output_tokens =
                u32::try_from(json["usage"]["completion_tokens"].as_u64().unwrap_or(0))
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
        let p = OpenAiProvider::new("key".to_owned(), None);
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn default_model() {
        let p = OpenAiProvider::new("key".to_owned(), None);
        assert_eq!(p.default_model(), "gpt-4o");
    }

    #[test]
    fn default_base_url() {
        let p = OpenAiProvider::new("key".to_owned(), None);
        assert_eq!(p.base_url, "https://api.openai.com");
    }

    #[test]
    fn custom_base_url() {
        let p = OpenAiProvider::new(
            "key".to_owned(),
            Some("https://my-azure-endpoint.openai.azure.com".to_owned()),
        );
        assert_eq!(p.base_url, "https://my-azure-endpoint.openai.azure.com");
    }
}
