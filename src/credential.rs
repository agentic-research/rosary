//! Per-provider credential bundle behind a uniform `bearer()` seam.
//!
//! Assimilates the pattern the AI SDK / LiteLLM / genai sweep converged on
//! (design: rosary-c79331) and turns the vault-custody RCA (rosary-470270) into
//! a type. One [`Credential`] enum whose variants model the *real* per-provider
//! shapes the RCA measured:
//!
//! - **codex** — OAuth with a *public* `client_id` and no secret (refresh works
//!   from a bare shell): `OAuth { client_secret: None, .. }`.
//! - **gemini** — OAuth needing `client_id` + `client_secret` + `refresh_token`
//!   (heavier custody surface): `OAuth { client_secret: Some(_), .. }`.
//! - **claude** — token held elsewhere, used directly as a key/bearer:
//!   [`Credential::ApiKey`]. Header shape (x-api-key vs `Authorization: Bearer`)
//!   is routed by token prefix at the call site (rosary-1be3b8), not here.
//!
//! [`Credential::bearer`] collapses all three to one call site and is where
//! refresh-if-expired + caching lives (mirrors LiteLLM's `_ensure_access_token`:
//! the minted short-lived bearer is cached separately from the long-lived
//! credential). Refreshing is injected via [`TokenMinter`] so the cache/expiry
//! logic is testable without a live token endpoint; production uses
//! [`HttpMinter`].
//!
//! This is the first slice of the ModelProvider work (rosary-c79331): the
//! `Credential` substrate. Its constructors + [`HttpMinter`] are consumed by the
//! provider-registry slice that lands next, so the public surface is
//! `dead_code`-allowed until that consumer merges.
#![allow(dead_code)]

use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

/// OpenAI/ChatGPT OAuth token endpoint (codex). RCA-confirmed custody-able.
const CODEX_TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// Google OAuth token endpoint (gemini). Refresh needs client_id + secret too.
const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Treat a token as expired this many seconds early so an in-flight request
/// never races the expiry boundary.
const EXPIRY_SKEW_SECS: i64 = 60;

/// A secret string that never prints its contents.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Expose the raw secret. Call sites must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// A minted short-lived bearer plus its absolute expiry.
#[derive(Clone, Debug)]
struct CachedToken {
    access: Secret,
    expires_at: DateTime<Utc>,
}

/// Inputs a token endpoint needs for a `refresh_token` grant.
#[derive(Debug, Clone)]
pub struct RefreshRequest<'a> {
    pub token_endpoint: &'a str,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub refresh_token: &'a str,
}

/// What a token endpoint returns on a successful refresh.
#[derive(Debug, Clone)]
pub struct MintedToken {
    pub access_token: Secret,
    pub expires_in_secs: i64,
}

/// Mints a bearer from a `refresh_token` grant. [`HttpMinter`] is the real
/// implementation; tests inject a fake so the cache/expiry path is verifiable
/// without a network round-trip.
pub trait TokenMinter: Send + Sync {
    fn mint(
        &self,
        req: RefreshRequest<'_>,
    ) -> impl std::future::Future<Output = Result<MintedToken>> + Send;
}

/// An OAuth credential that mints + caches short-lived bearers from a
/// long-lived `refresh_token`.
#[derive(Debug)]
pub struct OAuthCredential {
    pub token_endpoint: String,
    pub client_id: String,
    /// `None` for public clients (codex); `Some` for confidential ones (gemini).
    pub client_secret: Option<Secret>,
    pub refresh_token: Secret,
    cache: Mutex<Option<CachedToken>>,
}

/// Per-provider credential bundle. One uniform seam; the shape differences the
/// RCA found live inside the variants.
#[derive(Debug)]
pub enum Credential {
    /// A long-lived key/token used directly (claude: token-elsewhere).
    ApiKey(Secret),
    /// An OAuth `refresh_token` grant (codex / gemini).
    OAuth(OAuthCredential),
}

impl Credential {
    /// A static key/token (no refresh). Used as `x-api-key` or `Bearer`
    /// depending on token shape at the call site.
    pub fn api_key(token: impl Into<String>) -> Self {
        Credential::ApiKey(Secret::new(token))
    }

    /// codex / ChatGPT OAuth — public client, no secret.
    pub fn codex(client_id: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Credential::OAuth(OAuthCredential {
            token_endpoint: CODEX_TOKEN_ENDPOINT.to_string(),
            client_id: client_id.into(),
            client_secret: None,
            refresh_token: Secret::new(refresh_token),
            cache: Mutex::new(None),
        })
    }

    /// gemini / Google OAuth — confidential client, needs `client_secret`.
    pub fn gemini(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        refresh_token: impl Into<String>,
    ) -> Self {
        Credential::OAuth(OAuthCredential {
            token_endpoint: GOOGLE_TOKEN_ENDPOINT.to_string(),
            client_id: client_id.into(),
            client_secret: Some(Secret::new(client_secret)),
            refresh_token: Secret::new(refresh_token),
            cache: Mutex::new(None),
        })
    }

