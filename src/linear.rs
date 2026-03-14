use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::bead::BeadState;

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

/// Connect to a repo's Dolt database via its .beads/ directory.
async fn connect_repo_dolt(repo: &crate::config::RepoConfig) -> Result<crate::dolt::DoltClient> {
    let path = crate::scanner::expand_path(&repo.path);
    let beads_dir = path.join(".beads");
    let config = crate::dolt::DoltConfig::from_beads_dir(&beads_dir)?;
    crate::dolt::DoltClient::connect(&config).await
}

/// Bidirectional sync: beads <-> Linear.
///
/// 1. Link: match existing Linear issues to beads by title, store external_ref
/// 2. Push: create Linear issues for unlinked beads, store external_ref
/// 3. Close: update Linear issues for closed beads
pub async fn sync(dry_run: bool) -> Result<()> {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => return Ok(()),
    };

    // Read team key: env var > config > default
    let team_key = std::env::var("LINEAR_TEAM").unwrap_or_else(|_| {
        crate::config::load_merged("rosary.toml")
            .ok()
            .and_then(|cfg| cfg.linear.map(|l| l.team))
            .unwrap_or_else(|| "ART".to_string())
    });

    let client = build_client(&api_key)?;

    println!("Looking up team '{team_key}'...");
    let team_id = resolve_team_id(&client, &team_key).await?;

    let query = r#"
        query TeamIssues($teamId: String!, $filter: IssueFilter) {
            team(id: $teamId) {
                name
                key
                issues(first: 250, filter: $filter) {
                    nodes {
                        identifier
                        title
                        description
                        priority
                        state { name }
                        url
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

    // --- Build per-repo Dolt client map ---
    let cfg = crate::config::load_merged("rosary.toml")?;
    let beads = crate::scanner::scan_repos(&cfg.repo).await?;

    let mut dolt_clients: std::collections::HashMap<String, crate::dolt::DoltClient> =
        std::collections::HashMap::new();
    for repo in &cfg.repo {
        match connect_repo_dolt(repo).await {
            Ok(dc) => {
                dolt_clients.insert(repo.name.clone(), dc);
            }
            Err(e) => {
                eprintln!("  warning: cannot connect to {} Dolt: {e}", repo.name);
            }
        }
    }

    // Build Linear issue lookup: identifier → (title, description, url, state)
    let linear_issues: Vec<(&str, &str, &str, &str, &str)> = issues
        .iter()
        .filter_map(|i| {
            let ident = i["identifier"].as_str()?;
            let title = i["title"].as_str()?;
            let desc = i["description"].as_str().unwrap_or("");
            let url = i["url"].as_str().unwrap_or("");
            let state = i
                .pointer("/state/name")
                .and_then(|v| v.as_str())
                .unwrap_or("Todo");
            Some((ident, title, desc, url, state))
        })
        .collect();

    // --- LINK: match existing Linear issues to unlinked beads ---
    // Match by: (1) bead tag in description, (2) title match
    let mut linked = 0;
    let mut linked_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for bead in &beads {
        if bead.external_ref.is_some() {
            continue;
        }
        let bead_tag = format!("<!-- bead:{} ", bead.id);
        let prefixed_title = format!("[{}] {}", bead.repo, bead.title);

        let matched = linear_issues.iter().find(|(_, title, desc, _, _)| {
            // Match by bead tag in description (strongest signal)
            desc.contains(&bead_tag)
                // Or match by title
                || *title == prefixed_title
                || *title == bead.title
        });

        if let Some((ident, _, _, _url, _)) = matched {
            if dry_run {
                println!("  [dry-run] would link {} → {ident}", bead.id);
                linked += 1;
                linked_ids.insert(bead.id.clone());
            } else if let Some(dc) = dolt_clients.get(&bead.repo) {
                if let Err(e) = dc.set_external_ref(&bead.id, ident).await {
                    eprintln!("  ✗ Failed to link {} → {ident}: {e}", bead.id);
                } else {
                    println!("  ↔ Linked {} → {ident}", bead.id);
                    linked += 1;
                    linked_ids.insert(bead.id.clone());
                }
            }
        }
    }

    // --- PUSH: create Linear issues for unlinked beads, store external_ref ---
    let mut created = 0;
    for bead in &beads {
        if bead.external_ref.is_some() || linked_ids.contains(&bead.id) {
            continue;
        }
        if bead.status == "closed" {
            continue;
        }
        if bead.priority > 2 {
            continue;
        }
        // Skip if we just linked it above (check by title match)
        let prefixed_title = format!("[{}] {}", bead.repo, bead.title);
        if linear_issues
            .iter()
            .any(|(_, t, _, _, _)| *t == prefixed_title || *t == bead.title)
        {
            continue;
        }

        let label = format!("[{}] ", bead.repo);
        let full_title = format!("{label}{}", bead.title);

        if dry_run {
            println!("  [dry-run] would create: {full_title}");
            created += 1;
        } else {
            match create_linear_issue(
                &client,
                &team_id,
                &full_title,
                &bead.description,
                bead.priority,
                &bead.id,
                &bead.repo,
            )
            .await
            {
                Ok(ident) => {
                    println!("  → Created {ident}: {full_title}");
                    created += 1;
                    if let Some(dc) = dolt_clients.get(&bead.repo)
                        && let Err(e) = dc.set_external_ref(&bead.id, &ident).await
                    {
                        eprintln!("  ✗ Failed to store external_ref for {}: {e}", bead.id);
                    }
                }
                Err(e) => {
                    eprintln!("  ✗ Failed to create issue for {}: {e}", bead.id);
                }
            }
        }
    }

    // --- CLOSE: update Linear issues for closed beads ---
    let mut closed = 0;
    for repo in &cfg.repo {
        let Some(dc) = dolt_clients.get(&repo.name) else {
            continue;
        };
        let closed_beads = match dc.list_closed_linked_beads(&repo.name).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "  warning: failed to query closed beads for {}: {e}",
                    repo.name
                );
                continue;
            }
        };
        for bead in &closed_beads {
            let ext_ref = bead.external_ref.as_deref().unwrap_or_default();
            // Only close issues that are still open in Linear
            if linear_issues
                .iter()
                .any(|(ident, _, _, _, _)| *ident == ext_ref)
            {
                if dry_run {
                    println!("  [dry-run] would close {ext_ref} (bead {})", bead.id);
                    closed += 1;
                } else {
                    match update_linear_issue_status(&client, &team_id, ext_ref, "closed").await {
                        Ok(()) => {
                            println!("  ✓ Closed {ext_ref} (bead {})", bead.id);
                            closed += 1;
                        }
                        Err(e) => {
                            eprintln!("  ✗ Failed to close {ext_ref}: {e}",);
                        }
                    }
                }
            }
        }
    }

    println!();
    if linked > 0 {
        println!("  Linked {linked} existing issue(s)");
    }
    if created > 0 {
        println!("  Pushed {created} bead(s) → Linear");
    }
    if closed > 0 {
        println!("  Closed {closed} Linear issue(s)");
    }
    if linked == 0 && created == 0 && closed == 0 {
        println!("  Everything in sync.");
    }

    Ok(())
}

