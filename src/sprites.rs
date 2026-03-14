//! HTTP client for the sprites.dev compute API.
//!
//! Pure API wrapper — no rosary-specific logic. Follows the same pattern
//! as linear_tracker.rs: thin typed client over reqwest.
//!
//! API docs: https://sprites.dev/api
//! Auth: Bearer token via `Authorization` header.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_BASE_URL: &str = "https://api.sprites.dev/v1";

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

/// Sprite resource — a provisioned container.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sprite {
    pub name: String,
    #[serde(default)]
    pub status: String,
}

/// Options for creating a sprite.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateOpts {
    /// CPU cores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    /// Memory in MB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
}

/// Output from executing a command in a sprite.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecOutput {
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

/// Checkpoint snapshot.
#[derive(Debug, Clone, Deserialize)]
pub struct Checkpoint {
    pub id: String,
}

/// Resource policy for a sprite.
#[derive(Debug, Clone, Serialize)]
pub struct ResourcePolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
}

/// Network policy — egress allowlist.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkPolicy {
    pub allowlist: Vec<String>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// HTTP client for sprites.dev REST API.
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

    // -- Sprite CRUD --

    /// Create a sprite with the given name.
    pub async fn create_sprite(&self, name: &str, opts: &CreateOpts) -> Result<Sprite> {
        let url = self.url("/sprites");
        let mut body = json!({ "name": name });
        if let Some(cpu) = opts.cpu {
            body["cpu"] = json!(cpu);
        }
        if let Some(mem) = opts.memory_mb {
            body["memory_mb"] = json!(mem);
        }

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

    /// Get a sprite by name.
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

    /// Delete a sprite by name.
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

    // -- Exec --

    /// Execute a command in a sprite (HTTP, non-streaming).
    pub async fn exec(&self, name: &str, cmd: &str) -> Result<ExecOutput> {
        let url = self.url(&format!("/sprites/{name}/exec"));
        let body = json!({ "cmd": cmd });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("executing command")?;

        let status = resp.status();
        let text = resp.text().await.context("reading exec response")?;
        if !status.is_success() {
            anyhow::bail!("exec failed ({status}): {text}");
        }

        let output: ExecOutput = serde_json::from_str(&text).context("parsing exec response")?;
        Ok(output)
    }

    // -- Checkpoints --

    /// Create a checkpoint of the sprite's current state.
    pub async fn create_checkpoint(&self, name: &str) -> Result<Checkpoint> {
        let url = self.url(&format!("/sprites/{name}/checkpoints"));
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("creating checkpoint")?;

        let status = resp.status();
        let text = resp.text().await.context("reading checkpoint response")?;
        if !status.is_success() {
            anyhow::bail!("create checkpoint failed ({status}): {text}");
        }

        let cp: Checkpoint = serde_json::from_str(&text).context("parsing checkpoint response")?;
        Ok(cp)
    }

    /// Restore a checkpoint.
    pub async fn restore_checkpoint(&self, name: &str, checkpoint_id: &str) -> Result<()> {
        let url = self.url(&format!(
            "/sprites/{name}/checkpoints/{checkpoint_id}/restore"
        ));
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("restoring checkpoint")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("restore checkpoint failed ({status}): {text}");
        }

        Ok(())
    }

    // -- Policies --

    /// Set resource limits on a sprite.
    pub async fn set_resource_policy(&self, name: &str, policy: &ResourcePolicy) -> Result<()> {
        let url = self.url(&format!("/sprites/{name}/policies/resources"));
        let resp = self
            .client
            .post(&url)
            .json(policy)
            .send()
            .await
            .context("setting resource policy")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("set resource policy failed ({status}): {text}");
        }

        Ok(())
    }

    /// Set network egress policy (domain allowlist).
    pub async fn set_network_policy(&self, name: &str, domains: &[String]) -> Result<()> {
        let url = self.url(&format!("/sprites/{name}/policies/network"));
        let policy = NetworkPolicy {
            allowlist: domains.to_vec(),
        };
        let resp = self
            .client
            .post(&url)
            .json(&policy)
            .send()
            .await
            .context("setting network policy")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("set network policy failed ({status}): {text}");
        }

        Ok(())
    }

    // -- Filesystem --

    /// Write a file into the sprite.
    pub async fn write_file(&self, name: &str, path: &str, content: &[u8]) -> Result<()> {
        let url = self.url(&format!("/sprites/{name}/fs/{path}"));
        let resp = self
            .client
            .post(&url)
            .body(content.to_vec())
            .send()
            .await
            .context("writing file")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("write file failed ({status}): {text}");
        }

        Ok(())
    }

    /// Read a file from the sprite.
    pub async fn read_file(&self, name: &str, path: &str) -> Result<Vec<u8>> {
        let url = self.url(&format!("/sprites/{name}/fs/{path}"));
        let resp = self.client.get(&url).send().await.context("reading file")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("read file failed ({status}): {text}");
        }

        Ok(resp.bytes().await.context("reading file bytes")?.to_vec())
    }
}

