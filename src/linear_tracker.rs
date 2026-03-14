//! Linear implementation of the IssueTracker trait.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::sync::{ExternalIssue, IssueTracker};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

pub struct LinearTracker {
    client: reqwest::Client,
    team_id: String,
    team_key: String,
}

impl LinearTracker {
    pub async fn new(api_key: &str, team_key: &str) -> Result<Self> {
        use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        // Resolve team ID
        let team_id = resolve_team_id(&client, team_key).await?;

        Ok(Self {
            client,
            team_id,
            team_key: team_key.to_string(),
        })
    }
}

async fn graphql(client: &reqwest::Client, query: &str, variables: Value) -> Result<Value> {
    let body = json!({ "query": query, "variables": variables });
    let resp = client
        .post(LINEAR_API_URL)
        .json(&body)
        .send()
        .await
        .context("Linear API request failed")?;

    let status = resp.status();
    let text = resp.text().await.context("reading Linear response")?;

    if !status.is_success() {
        anyhow::bail!("Linear API {status}: {text}");
    }

    let json: Value = serde_json::from_str(&text).context("parsing Linear response")?;
    if let Some(errors) = json.get("errors") {
        anyhow::bail!("Linear GraphQL errors: {errors}");
    }
    Ok(json)
}

async fn resolve_team_id(client: &reqwest::Client, team_key: &str) -> Result<String> {
    let query = r#"
        query Teams {
            teams { nodes { id key } }
        }
    "#;
    let resp = graphql(client, query, json!({})).await?;
    let teams = resp
        .pointer("/data/teams/nodes")
        .and_then(|v| v.as_array())
        .context("fetching teams")?;

    for team in teams {
        if team["key"].as_str() == Some(team_key)
            && let Some(id) = team["id"].as_str()
        {
            return Ok(id.to_string());
        }
    }
    anyhow::bail!("team '{team_key}' not found")
}

/// Map Linear workflow state name to rosary status.
fn map_linear_status(state_name: &str) -> &'static str {
    match state_name.to_lowercase().as_str() {
        "done" | "completed" | "closed" | "cancelled" | "canceled" => "closed",
        "in progress" | "in review" | "started" => "in_progress",
        _ => "open", // Todo, Backlog, Triage, etc.
    }
}

/// Map rosary priority to Linear priority.
fn to_linear_priority(priority: u8) -> i32 {
    match priority {
        0 => 1, // P0 → Urgent
        1 => 2, // P1 → High
        2 => 3, // P2 → Medium
        _ => 4, // P3+ → Low
    }
}

#[async_trait::async_trait]
impl IssueTracker for LinearTracker {
    async fn list_open(&self) -> Result<Vec<ExternalIssue>> {
        let query = r#"
            query TeamIssues($teamId: String!, $filter: IssueFilter) {
                team(id: $teamId) {
                    issues(first: 250, filter: $filter) {
                        nodes {
                            identifier
                            title
                            description
                            priority
                            state { name }
                            labels { nodes { name } }
                        }
                    }
                }
            }
        "#;

        let variables = json!({
            "teamId": self.team_id,
            "filter": {
                "state": { "type": { "in": ["started", "unstarted"] } }
            }
        });

        let resp = graphql(&self.client, query, variables).await?;
        let nodes = resp
            .pointer("/data/team/issues/nodes")
            .and_then(|v| v.as_array())
            .context("fetching issues")?;

        let issues = nodes
            .iter()
            .map(|n| {
                let labels: Vec<String> = n
                    .pointer("/labels/nodes")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| l["name"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                let state_name = n
                    .pointer("/state/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Todo");

                ExternalIssue {
                    external_id: n["identifier"].as_str().unwrap_or("").to_string(),
                    title: n["title"].as_str().unwrap_or("").to_string(),
                    description: n["description"].as_str().unwrap_or("").to_string(),
                    status: map_linear_status(state_name).to_string(),
                    priority: n["priority"].as_u64().unwrap_or(3) as u8,
                    labels,
                }
            })
            .collect();

        Ok(issues)
    }

    async fn create(&self, issue: &ExternalIssue) -> Result<String> {
        let mutation = r#"
            mutation CreateIssue($input: IssueCreateInput!) {
                issueCreate(input: $input) {
                    success
                    issue { identifier }
                }
            }
        "#;

        let variables = json!({
            "input": {
                "teamId": self.team_id,
                "title": issue.title,
                "description": issue.description,
                "priority": to_linear_priority(issue.priority),
            }
        });

        let resp = graphql(&self.client, mutation, variables).await?;
        let success = resp
            .pointer("/data/issueCreate/success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !success {
            anyhow::bail!("issueCreate failed");
        }

        Ok(resp
            .pointer("/data/issueCreate/issue/identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("???")
            .to_string())
    }

    async fn update_status(&self, external_id: &str, status: &str) -> Result<()> {
        // First, find the issue's internal ID
        let query = r#"
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

        let resp = graphql(&self.client, query, json!({ "filter": filter })).await?;
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
        let states_resp = graphql(
            &self.client,
            states_query,
            json!({ "teamId": self.team_id }),
        )
        .await?;
        let states = states_resp
            .pointer("/data/team/states/nodes")
            .and_then(|v| v.as_array())
            .context("fetching workflow states")?;

        // Map rosary status to Linear state type
        let target_type = match status {
            "closed" => "completed",
            "in_progress" => "started",
            "blocked" => "started", // Linear has no "blocked" — use started
            _ => "unstarted",
        };

        let target_state = states
            .iter()
            .find(|s| s["type"].as_str() == Some(target_type))
            .and_then(|s| s["id"].as_str())
            .context("no matching workflow state")?;

        // Update the issue
        let mutation = r#"
            mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {
                issueUpdate(id: $id, input: $input) { success }
            }
        "#;
        let resp = graphql(
            &self.client,
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

    fn name(&self) -> &str {
        "linear"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_status_done() {
        assert_eq!(map_linear_status("Done"), "closed");
        assert_eq!(map_linear_status("Completed"), "closed");
        assert_eq!(map_linear_status("Cancelled"), "closed");
    }

    #[test]
    fn map_status_in_progress() {
        assert_eq!(map_linear_status("In Progress"), "in_progress");
        assert_eq!(map_linear_status("In Review"), "in_progress");
        assert_eq!(map_linear_status("Started"), "in_progress");
    }

    #[test]
    fn map_status_open() {
        assert_eq!(map_linear_status("Todo"), "open");
        assert_eq!(map_linear_status("Backlog"), "open");
        assert_eq!(map_linear_status("Triage"), "open");
    }

    #[test]
    fn priority_mapping() {
        assert_eq!(to_linear_priority(0), 1); // P0 → Urgent
        assert_eq!(to_linear_priority(1), 2); // P1 → High
        assert_eq!(to_linear_priority(2), 3); // P2 → Medium
        assert_eq!(to_linear_priority(3), 4); // P3 → Low
    }
}
