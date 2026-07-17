//! `ModelProvider` — the uniform seam for talking to a model provider, and the
//! registry that routes a `"provider/model"` string to one.
//!
//! Slice 2 of rosary-c79331. This lands the *shape* both the AI SDK and the
//! Rust `genai` crate converged on — a small provider trait plus a registry
//! that resolves a two-part model reference — and ties in the [`Credential`]
//! substrate from slice 1. The async call surface (`generate`/`stream`, the
//! wire-format adapters, and the middleware wrapper) lands in a later slice;
//! this slice is the routing + credential-carrying core, which is pure logic
//! and fully testable without a network.
//!
//! Design notes carried from the prior-art sweep:
//! - Route by a **strict two-part** `provider/model` split — never overload the
//!   string with special-cases (a LiteLLM wart). Variant selection lives in the
//!   provider, not the string.
//! - Distinguish **unknown provider** (typo / unsupported) from **bad format**
//!   (no `/`) from **not registered** (known but not wired) — three different
//!   errors, each actionable, both external lenses flagged this.
//! - The registry is the natural home for the middleware wrapper (feedback
//!   contract / observation logging around every call) once the call surface
//!   exists.
#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::credential::Credential;

/// The closed set of providers rosary speaks to. Closed (not open/`dyn`-plugin)
/// because rosary's provider set is known — genai's enum-dispatch fit, per the
/// sweep — but each provider is still a trait object so the registry can hold a
/// heterogeneous map and wrap them uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Anthropic,
    OpenAI,
    Google,
    /// cloister `/vault/proxy` — a remote, OpenAI-compatible provider behind the
    /// same seam (the AI SDK "gateway" pattern). Credentials stay in the vault.
    VaultProxy,
}

impl ProviderKind {
    /// Canonical prefix for this provider.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAI => "openai",
            ProviderKind::Google => "google",
            ProviderKind::VaultProxy => "vault",
        }
    }

    /// Parse a provider prefix, accepting the common aliases.
    pub fn parse(prefix: &str) -> Option<Self> {
        match prefix {
            "anthropic" | "claude" => Some(ProviderKind::Anthropic),
            "openai" | "codex" => Some(ProviderKind::OpenAI),
            "google" | "gemini" => Some(ProviderKind::Google),
            "vault" | "vault-proxy" => Some(ProviderKind::VaultProxy),
            _ => None,
        }
    }

    /// Every known prefix, sorted — for actionable error messages.
    fn known() -> &'static [&'static str] {
        &["anthropic", "google", "openai", "vault"]
    }
}

/// A chat role in a normalized request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One message in a normalized chat request.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// A normalized chat request — the one shape every provider adapter maps to its
/// native wire format. `provider_options` is the namespaced pass-through bag
/// (AI SDK pattern): keyed by provider prefix, only the matching adapter reads
/// its entry, so provider-specific params never touch the common shape.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub provider_options: HashMap<String, serde_json::Value>,
}

/// Token accounting from a response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A non-fatal degradation — an unsupported knob dropped, etc. Providers push
/// these instead of erroring (AI SDK pattern: degrade honestly, don't fail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub feature: String,
    pub message: String,
}

/// A normalized chat response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub finish_reason: String,
    pub usage: Usage,
    pub warnings: Vec<Warning>,
}

/// A model provider: knows its kind, carries its [`Credential`], and makes
/// calls. `generate` is object-safe via `async_trait` so the [`Registry`] can
/// hold `Box<dyn ModelProvider>`. Streaming (`stream`) + the middleware wrapper
/// land in the next slice.
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn credential(&self) -> &Credential;
    async fn generate(&self, req: &ChatRequest) -> Result<ChatResponse>;
}

/// A resolved provider + the bare model id (prefix stripped).
pub struct Resolved<'a> {
    pub provider: &'a dyn ModelProvider,
    pub model_id: &'a str,
}

impl std::fmt::Debug for Resolved<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("provider", &self.provider.kind())
            .field("model_id", &self.model_id)
            .finish()
    }
}

/// Routes `"provider/model"` references to registered providers.
#[derive(Default)]
pub struct Registry {
    providers: HashMap<ProviderKind, Box<dyn ModelProvider>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider under its own [`ProviderKind`]. Later registrations
    /// for the same kind replace earlier ones.
    pub fn register(&mut self, provider: Box<dyn ModelProvider>) -> &mut Self {
        self.providers.insert(provider.kind(), provider);
        self
    }

