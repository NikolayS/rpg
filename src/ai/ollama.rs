//! Ollama local model provider implementation.
//!
//! Connects to a locally-running Ollama instance at (by default)
//! `http://localhost:11434`.

use super::{CompletionOptions, CompletionResult, LlmProvider, Message};

// ---------------------------------------------------------------------------
// OllamaProvider
// ---------------------------------------------------------------------------

/// LLM provider backed by a local Ollama instance.
#[derive(Debug)]
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Create a new `OllamaProvider`.
    ///
    /// `base_url` should be the full URL of the Ollama server, e.g.
    /// `http://localhost:11434`.
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }
}

impl LlmProvider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn default_model(&self) -> &'static str {
        "llama3"
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

            // Ollama /api/chat uses the same roles as OpenAI.
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
                "messages": conv_messages,
                "stream": false,
            });

            let resp = self
                .client
                .post(format!("{}/api/chat", self.base_url))
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Ollama API error: {e}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(format!("Ollama API {status}: {body_text}"));
            }

            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Ollama response parse error: {e}"))?;

            let content = json["message"]["content"].as_str().unwrap_or("").to_owned();

            // Ollama reports token counts in eval_count / prompt_eval_count.
            let input_tokens =
                u32::try_from(json["prompt_eval_count"].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
            let output_tokens =
                u32::try_from(json["eval_count"].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);

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
        let p = OllamaProvider::new("http://localhost:11434".to_owned());
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn default_model() {
        let p = OllamaProvider::new("http://localhost:11434".to_owned());
        assert_eq!(p.default_model(), "llama3");
    }

    #[test]
    fn custom_url_stored() {
        let p = OllamaProvider::new("http://myhost:11434".to_owned());
        assert_eq!(p.base_url, "http://myhost:11434");
    }
}