    /// Return a valid bearer, refreshing + caching an OAuth credential if the
    /// cached token is missing or within [`EXPIRY_SKEW_SECS`] of expiry.
    /// `now` is injected so callers/tests control the clock.
    pub async fn bearer(&self, minter: &impl TokenMinter, now: DateTime<Utc>) -> Result<Secret> {
        match self {
            Credential::ApiKey(k) => Ok(k.clone()),
            Credential::OAuth(o) => o.access_token(minter, now).await,
        }
    }
}

impl OAuthCredential {
    async fn access_token(&self, minter: &impl TokenMinter, now: DateTime<Utc>) -> Result<Secret> {
        // Fast path: a cached token still comfortably valid. The guard is
        // scoped so it is dropped before the await below — never hold a lock
        // across a refresh.
        {
            let guard = self.cache.lock().expect("credential cache poisoned");
            if let Some(cached) = guard.as_ref()
                && cached.expires_at > now + Duration::seconds(EXPIRY_SKEW_SECS)
            {
                return Ok(cached.access.clone());
            }
        }

        let minted = minter
            .mint(RefreshRequest {
                token_endpoint: &self.token_endpoint,
                client_id: &self.client_id,
                client_secret: self.client_secret.as_ref().map(Secret::expose),
                refresh_token: self.refresh_token.expose(),
            })
            .await
            .context("minting bearer via refresh_token grant")?;

        let token = CachedToken {
            access: minted.access_token.clone(),
            expires_at: now + Duration::seconds(minted.expires_in_secs),
        };
        *self.cache.lock().expect("credential cache poisoned") = Some(token);
        Ok(minted.access_token)
    }
}

/// Production [`TokenMinter`] — POSTs a `refresh_token` grant to the endpoint.
pub struct HttpMinter {
    pub http: reqwest::Client,
}

impl TokenMinter for HttpMinter {
    async fn mint(&self, req: RefreshRequest<'_>) -> Result<MintedToken> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("client_id", req.client_id),
            ("refresh_token", req.refresh_token),
        ];
        if let Some(secret) = req.client_secret {
            form.push(("client_secret", secret));
        }
        let resp = self
            .http
            .post(req.token_endpoint)
            .form(&form)
            .send()
            .await
            .context("posting refresh_token grant")?
            .error_for_status()
            .context("token endpoint returned an error status")?;
        let body: serde_json::Value = resp.json().await.context("parsing token response")?;
        let access = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .context("token response had no access_token")?;
        // OAuth `expires_in` is seconds; default to 1h if the endpoint omits it.
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        Ok(MintedToken {
            access_token: Secret::new(access),
            expires_in_secs: expires_in,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts refreshes and hands back a distinct token each time, so tests can
    /// assert exactly when a refresh happened.
    struct FakeMinter {
        calls: AtomicUsize,
    }

    impl TokenMinter for FakeMinter {
        async fn mint(&self, _req: RefreshRequest<'_>) -> Result<MintedToken> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(MintedToken {
                access_token: Secret::new(format!("tok-{n}")),
                expires_in_secs: 3600,
            })
        }
    }

    fn fixed_clock() -> DateTime<Utc> {
        Utc.timestamp_opt(1_000_000, 0).unwrap()
    }

    #[tokio::test]
    async fn oauth_bearer_refreshes_when_expired_and_caches() {
        let cred = Credential::gemini("cid", "csecret", "rtoken");
        let minter = FakeMinter {
            calls: AtomicUsize::new(0),
        };
        let t0 = fixed_clock();

        // First call mints.
        let b1 = cred.bearer(&minter, t0).await.unwrap();
        assert_eq!(b1.expose(), "tok-0");
        assert_eq!(minter.calls.load(Ordering::SeqCst), 1);

        // Well within expiry → served from cache, no new mint.
        let b2 = cred
            .bearer(&minter, t0 + Duration::seconds(600))
            .await
            .unwrap();
        assert_eq!(b2.expose(), "tok-0");
        assert_eq!(minter.calls.load(Ordering::SeqCst), 1);

        // Past the skew-adjusted expiry (3600 - 60) → refresh.
        let b3 = cred
            .bearer(&minter, t0 + Duration::seconds(3600))
            .await
            .unwrap();
        assert_eq!(b3.expose(), "tok-1");
        assert_eq!(minter.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn api_key_bearer_is_static_and_never_mints() {
        let cred = Credential::api_key("sk-ant-oat-example");
        let minter = FakeMinter {
            calls: AtomicUsize::new(0),
        };
        let b = cred.bearer(&minter, fixed_clock()).await.unwrap();
        assert_eq!(b.expose(), "sk-ant-oat-example");
        assert_eq!(minter.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn secret_never_leaks_through_debug() {
        let s = Secret::new("supersecret");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        // The whole credential must not leak the secret via Debug either.
        let dbg = format!("{:?}", Credential::gemini("cid", "csecret", "rtoken"));
        assert!(!dbg.contains("csecret"));
        assert!(!dbg.contains("rtoken"));
    }

    #[test]
    fn codex_is_public_client_gemini_is_confidential() {
        match Credential::codex("cid", "rt") {
            Credential::OAuth(o) => assert!(o.client_secret.is_none(), "codex client is public"),
            _ => panic!("codex should be OAuth"),
        }
        match Credential::gemini("cid", "cs", "rt") {
            Credential::OAuth(o) => assert!(o.client_secret.is_some(), "gemini needs a secret"),
            _ => panic!("gemini should be OAuth"),
        }
    }
}
