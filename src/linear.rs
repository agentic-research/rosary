use anyhow::{Context, Result};
use serde_json::{Value, json};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

/// Build a reqwest client with the Linear API key in the Authorization header.
fn build_client(api_key: &str) -> Result<reqwest::Client> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
}

/// Execute a GraphQL query against the Linear API and return the JSON response.
async fn graphql(client: &reqwest::Client, query: &str, variables: Value) -> Result<Value> {
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

/// Read LINEAR_API_KEY from the environment. Returns None (with a helpful message) if unset.
fn get_api_key() -> Option<String> {
    match std::env::var("LINEAR_API_KEY") {
        Ok(key) if !key.is_empty() => Some(key),
        _ => {
            eprintln!("LINEAR_API_KEY is not set.");
            eprintln!("Get your API key from: https://linear.app/settings/api");
            eprintln!("Then: export LINEAR_API_KEY=lin_api_...");
            None
        }
    }
}

/// Parse a Linear issue identifier from either a bare ID ("ART-123") or a URL.
///
/// Supported URL formats:
/// - `https://linear.app/team/issue/ART-123/slug`
/// - `https://linear.app/team/issue/ART-123`
fn parse_issue_identifier(ticket: &str) -> String {
    // If it looks like a URL, extract the identifier segment
    if ticket.starts_with("http://") || ticket.starts_with("https://") {
        // URL pattern: .../issue/ART-123/...
        if let Some(idx) = ticket.find("/issue/") {
            let after = &ticket[idx + 7..]; // skip "/issue/"
            // Take up to the next '/' or end of string
            return after.split('/').next().unwrap_or(after).to_string();
        }
    }
    ticket.to_string()
}

/// Format a priority number into a human-readable label.
fn priority_label(p: i64) -> &'static str {
    match p {
        0 => "No priority",
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "Unknown",
    }
}

/// Decompose a Linear ticket into repo-scoped beads (top-down planning).
///
/// Currently fetches and displays the issue. Future: analyze description for repo
/// references and create beads in each referenced repo via `bd create`.
pub async fn plan(ticket: &str) -> Result<()> {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => return Ok(()),
    };

    let client = build_client(&api_key)?;
    let identifier = parse_issue_identifier(ticket);

    // Use the issues filter to look up by team key + number.
    let query = r#"
        query IssueByIdentifier($filter: IssueFilter!) {
            issues(filter: $filter, first: 1) {
                nodes {
                    id
                    identifier
                    title
                    description
                    priority
                    state { name }
                    labels { nodes { name } }
                }
            }
        }
    "#;

    // Split identifier (e.g., "ART-123") into team key and number
    let parts: Vec<&str> = identifier.splitn(2, '-').collect();
    let filter = if parts.len() == 2 {
        if let Ok(num) = parts[1].parse::<i64>() {
            json!({
                "team": { "key": { "eq": parts[0] } },
                "number": { "eq": num }
            })
        } else {
            json!({ "identifier": { "eq": identifier } })
        }
    } else {
        json!({ "identifier": { "eq": identifier } })
    };

    let resp = graphql(&client, query, json!({ "filter": filter })).await?;

    let issue = resp
        .pointer("/data/issues/nodes/0")
        .context("issue not found — check that the identifier is correct")?;

    let title = issue["title"].as_str().unwrap_or("(untitled)");
    let ident = issue["identifier"].as_str().unwrap_or("???");
    let state = issue
        .pointer("/state/name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let priority = issue["priority"].as_i64().unwrap_or(0);
    let description = issue["description"].as_str().unwrap_or("(no description)");

    let labels: Vec<&str> = issue
        .pointer("/labels/nodes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|l| l["name"].as_str()).collect())
        .unwrap_or_default();

    println!("--- Linear Issue ---");
    println!("Identifier: {ident}");
    println!("Title:      {title}");
    println!("State:      {state}");
    println!("Priority:   {} ({})", priority, priority_label(priority));
    if !labels.is_empty() {
        println!("Labels:     {}", labels.join(", "));
    }
    println!();
    println!("Description:");
    println!("{description}");
    println!("--------------------");

    // Future: parse description for repo references, decompose into beads
    println!();
    println!("(bead decomposition not yet implemented — coming soon)");

    Ok(())
}

/// Look up a team's internal ID by its key (e.g., "ART").
async fn resolve_team_id(client: &reqwest::Client, team_key: &str) -> Result<String> {
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

/// Bidirectional sync: beads <-> Linear.
///
/// Currently fetches open issues from the configured team and prints a summary.
/// Bidirectional sync is not yet implemented.
pub async fn sync() -> Result<()> {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => return Ok(()),
    };

    let team_key = std::env::var("LINEAR_TEAM").unwrap_or_else(|_| "ART".to_string());

    let client = build_client(&api_key)?;

    println!("Looking up team '{team_key}'...");
    let team_id = resolve_team_id(&client, &team_key).await?;

    let query = r#"
        query TeamIssues($teamId: String!, $filter: IssueFilter) {
            team(id: $teamId) {
                name
                key
                issues(first: 50, filter: $filter) {
                    nodes {
                        identifier
                        title
                        priority
                        state { name }
                    }
                }
            }
        }
    "#;

    let variables = json!({
        "teamId": team_id,
        "filter": {
            "state": { "type": { "in": ["started", "unstarted"] } }
        }
    });

    let resp = graphql(&client, query, variables).await?;

    let team_name = resp
        .pointer("/data/team/name")
        .and_then(|v| v.as_str())
        .unwrap_or(&team_key);

    let issues = resp
        .pointer("/data/team/issues/nodes")
        .and_then(|v| v.as_array())
        .context("could not fetch issues from Linear")?;

    println!();
    println!("=== {team_name} — Open Issues ({} found) ===", issues.len());
    println!();

    if issues.is_empty() {
        println!("  No open issues.");
    } else {
        for issue in issues {
            let ident = issue["identifier"].as_str().unwrap_or("???");
            let title = issue["title"].as_str().unwrap_or("(untitled)");
            let state = issue
                .pointer("/state/name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let priority = issue["priority"].as_i64().unwrap_or(0);
            println!("  {ident}  [{state}]  P{priority}  {title}");
        }
    }

    println!();
    println!("Note: bidirectional bead <-> Linear sync is not yet implemented.");
    println!("Currently read-only. Tracking: loom-7sd");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_identifier() {
        assert_eq!(parse_issue_identifier("ART-123"), "ART-123");
    }

    #[test]
    fn parse_url_with_slug() {
        let url = "https://linear.app/art/issue/ART-123/some-slug-here";
        assert_eq!(parse_issue_identifier(url), "ART-123");
    }

    #[test]
    fn parse_url_without_slug() {
        let url = "https://linear.app/art/issue/ART-456";
        assert_eq!(parse_issue_identifier(url), "ART-456");
    }

    #[test]
    fn parse_non_url_passthrough() {
        assert_eq!(parse_issue_identifier("FOO-1"), "FOO-1");
    }

    #[test]
    fn priority_labels() {
        assert_eq!(priority_label(0), "No priority");
        assert_eq!(priority_label(1), "Urgent");
        assert_eq!(priority_label(2), "High");
        assert_eq!(priority_label(3), "Medium");
        assert_eq!(priority_label(4), "Low");
        assert_eq!(priority_label(99), "Unknown");
    }
}
