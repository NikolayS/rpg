//! AI subsystem: LLM providers, schema context, and AI commands.

pub mod anthropic;
pub mod context;
pub mod ollama;
pub mod openai;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A message in a conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Message {
    /// The role of the message author.
    pub role: Role,
    /// The text content of the message.
    pub content: String,
}

/// The role of a message author.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System instructions.
    System,
    /// User turn.
    User,
    /// Assistant turn (included for completeness; used in multi-turn
    /// conversation context).
    #[allow(dead_code)]
    Assistant,
}

/// Options for a completion request.
#[derive(Debug, Clone)]
pub struct CompletionOptions {
    /// Model identifier (empty string means use the provider's default).
    pub model: String,
    /// Maximum number of tokens to generate.
    pub max_tokens: u32,
    /// Sampling temperature (0.0 = deterministic).
    pub temperature: f32,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 4096,
            temperature: 0.0,
        }
    }
}

/// Result of a non-streaming completion request.
#[derive(Debug)]
pub struct CompletionResult {
    /// The generated text content.
    pub content: String,
    /// Number of tokens in the prompt.
    ///
    /// Reserved for future token-usage tracking (issue #75).
    #[allow(dead_code)]
    pub input_tokens: u32,
    /// Number of tokens in the completion.
    ///
    /// Reserved for future token-usage tracking (issue #75).
    #[allow(dead_code)]
    pub output_tokens: u32,
}

// ---------------------------------------------------------------------------
// LlmProvider trait
// ---------------------------------------------------------------------------

/// Trait for LLM providers.
///
/// Uses `Pin<Box<dyn Future>>` to avoid the `async_trait` crate dependency.
pub trait LlmProvider: Send + Sync + std::fmt::Debug {
    /// Human-readable name of this provider.
    ///
    /// Used for logging and diagnostics.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Default model for this provider.
    fn default_model(&self) -> &'static str;

    /// Send a completion request and return the result.
    fn complete(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>,
    >;
}

// ---------------------------------------------------------------------------
// Provider factory
// ---------------------------------------------------------------------------

/// Create an [`LlmProvider`] from configuration values.
///
/// # Errors
///
/// Returns an error string when the provider name is unknown or a required
/// credential is missing.
pub fn create_provider(
    provider_name: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Box<dyn LlmProvider>, String> {
    match provider_name {
        "anthropic" | "claude" => {
            let key = api_key.ok_or("ANTHROPIC_API_KEY not set")?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                key.to_owned(),
                base_url.map(str::to_owned),
            )))
        }
        "openai" => {
            let key = api_key.ok_or("OPENAI_API_KEY not set")?;
            Ok(Box::new(openai::OpenAiProvider::new(
                key.to_owned(),
                base_url.map(str::to_owned),
            )))
        }
        "ollama" => {
            let url = base_url.unwrap_or("http://localhost:11434");
            Ok(Box::new(ollama::OllamaProvider::new(url.to_owned())))
        }
        other => Err(format!("unknown AI provider: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_options_default() {
        let opts = CompletionOptions::default();
        assert!(opts.model.is_empty());
        assert_eq!(opts.max_tokens, 4096);
        assert!(opts.temperature.abs() < f32::EPSILON);
    }

    #[test]
    fn message_construction() {
        let msg = Message {
            role: Role::User,
            content: "hello".to_owned(),
        };
        assert_eq!(msg.content, "hello");
        assert!(matches!(msg.role, Role::User));
    }

    #[test]
    fn role_serialize() {
        let system = serde_json::to_string(&Role::System).unwrap();
        let user = serde_json::to_string(&Role::User).unwrap();
        let assistant = serde_json::to_string(&Role::Assistant).unwrap();
        assert_eq!(system, "\"system\"");
        assert_eq!(user, "\"user\"");
        assert_eq!(assistant, "\"assistant\"");
    }

    #[test]
    fn create_provider_unknown() {
        let err = create_provider("unknown_xyz", None, None).unwrap_err();
        assert!(err.contains("unknown AI provider"));
    }

    #[test]
    fn create_provider_anthropic_missing_key() {
        let err = create_provider("anthropic", None, None).unwrap_err();
        assert!(err.contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn create_provider_openai_missing_key() {
        let err = create_provider("openai", None, None).unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn create_provider_anthropic_with_key() {
        let p =
            create_provider("anthropic", Some("sk-test"), None).expect("should succeed with key");
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn create_provider_claude_alias() {
        let p = create_provider("claude", Some("sk-test"), None).expect("claude alias should work");
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn create_provider_openai_with_key() {
        let p = create_provider("openai", Some("sk-test"), None).expect("should succeed with key");
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn create_provider_ollama_no_key_needed() {
        let p = create_provider("ollama", None, None).expect("ollama needs no key");
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn create_provider_ollama_custom_url() {
        let p = create_provider("ollama", None, Some("http://myhost:11434"))
            .expect("ollama custom url");
        assert_eq!(p.name(), "ollama");
    }
}