/// Create a new issue in Linear with bead ID tagged in description.
/// Returns the issue identifier (e.g., "AGE-5").
async fn create_linear_issue(
    client: &reqwest::Client,
    team_id: &str,
    title: &str,
    description: &str,
    priority: u8,
    bead_id: &str,
    repo_name: &str,
) -> Result<String> {
    // Tag the description with bead ID for bidirectional linkage
    let tagged_description = format!("{description}\n\n<!-- bead:{bead_id} repo:{repo_name} -->",);
    let mutation = r#"
        mutation CreateIssue($input: IssueCreateInput!) {
            issueCreate(input: $input) {
                success
                issue {
                    identifier
                    title
                }
            }
        }
    "#;

    // Map bead priority (0=P0 highest) to Linear priority (1=urgent, 4=low)
    let linear_priority = match priority {
        0 => 1, // P0 → Urgent
        1 => 2, // P1 → High
        2 => 3, // P2 → Medium
        _ => 4, // P3+ → Low
    };

    let variables = json!({
        "input": {
            "teamId": team_id,
            "title": title,
            "description": tagged_description,
            "priority": linear_priority,
        }
    });

    let resp = graphql(client, mutation, variables).await?;

    let success = resp
        .pointer("/data/issueCreate/success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !success {
        anyhow::bail!("Linear issueCreate returned success=false");
    }

    let identifier = resp
        .pointer("/data/issueCreate/issue/identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("???");

    Ok(identifier.to_string())
}

/// Update a Linear issue's workflow state.
/// Maps rosary status ("closed", "in_progress", "open") to Linear state types.
async fn update_linear_issue_status(
    client: &reqwest::Client,
    team_id: &str,
    external_id: &str,
    status: &str,
) -> Result<()> {
    // Find the issue's internal ID by identifier
    let find_query = r#"
        query FindIssue($filter: IssueFilter!) {
            issues(filter: $filter, first: 1) {
                nodes { id }
            }
        }
    "#;

    let parts: Vec<&str> = external_id.splitn(2, '-').collect();
    let filter = if parts.len() == 2 {
        if let Ok(num) = parts[1].parse::<i64>() {
            json!({
                "team": { "key": { "eq": parts[0] } },
                "number": { "eq": num }
            })
        } else {
            json!({ "identifier": { "eq": external_id } })
        }
    } else {
        json!({ "identifier": { "eq": external_id } })
    };

    let resp = graphql(client, find_query, json!({ "filter": filter })).await?;
    let issue_id = resp
        .pointer("/data/issues/nodes/0/id")
        .and_then(|v| v.as_str())
        .context("issue not found in Linear")?;

    // Find the target workflow state
    let states_query = r#"
        query TeamStates($teamId: String!) {
            team(id: $teamId) {
                states { nodes { id name type } }
            }
        }
    "#;
    let states_resp = graphql(client, states_query, json!({ "teamId": team_id })).await?;
    let states = states_resp
        .pointer("/data/team/states/nodes")
        .and_then(|v| v.as_array())
        .context("fetching workflow states")?;

    // Map rosary status to Linear state via type (stable) + name hint (refinement)
    let bead_state = BeadState::from(status);
    let (target_type, preferred_name) = bead_state.to_linear_type();

    // Try preferred name within type first, fall back to any state with matching type
    let target_state = states
        .iter()
        .find(|s| {
            s["type"].as_str() == Some(target_type) && s["name"].as_str() == Some(preferred_name)
        })
        .or_else(|| {
            states
                .iter()
                .find(|s| s["type"].as_str() == Some(target_type))
        })
        .and_then(|s| s["id"].as_str())
        .context(format!(
            "no Linear state with type '{target_type}' for bead status '{status}'"
        ))?;

    // Update the issue
    let mutation = r#"
        mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {
            issueUpdate(id: $id, input: $input) { success }
        }
    "#;
    let resp = graphql(
        client,
        mutation,
        json!({ "id": issue_id, "input": { "stateId": target_state } }),
    )
    .await?;

    let success = resp
        .pointer("/data/issueUpdate/success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !success {
        anyhow::bail!("issueUpdate failed for {external_id}");
    }

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

    #[test]
    fn bead_to_linear_priority_mapping() {
        // Bead P0 (critical) → Linear 1 (Urgent)
        // Bead P1 (high) → Linear 2 (High)
        // Bead P2 (medium) → Linear 3 (Medium)
        // Bead P3+ → Linear 4 (Low)
        let map = |p: u8| -> i32 {
            match p {
                0 => 1,
                1 => 2,
                2 => 3,
                _ => 4,
            }
        };
        assert_eq!(map(0), 1);
        assert_eq!(map(1), 2);
        assert_eq!(map(2), 3);
        assert_eq!(map(3), 4);
        assert_eq!(map(4), 4);
    }

    #[test]
    fn build_client_requires_valid_header() {
        // API key must be valid ASCII for HTTP header
        let result = build_client("lin_api_valid_key_123");
        assert!(result.is_ok());
    }

    #[test]
    fn build_client_rejects_invalid_header() {
        // Non-ASCII in header value should fail
        let result = build_client("invalid\x00key");
        assert!(result.is_err());
    }

    /// Integration test — only runs when LINEAR_API_KEY is set.
    #[tokio::test]
    async fn sync_live_linear() {
        let api_key = match std::env::var("LINEAR_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping: LINEAR_API_KEY not set");
                return;
            }
        };

        let client = build_client(&api_key).unwrap();
        let team_key = std::env::var("LINEAR_TEAM").unwrap_or_else(|_| "AGE".to_string());

        // Should be able to resolve team
        let team_id = resolve_team_id(&client, &team_key).await;
        assert!(team_id.is_ok(), "failed to resolve team {team_key}");
    }
}
