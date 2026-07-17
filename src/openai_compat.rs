//! OpenAI-compatible [`ModelProvider`] adapter.
//!
//! Slice 3 of rosary-c79331. One adapter speaking the OpenAI Chat Completions
//! wire format serves OpenAI itself *and* cloister's `/vault/proxy` (same wire,
//! different `base_url` + credential) — the "gateway is just a provider behind
//! the same seam" point made real. The request/response translation
//! (`build_body`/`parse_response`) is pure and unit-tested without a network;
//! `generate` is the thin glue: mint a bearer, POST, parse.
//!
//! `new()` is unused until a consumer (enrichment / the registry wiring) lands,
//! so the constructor is `dead_code`-allowed for now.
#![allow(dead_code)]

use anyhow::{Context, Result};

use crate::credential::{Credential, TokenMinter};
use crate::model_provider::{
    ChatRequest, ChatResponse, ModelProvider, ProviderKind, Usage, Warning,
};

/// A provider speaking the OpenAI Chat Completions wire format. Generic over the
/// [`TokenMinter`] (RPITIT isn't object-safe) but boxes into
/// `Box<dyn ModelProvider>` once monomorphized.
pub struct OpenAiCompatProvider<M: TokenMinter> {
    kind: ProviderKind,
    /// e.g. `https://api.openai.com/v1` or the vault-proxy base.
    base_url: String,
    credential: Credential,
    minter: M,
    http: reqwest::Client,
}

impl<M: TokenMinter> OpenAiCompatProvider<M> {
    pub fn new(
        kind: ProviderKind,
        base_url: impl Into<String>,
        credential: Credential,
        minter: M,
        http: reqwest::Client,
    ) -> Self {
        Self {
            kind,
            base_url: base_url.into(),
            credential,
            minter,
            http,
        }
    }

    /// Map the normalized request to the OpenAI chat-completions body. Pure —
    /// unit-tested without a network.
    fn build_body(kind: ProviderKind, req: &ChatRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role.wire(), "content": m.content }))
            .collect();
        let mut body = serde_json::json!({ "model": req.model_id, "messages": messages });
        let obj = body.as_object_mut().expect("json object");
        if let Some(mt) = req.max_tokens {
            obj.insert("max_tokens".into(), serde_json::json!(mt));
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".into(), serde_json::json!(t));
        }
        // Merge this provider's namespaced options bag (if any) verbatim.
        if let Some(serde_json::Value::Object(extra)) = req.provider_options.get(kind.as_str()) {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        body
    }

    /// Parse an OpenAI chat-completions response into the normalized shape.
    /// Pure — unit-tested. Emits a [`Warning`] rather than erroring when
    /// optional fields are absent.
    fn parse_response(v: &serde_json::Value) -> Result<ChatResponse> {
        let choice = v
            .get("choices")
            .and_then(|c| c.get(0))
            .context("response had no choices[0]")?;
        let content = choice
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("choices[0].message.content missing")?
            .to_string();
        let mut warnings = Vec::new();
        let finish_reason = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .unwrap_or_else(|| {
                warnings.push(Warning {
                    feature: "finish_reason".into(),
                    message: "response omitted finish_reason".into(),
                });
                "unknown"
            })
            .to_string();
        let usage = v.get("usage");
        let usage = Usage {
            input_tokens: usage
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as u32,
            output_tokens: usage
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as u32,
        };
        Ok(ChatResponse {
            content,
            finish_reason,
            usage,
            warnings,
        })
    }
}

#[async_trait::async_trait]
impl<M: TokenMinter> ModelProvider for OpenAiCompatProvider<M> {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn credential(&self) -> &Credential {
        &self.credential
    }
    async fn generate(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let bearer = self
            .credential
            .bearer(&self.minter, chrono::Utc::now())
            .await?;
        let body = Self::build_body(self.kind, req);
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(bearer.expose())
            .json(&body)
            .send()
            .await
            .context("posting chat/completions")?
            .error_for_status()
            .context("chat/completions returned an error status")?;
        let v: serde_json::Value = resp.json().await.context("parsing chat/completions")?;
        Self::parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::HttpMinter;
    use crate::model_provider::{ChatMessage, Role};

    type OaProvider = OpenAiCompatProvider<HttpMinter>;

    fn sample_request() -> ChatRequest {
        ChatRequest {
            model_id: "gpt-5".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "hi".into(),
            }],
            max_tokens: Some(256),
            temperature: Some(0.2),
            ..Default::default()
        }
    }

    #[test]
    fn build_body_maps_common_params_to_wire() {
        let body = OaProvider::build_body(ProviderKind::OpenAI, &sample_request());
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn build_body_merges_namespaced_provider_options() {
        let mut req = sample_request();
        req.provider_options.insert(
            "openai".into(),
            serde_json::json!({ "reasoning_effort": "high" }),
        );
        // An option namespaced to a DIFFERENT provider must not leak in.
        req.provider_options
            .insert("anthropic".into(), serde_json::json!({ "thinking": true }));
        let body = OaProvider::build_body(ProviderKind::OpenAI, &req);
        assert_eq!(body["reasoning_effort"], "high");
        assert!(
            body.get("thinking").is_none(),
            "cross-provider option leaked"
        );
    }

    #[test]
    fn parse_response_extracts_content_finish_usage() {
        let v = serde_json::json!({
            "choices": [{ "message": { "content": "hello" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 7 }
        });
        let resp = OaProvider::parse_response(&v).unwrap();
        assert_eq!(resp.content, "hello");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: 12,
                output_tokens: 7
            }
        );
        assert!(resp.warnings.is_empty());
    }

    #[test]
    fn parse_response_warns_instead_of_erroring_on_missing_finish() {
        let v = serde_json::json!({
            "choices": [{ "message": { "content": "hi" } }]  // no finish_reason, no usage
        });
        let resp = OaProvider::parse_response(&v).unwrap();
        assert_eq!(resp.finish_reason, "unknown");
        assert_eq!(resp.usage, Usage::default());
        assert_eq!(resp.warnings.len(), 1);
        assert_eq!(resp.warnings[0].feature, "finish_reason");
    }
}