// ---------------------------------------------------------------------------
// Tests — all use mock HTTP responses, no real API calls
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

    #[test]
    fn url_exec() {
        let client = SpritesClient::with_base_url("tok", "https://api.sprites.dev/v1").unwrap();
        assert_eq!(
            client.url("/sprites/s1/exec"),
            "https://api.sprites.dev/v1/sprites/s1/exec"
        );
    }

    #[test]
    fn url_checkpoints() {
        let client = SpritesClient::with_base_url("tok", "https://api.sprites.dev/v1").unwrap();
        assert_eq!(
            client.url("/sprites/s1/checkpoints"),
            "https://api.sprites.dev/v1/sprites/s1/checkpoints"
        );
        assert_eq!(
            client.url("/sprites/s1/checkpoints/cp-1/restore"),
            "https://api.sprites.dev/v1/sprites/s1/checkpoints/cp-1/restore"
        );
    }

    #[test]
    fn url_policies() {
        let client = SpritesClient::with_base_url("tok", "https://api.sprites.dev/v1").unwrap();
        assert_eq!(
            client.url("/sprites/s1/policies/resources"),
            "https://api.sprites.dev/v1/sprites/s1/policies/resources"
        );
        assert_eq!(
            client.url("/sprites/s1/policies/network"),
            "https://api.sprites.dev/v1/sprites/s1/policies/network"
        );
    }

    #[test]
    fn url_filesystem() {
        let client = SpritesClient::with_base_url("tok", "https://api.sprites.dev/v1").unwrap();
        assert_eq!(
            client.url("/sprites/s1/fs/tmp/prompt.txt"),
            "https://api.sprites.dev/v1/sprites/s1/fs/tmp/prompt.txt"
        );
    }

    // -- Serialization --

    #[test]
    fn create_opts_serializes() {
        let opts = CreateOpts {
            cpu: Some(2),
            memory_mb: Some(4096),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["cpu"], 2);
        assert_eq!(json["memory_mb"], 4096);
    }

    #[test]
    fn create_opts_skips_none() {
        let opts = CreateOpts::default();
        let json = serde_json::to_value(&opts).unwrap();
        assert!(json.get("cpu").is_none());
        assert!(json.get("memory_mb").is_none());
    }

    #[test]
    fn resource_policy_serializes() {
        let policy = ResourcePolicy {
            cpu: Some(4),
            memory_mb: None,
        };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json["cpu"], 4);
        assert!(json.get("memory_mb").is_none());
    }

    #[test]
    fn network_policy_serializes() {
        let policy = NetworkPolicy {
            allowlist: vec!["api.github.com".into(), "api.linear.app".into()],
        };
        let json = serde_json::to_value(&policy).unwrap();
        let list = json["allowlist"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], "api.github.com");
    }

    // -- Deserialization --

    #[test]
    fn sprite_deserializes() {
        let json = r#"{"name": "rsry-abc", "status": "running"}"#;
        let sprite: Sprite = serde_json::from_str(json).unwrap();
        assert_eq!(sprite.name, "rsry-abc");
        assert_eq!(sprite.status, "running");
    }

    #[test]
    fn sprite_deserializes_minimal() {
        // status is optional (defaults to empty)
        let json = r#"{"name": "test"}"#;
        let sprite: Sprite = serde_json::from_str(json).unwrap();
        assert_eq!(sprite.name, "test");
        assert_eq!(sprite.status, "");
    }

    #[test]
    fn exec_output_deserializes() {
        let json = r#"{"exit_code": 0, "stdout": "hello\n", "stderr": ""}"#;
        let out: ExecOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout, "hello\n");
    }

    #[test]
    fn exec_output_deserializes_minimal() {
        let json = r#"{"exit_code": 1}"#;
        let out: ExecOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.exit_code, 1);
        assert_eq!(out.stdout, "");
        assert_eq!(out.stderr, "");
    }

    #[test]
    fn checkpoint_deserializes() {
        let json = r#"{"id": "cp-12345"}"#;
        let cp: Checkpoint = serde_json::from_str(json).unwrap();
        assert_eq!(cp.id, "cp-12345");
    }
}
