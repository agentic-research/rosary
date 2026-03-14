use serde::Serialize;

use crate::error::Result;
use crate::projection::{BeadProjection, project_bead};

/// Maps a BeadProjection to the Wasteland MVR `wanted` table schema.
#[derive(Debug, Serialize)]
pub struct WantedItem {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub project: Option<String>,
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    pub priority: Option<i32>,
    pub tags: Option<serde_json::Value>,
    pub posted_by: String,
    pub status: String,
    pub effort_level: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Convert a bead JSON to a Wasteland wanted item.
/// Public fields map directly. Private fields go into metadata as encrypted blobs.
pub fn bead_to_wanted(
    bead_json: &serde_json::Value,
    key: &[u8; 32],
    rig_handle: &str,
    project: Option<&str>,
) -> Result<WantedItem> {
    let projection = project_bead(bead_json, key)?;

    let priority = projection
        .priority
        .as_deref()
        .and_then(|p| p.parse::<i32>().ok());

    let bead_status = projection.status.as_deref().unwrap_or("open");
    let wl_status = match bead_status {
        "open" => "open",
        "in_progress" => "claimed",
        "closed" | "done" => "completed",
        "deferred" => "withdrawn",
        _ => "open",
    };

    // Pack encrypted private fields into metadata
    let encrypted_meta = build_encrypted_metadata(&projection);

    Ok(WantedItem {
        id: format!("w-rr-{}", &projection.id),
        title: projection.title,
        description: None, // encrypted, not exposed
        project: project.map(|s| s.to_string()),
        item_type: projection.issue_type,
        priority,
        tags: None,
        posted_by: rig_handle.to_string(),
        status: wl_status.to_string(),
        effort_level: None,
        metadata: encrypted_meta,
    })
}

fn build_encrypted_metadata(projection: &BeadProjection) -> Option<serde_json::Value> {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "source".to_string(),
        serde_json::Value::String("rosary".to_string()),
    );
    meta.insert(
        "version".to_string(),
        serde_json::Value::String("0.1".to_string()),
    );

    let mut encrypted = serde_json::Map::new();
    if let Some(ref d) = projection.description {
        encrypted.insert(
            "description".to_string(),
            serde_json::Value::String(d.clone()),
        );
    }
    if let Some(ref o) = projection.owner {
        encrypted.insert("owner".to_string(), serde_json::Value::String(o.clone()));
    }
    if let Some(ref b) = projection.branch {
        encrypted.insert("branch".to_string(), serde_json::Value::String(b.clone()));
    }
    if let Some(ref p) = projection.pr_url {
        encrypted.insert("pr_url".to_string(), serde_json::Value::String(p.clone()));
    }
    if let Some(ref d) = projection.design {
        encrypted.insert("design".to_string(), serde_json::Value::String(d.clone()));
    }
    if let Some(ref n) = projection.notes {
        encrypted.insert("notes".to_string(), serde_json::Value::String(n.clone()));
    }

    if !encrypted.is_empty() {
        meta.insert(
            "encrypted_fields".to_string(),
            serde_json::Value::Object(encrypted),
        );
    }

    Some(serde_json::Value::Object(meta))
}

/// Generate the Dolt SQL INSERT for a wanted item.
pub fn wanted_to_sql(item: &WantedItem) -> String {
    let tags = item
        .tags
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "NULL".to_string());

    let metadata = item
        .metadata
        .as_ref()
        .map(|m| format!("'{}'", m.to_string().replace('\'', "''")))
        .unwrap_or_else(|| "NULL".to_string());

    let project = item
        .project
        .as_deref()
        .map(|p| format!("'{}'", p))
        .unwrap_or_else(|| "NULL".to_string());

    let item_type = item
        .item_type
        .as_deref()
        .map(|t| format!("'{}'", t))
        .unwrap_or_else(|| "'feature'".to_string());

    let priority = item.priority.unwrap_or(2);

    let effort = item
        .effort_level
        .as_deref()
        .map(|e| format!("'{}'", e))
        .unwrap_or_else(|| "'medium'".to_string());

    format!(
        "INSERT INTO wanted (id, title, description, project, type, priority, tags, posted_by, status, effort_level, metadata, created_at, updated_at) \
         VALUES ('{}', '{}', NULL, {}, {}, {}, {}, '{}', '{}', {}, {}, NOW(), NOW())",
        item.id,
        item.title.replace('\'', "''"),
        project,
        item_type,
        priority,
        tags,
        item.posted_by,
        item.status,
        effort,
        metadata,
    )
}
