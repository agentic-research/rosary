//! Shared low-level Linear GraphQL transport.
//!
//! `linear.rs` (the CLI's one-shot `rsry sync`) and `linear_tracker.rs` (the
//! reconciler's `IssueTracker` impl) are a deliberate role split (commit
//! 4cba208) — but each carried its own copy of the API URL, client builder,
//! `graphql()` and `resolve_team_id()`, which had already drifted (one
//! `resolve_team_id` listed available teams on failure, the other didn't).
//! One transport, two consumers (rosary-457927).

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

/// Build a reqwest client with the Linear API key in the Authorization header.
pub(crate) fn build_client(api_key: &str) -> Result<reqwest::Client> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
}

/// Execute a GraphQL query against the Linear API and return the JSON
/// response. Fails loud on HTTP errors, non-JSON bodies, and GraphQL-level
/// `errors` — the caller never has to re-check.
pub(crate) async fn graphql(
    client: &reqwest::Client,
    query: &str,
    variables: Value,
) -> Result<Value> {
    let body = json!({
        "query": query,
        "variables": variables,
    });

    let resp = client
        .post(LINEAR_API_URL)
        .json(&body)
        .send()
        .await
        .context("failed to reach Linear API")?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .context("failed to read Linear response body")?;

    if !status.is_success() {
        anyhow::bail!("Linear API returned {status}: {text}");
    }

    let json: Value = serde_json::from_str(&text).context("Linear response is not valid JSON")?;

    if let Some(errors) = json.get("errors") {
        anyhow::bail!("Linear GraphQL errors: {errors}");
    }

    Ok(json)
}

/// Resolve a team key (e.g. "AGE") to Linear's internal team id. On a miss,
/// the error names the teams that ARE available.
pub(crate) async fn resolve_team_id(client: &reqwest::Client, team_key: &str) -> Result<String> {
    let query = r#"
        query Teams {
            teams {
                nodes {
                    id
                    key
                    name
                }
            }
        }
    "#;

    let resp = graphql(client, query, json!({})).await?;

    let teams = resp
        .pointer("/data/teams/nodes")
        .and_then(|v| v.as_array())
        .context("could not fetch teams from Linear")?;

    for team in teams {
        if team["key"].as_str() == Some(team_key)
            && let Some(id) = team["id"].as_str()
        {
            return Ok(id.to_string());
        }
    }

    anyhow::bail!(
        "team '{team_key}' not found. Available teams: {}",
        teams
            .iter()
            .filter_map(|t| t["key"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}
