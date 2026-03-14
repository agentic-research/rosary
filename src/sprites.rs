//! HTTP client for the sprites.dev compute API.
//!
//! API base: `https://api.sprites.dev/v1`
//! Auth: `Authorization: Bearer {token}` (from SPRITES_TOKEN env var)
//!
//! # Validation status
//!
//! Endpoints marked `[VALIDATED]` were tested against the live API on 2026-03-14.
//! Endpoints marked `[ASSUMED]` are based on docs but not yet validated.
//!
//! Real API responses captured from `sprite api` CLI:
//! - GET /sprites/{name} → full Sprite JSON with id, status, url, org, timestamps
//! - POST /sprites → create with `{"name": "..."}` body
//! - DELETE /sprites/{name} → destroys sprite
//! - POST /sprites/{name}/exec → streams raw stdout (NOT JSON), no exit code in response
//! - POST /checkpoints (via CLI) → creates sequential IDs (v0, v1, v2)
//!
//! Known 404s (do NOT exist):
//! - /policies/network
//! - /policies/resources
//! - /fs/{path} (filesystem API)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_BASE_URL: &str = "https://api.sprites.dev/v1";

// ---------------------------------------------------------------------------
// API types — [VALIDATED] against real responses from 2026-03-14
// ---------------------------------------------------------------------------

/// Sprite resource as returned by GET /sprites/{name}.
///
/// [VALIDATED] Real response:
/// ```json
/// {
///   "id": "sprite-091278d4-...",
///   "name": "rsry-test",
///   "status": "warm",
///   "url": "https://rsry-test-wrsa.sprites.app",
///   "organization": "james-gardner-570",
///   "created_at": "2026-03-14T21:31:57.855138Z",
///   ...
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sprite {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Options for creating a sprite.
/// [VALIDATED] Create body is just `{"name": "..."}`.
/// Resource options (cpu, memory) are [ASSUMED] — not yet tested.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateOpts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
}

/// Checkpoint as returned by the CLI.
/// [VALIDATED] IDs are sequential: v0, v1, v2, etc.
#[derive(Debug, Clone, Deserialize)]
pub struct Checkpoint {
    pub id: String,
}

/// Output from exec. [ASSUMED] — the real exec API streams raw stdout
/// over HTTP/WebSocket, NOT a JSON object. This struct is a rosary
/// abstraction that we populate by capturing the stream + exit code.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// HTTP client for sprites.dev REST API.
///
/// For exec, the real API uses WebSocket streaming. This client currently
/// uses the `sprite` CLI as a subprocess for exec (same pattern as our
/// AgentProvider). Direct WebSocket exec is a future optimization.
#[derive(Debug)]
pub struct SpritesClient {
    client: reqwest::Client,
    base_url: String,
}

impl SpritesClient {
    /// Create a client with the given API token.
    pub fn new(token: &str) -> Result<Self> {
        Self::with_base_url(token, DEFAULT_BASE_URL)
    }

    /// Create a client with a custom base URL (for testing with mock server).
    pub fn with_base_url(token: &str, base_url: &str) -> Result<Self> {
        use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

        anyhow::ensure!(!token.is_empty(), "sprites API token is empty");

        let bearer = format!("Bearer {token}");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&bearer).context("invalid token characters")?,
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Build a URL for the given path segments.
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    // -- Sprite CRUD [VALIDATED] --

    /// Create a sprite. [VALIDATED]
    pub async fn create_sprite(&self, name: &str, _opts: &CreateOpts) -> Result<Sprite> {
        let url = self.url("/sprites");
        let body = json!({ "name": name });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("creating sprite")?;

        let status = resp.status();
        let text = resp.text().await.context("reading create response")?;
        if !status.is_success() {
            anyhow::bail!("create sprite failed ({status}): {text}");
        }

        let sprite: Sprite =
            serde_json::from_str(&text).context("parsing create sprite response")?;
        Ok(sprite)
    }

    /// Get sprite details. [VALIDATED]
    pub async fn get_sprite(&self, name: &str) -> Result<Sprite> {
        let url = self.url(&format!("/sprites/{name}"));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("getting sprite")?;

        let status = resp.status();
        let text = resp.text().await.context("reading get response")?;
        if !status.is_success() {
            anyhow::bail!("get sprite failed ({status}): {text}");
        }

        let sprite: Sprite = serde_json::from_str(&text).context("parsing get sprite response")?;
        Ok(sprite)
    }

    /// Delete a sprite. [VALIDATED]
    pub async fn delete_sprite(&self, name: &str) -> Result<()> {
        let url = self.url(&format!("/sprites/{name}"));
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .context("deleting sprite")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("delete sprite failed ({status}): {text}");
        }