    /// Resolve `"provider/model"` to a provider + model id. Strict two-part
    /// split; distinct errors for bad-format vs unknown-provider vs
    /// not-registered.
    pub fn resolve<'a>(&'a self, model_ref: &'a str) -> Result<Resolved<'a>> {
        let Some((prefix, model_id)) = model_ref.split_once('/') else {
            bail!(
                "invalid model ref {model_ref:?}: expected 'provider/model' \
                 (e.g. 'anthropic/claude-sonnet-4-6')"
            );
        };
        if model_id.is_empty() || prefix.is_empty() {
            bail!(
                "invalid model ref {model_ref:?}: both provider and model must be \
                 non-empty (e.g. 'anthropic/claude-sonnet-4-6')"
            );
        }
        let Some(kind) = ProviderKind::parse(prefix) else {
            bail!(
                "unknown provider {prefix:?} in {model_ref:?}; known providers: {}",
                ProviderKind::known().join(", ")
            );
        };
        let Some(provider) = self.providers.get(&kind) else {
            bail!(
                "provider {prefix:?} is not registered; registered: {}",
                self.registered()
            );
        };
        Ok(Resolved {
            provider: provider.as_ref(),
            model_id,
        })
    }

    /// Sorted list of registered provider prefixes (for error messages).
    fn registered(&self) -> String {
        let mut names: Vec<&str> = self.providers.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        if names.is_empty() {
            "(none)".to_string()
        } else {
            names.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        kind: ProviderKind,
        cred: Credential,
    }

    #[async_trait::async_trait]
    impl ModelProvider for FakeProvider {
        fn kind(&self) -> ProviderKind {
            self.kind
        }
        fn credential(&self) -> &Credential {
            &self.cred
        }
        async fn generate(&self, _req: &ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                content: "fake".into(),
                finish_reason: "stop".into(),
                usage: Usage::default(),
                warnings: vec![],
            })
        }
    }

    fn registry_with(kinds: &[ProviderKind]) -> Registry {
        let mut r = Registry::new();
        for &kind in kinds {
            r.register(Box::new(FakeProvider {
                kind,
                cred: Credential::api_key("k"),
            }));
        }
        r
    }

    #[test]
    fn resolves_provider_and_strips_prefix() {
        let r = registry_with(&[ProviderKind::Anthropic]);
        let got = r.resolve("anthropic/claude-sonnet-4-6").unwrap();
        assert_eq!(got.provider.kind(), ProviderKind::Anthropic);
        assert_eq!(got.model_id, "claude-sonnet-4-6");
    }

    #[test]
    fn aliases_map_to_canonical_kind() {
        let r = registry_with(&[ProviderKind::Google]);
        // "gemini/" is an alias for the Google provider.
        assert_eq!(
            r.resolve("gemini/gemini-2.0").unwrap().provider.kind(),
            ProviderKind::Google
        );
    }

    #[test]
    fn bad_format_is_its_own_error() {
        let r = registry_with(&[ProviderKind::Anthropic]);
        let err = r.resolve("no-slash-here").unwrap_err().to_string();
        assert!(err.contains("provider/model"), "got: {err}");
    }

    #[test]
    fn unknown_provider_lists_known() {
        let r = registry_with(&[ProviderKind::Anthropic]);
        let err = r.resolve("bogus/model").unwrap_err().to_string();
        assert!(err.contains("unknown provider"), "got: {err}");
        assert!(
            err.contains("anthropic"),
            "should list known providers: {err}"
        );
    }

    #[test]
    fn known_but_unregistered_is_distinct_from_unknown() {
        // openai is a known kind, but only anthropic is registered here.
        let r = registry_with(&[ProviderKind::Anthropic]);
        let err = r.resolve("openai/gpt-5").unwrap_err().to_string();
        assert!(err.contains("not registered"), "got: {err}");
        assert!(
            err.contains("anthropic"),
            "should list what IS registered: {err}"
        );
    }

    #[test]
    fn empty_model_id_rejected() {
        let r = registry_with(&[ProviderKind::Anthropic]);
        assert!(r.resolve("anthropic/").is_err());
    }

    #[test]
    fn resolved_provider_carries_its_credential() {
        // Ties slice 2 to slice 1: the provider owns a Credential.
        let mut r = Registry::new();
        r.register(Box::new(FakeProvider {
            kind: ProviderKind::Google,
            cred: Credential::gemini("cid", "csecret", "rtoken"),
        }));
        let got = r.resolve("google/gemini-2.0").unwrap();
        match got.provider.credential() {
            Credential::OAuth(o) => assert!(o.client_secret.is_some()),
            _ => panic!("gemini provider should carry an OAuth credential"),
        }
    }

    // ── slice 3: the async call surface integrates through the trait object ──
    // (adapter transform tests live with the adapter in src/openai_compat.rs)

    #[tokio::test]
    async fn generate_flows_through_the_trait() {
        let p: Box<dyn ModelProvider> = Box::new(FakeProvider {
            kind: ProviderKind::OpenAI,
            cred: Credential::api_key("k"),
        });
        let req = ChatRequest {
            model_id: "gpt-5".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "hi".into(),
            }],
            ..Default::default()
        };
        let resp = p.generate(&req).await.unwrap();
        assert_eq!(resp.content, "fake");
    }
}
