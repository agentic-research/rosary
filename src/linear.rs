use anyhow::Result;

/// Decompose a Linear ticket into repo-scoped beads (top-down planning)
pub async fn plan(_ticket: &str) -> Result<()> {
    // TODO: fetch Linear ticket via GraphQL
    // TODO: analyze description for repo references
    // TODO: create beads in each referenced repo via `bd create`
    todo!("plan: Linear ticket → decompose → beads")
}

/// Bidirectional sync: beads ↔ Linear
pub async fn sync() -> Result<()> {
    // TODO: for each repo, read beads
    // TODO: for each bead with a Linear ref, update Linear status
    // TODO: for each Linear ticket with bead refs, update bead status
    todo!("sync: beads ↔ Linear")
}

#[cfg(test)]
mod tests {
    // Linear integration tests will go here
    // They should be gated behind an env var (LOOM_LINEAR_API_KEY)
}