        Ok(())
    }

    // -- Exec [ASSUMED — uses CLI subprocess, not direct API] --

    /// Execute a command in a sprite via the `sprite` CLI.
    ///
    /// The sprites exec API uses WebSocket streaming, not REST JSON.
    /// We shell out to `sprite exec --http-post` which handles the
    /// protocol and gives us stdout on its own stdout.
    pub async fn exec(&self, name: &str, cmd: &str) -> Result<ExecOutput> {
        let output = tokio::process::Command::new("sprite")
            .args(["exec", "-s", name, "--http-post", "--", "sh", "-c", cmd])
            .output()
            .await
            .with_context(|| format!("sprite exec -s {name}: {cmd}"))?;

        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    // -- Checkpoints [VALIDATED via CLI] --

    /// Create a checkpoint via the `sprite` CLI.
    /// [VALIDATED] Returns sequential IDs: v0, v1, v2.
    pub async fn create_checkpoint(&self, name: &str) -> Result<Checkpoint> {
        let output = tokio::process::Command::new("sprite")
            .args(["checkpoint", "create", "-s", name])
            .output()
            .await
            .context("sprite checkpoint create")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("checkpoint create failed: {stderr}");
        }

        // Parse "✓ Checkpoint v2 created" from stdout
        let stdout = String::from_utf8_lossy(&output.stdout);
        let id = stdout
            .split_whitespace()
            .find(|w| w.starts_with('v') && w[1..].chars().all(|c| c.is_ascii_digit()))
            .unwrap_or("v0")
            .to_string();

        Ok(Checkpoint { id })
    }

    /// Restore a checkpoint via the `sprite` CLI. [VALIDATED]
    pub async fn restore_checkpoint(&self, name: &str, checkpoint_id: &str) -> Result<()> {
        let output = tokio::process::Command::new("sprite")
            .args(["restore", checkpoint_id, "-s", name])
            .output()
            .await
            .context("sprite restore")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("restore failed: {stderr}");
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests — validated against real API responses from 2026-03-14
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Construction --

    #[test]
    fn new_requires_token() {
        let result = SpritesClient::new("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("token is empty"));
    }

    #[test]
    fn new_with_valid_token() {
        let client = SpritesClient::new("test-token-123");
        assert!(client.is_ok());
    }

    #[test]
    fn with_base_url_overrides_default() {
        let client = SpritesClient::with_base_url("tok", "http://localhost:9999").unwrap();
        assert_eq!(client.url("/sprites"), "http://localhost:9999/sprites");
    }

    #[test]
    fn url_strips_trailing_slash() {
        let client = SpritesClient::with_base_url("tok", "http://localhost:9999/").unwrap();
        assert_eq!(client.url("/sprites"), "http://localhost:9999/sprites");
    }

    // -- URL building --

    #[test]
    fn url_sprite_crud() {
        let client = SpritesClient::with_base_url("tok", "https://api.sprites.dev/v1").unwrap();
        assert_eq!(client.url("/sprites"), "https://api.sprites.dev/v1/sprites");
        assert_eq!(
            client.url("/sprites/my-sprite"),
            "https://api.sprites.dev/v1/sprites/my-sprite"
        );
    }

    // -- Deserialization against REAL API responses --

    #[test]
    fn sprite_deserializes_real_response() {
        // Captured from: sprite api -s rsry-test /
        let json = r#"{
            "id": "sprite-091278d4-467a-4e40-b3d2-4fbda56ff365",
            "name": "rsry-test",
            "status": "warm",
            "version": null,
            "url": "https://rsry-test-wrsa.sprites.app",
            "url_settings": {"auth": "sprite"},
            "organization": "james-gardner-570",
            "last_running_at": "2026-03-14T21:32:03Z",
            "last_warming_at": "2026-03-14T21:32:05Z",
            "updated_at": "2026-03-14T21:31:57.855138Z",
            "created_at": "2026-03-14T21:31:57.855138Z",
            "environment_version": null
        }"#;
        let sprite: Sprite = serde_json::from_str(json).unwrap();
        assert_eq!(sprite.name, "rsry-test");
        assert_eq!(sprite.status, "warm");
        assert_eq!(sprite.id, "sprite-091278d4-467a-4e40-b3d2-4fbda56ff365");
        assert_eq!(
            sprite.url.as_deref(),
            Some("https://rsry-test-wrsa.sprites.app")
        );
        assert_eq!(sprite.organization.as_deref(), Some("james-gardner-570"));
    }

    #[test]
    fn sprite_list_response_deserializes() {
        // Captured from: sprite api /sprites
        let json = r#"{
            "name": "james-gardner-570",
            "running": 0,
            "sprites": [{
                "id": "sprite-091278d4-467a-4e40-b3d2-4fbda56ff365",
                "name": "rsry-test",
                "status": "warm",
                "version": null,
                "url": "https://rsry-test-wrsa.sprites.app",
                "url_settings": {"auth": "sprite"},
                "organization": "james-gardner-570",
                "last_running_at": "2026-03-14T21:32:03Z",
                "last_warming_at": "2026-03-14T21:32:05Z",
                "updated_at": "2026-03-14T21:31:57.855138Z",
                "created_at": "2026-03-14T21:31:57.855138Z",
                "environment_version": null
            }],
            "next_continuation_token": null,
            "warm": 1,
            "cold": 0,
            "running_limit": 10,
            "warm_limit": 10,
            "has_more": true
        }"#;
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        let sprites: Vec<Sprite> = serde_json::from_value(parsed["sprites"].clone()).unwrap();
        assert_eq!(sprites.len(), 1);
        assert_eq!(sprites[0].name, "rsry-test");
    }

    #[test]
    fn exec_output_fields() {
        let out = ExecOutput {
            exit_code: 0,
            stdout: "hello\n".into(),
            stderr: String::new(),
        };
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout, "hello\n");
    }

    #[test]
    fn checkpoint_id_format() {
        // Real checkpoints use v0, v1, v2 — not UUIDs
        let cp = Checkpoint {
            id: "v1".to_string(),
        };
        assert_eq!(cp.id, "v1");
    }

    #[test]
    fn create_opts_default() {
        let opts = CreateOpts::default();
        assert!(opts.cpu.is_none());
        assert!(opts.memory_mb.is_none());
    }
}
