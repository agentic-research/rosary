use super::*;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

/// Sandboxed Dolt beads database for integration testing.
///
/// Spins up a fresh Dolt instance in a temp directory with the beads schema,
/// then kills the server on drop. Each `fresh_client()` call returns a new
/// connection pool — simulating an MCP reconnect.
struct SandboxBeads {
    config: DoltConfig,
    _tmp: TempDir,
}

impl SandboxBeads {
    async fn new() -> Option<Self> {
        if std::process::Command::new("dolt")
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping: dolt not installed");
            return None;
        }

        let tmp = TempDir::new().unwrap();
        let beads_dir = tmp.path();
        let db_dir = beads_dir.join("dolt").join("beads");
        std::fs::create_dir_all(&db_dir).unwrap();

        // Initialize dolt database
        let status = std::process::Command::new("dolt")
            .args(["init"])
            .current_dir(&db_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("dolt init");
        assert!(status.success(), "dolt init failed");

        std::fs::write(
            beads_dir.join("metadata.json"),
            r#"{"dolt_database": "beads"}"#,
        )
        .unwrap();

        // port=0 → connect() will auto-start
        let config = DoltConfig::from_beads_dir(beads_dir).unwrap();
        let client = DoltClient::connect(&config).await.unwrap();

        // Create beads schema
        for sql in [
            "CREATE TABLE issues (
                id VARCHAR(128) PRIMARY KEY,
                title VARCHAR(512) NOT NULL,
                description TEXT,
                design TEXT DEFAULT '',
                acceptance_criteria TEXT DEFAULT '',
                notes TEXT DEFAULT '',
                status VARCHAR(32) NOT NULL DEFAULT 'open',
                priority INT NOT NULL DEFAULT 2,
                issue_type VARCHAR(32) NOT NULL DEFAULT 'task',
                assignee VARCHAR(128),
                external_ref VARCHAR(128),
                created_at DATETIME NOT NULL,
                updated_at DATETIME NOT NULL
            )",
            "CREATE TABLE comments (
                issue_id VARCHAR(128) NOT NULL,
                text TEXT NOT NULL,
                author VARCHAR(128) NOT NULL,
                created_at DATETIME NOT NULL
            )",
            "CREATE TABLE dependencies (
                issue_id VARCHAR(128) NOT NULL,
                depends_on_id VARCHAR(128) NOT NULL,
                PRIMARY KEY (issue_id, depends_on_id)
            )",
            "CREATE TABLE events (
                issue_id VARCHAR(128) NOT NULL,
                event_type VARCHAR(64) NOT NULL,
                actor VARCHAR(128) NOT NULL,
                comment TEXT,
                created_at DATETIME NOT NULL
            )",
        ] {
            client.execute_raw(sql).await.unwrap();
        }

        // Commit schema so it's visible to all future connections
        client
            .execute_raw("CALL DOLT_COMMIT('-Am', 'init schema', '--allow-empty')")
            .await
            .unwrap();

        // Re-read config to pick up the port written by auto-start
        let config = DoltConfig::from_beads_dir(beads_dir).unwrap();
        Some(SandboxBeads { config, _tmp: tmp })
    }

    /// Each call returns a fresh connection pool — simulates MCP reconnect.
    async fn fresh_client(&self) -> DoltClient {
        DoltClient::connect(&self.config).await.unwrap()
    }
}

impl Drop for SandboxBeads {
    fn drop(&mut self) {
        let pid_file = self.config.beads_dir.join("dolt-server.pid");
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid_str.trim().parse::<i32>()
        {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

// ── Sandboxed cross-connection tests ────────────────────

/// The exact bug scenario: bead created on connection A must be
/// findable from a completely new connection B (simulating MCP reconnect).
#[tokio::test]
async fn create_bead_visible_to_new_connection() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    // Session A: create a bead
    let client_a = sandbox.fresh_client().await;
    client_a
        .create_bead(
            "vis-1",
            "Cross-session visibility",
            "Should survive reconnect",
            1,
            "bug",
        )
        .await
        .unwrap();
    drop(client_a);

    // Session B: completely new pool — must see the bead
    let client_b = sandbox.fresh_client().await;
    let found = client_b
        .search_beads("Cross-session", "test")
        .await
        .unwrap();
    assert!(
        found.iter().any(|b| b.id == "vis-1"),
        "bead created in session A must be visible to session B (auto_commit guarantees this)"
    );

    let bead = client_b.get_bead("vis-1", "test").await.unwrap();
    assert!(bead.is_some());
    assert_eq!(bead.unwrap().title, "Cross-session visibility");
}

/// Every write path must auto-commit: update_status, close_bead,
/// add_comment, update_bead_fields. Verified by checking from a fresh connection.
#[tokio::test]
async fn all_write_paths_visible_across_connections() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    // Setup: create bead
    let setup = sandbox.fresh_client().await;
    setup
        .create_bead("wp-1", "Write paths test", "desc", 2, "task")
        .await
        .unwrap();
    drop(setup);

    // update_status
    let writer = sandbox.fresh_client().await;
    writer.update_status("wp-1", "in_progress").await.unwrap();
    drop(writer);

    let reader = sandbox.fresh_client().await;
    let status = reader.get_status("wp-1").await.unwrap();
    assert_eq!(
        status.as_deref(),
        Some("in_progress"),
        "update_status must auto_commit"
    );
    drop(reader);

    // add_comment
    let writer = sandbox.fresh_client().await;
    writer
        .add_comment("wp-1", "test comment", "test-runner")
        .await
        .unwrap();
    drop(writer);

    let reader = sandbox.fresh_client().await;
    let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
    assert_eq!(bead.comment_count, 1, "add_comment must auto_commit");
    drop(reader);

    // update_bead_fields (PATCH)
    let writer = sandbox.fresh_client().await;
    let update = crate::bead::BeadUpdate {
        title: Some("Updated title".into()),
        ..Default::default()
    };
    writer.update_bead_fields("wp-1", &update).await.unwrap();
    drop(writer);

    let reader = sandbox.fresh_client().await;
    let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
    assert_eq!(
        bead.title, "Updated title",
        "update_bead_fields must auto_commit"
    );
    drop(reader);

    // close_bead
    let writer = sandbox.fresh_client().await;
    writer.close_bead("wp-1").await.unwrap();
    drop(writer);

    let reader = sandbox.fresh_client().await;
    let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
    assert_eq!(bead.status, "closed", "close_bead must auto_commit");
}

/// Regression: when a port file exists, reconnecting must use THAT server,
/// not auto-start a fresh empty one. This was the root cause of beads
/// "disappearing" after /mcp reconnect — rsry connected to a new empty DB.
#[tokio::test]
async fn reconnect_uses_existing_server_not_fresh() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    // Create a bead on the original server
    let client = sandbox.fresh_client().await;
    client
        .create_bead("reconnect-1", "Reconnect test", "must survive", 1, "bug")
        .await
        .unwrap();
    drop(client);

    // Simulate /mcp reconnect: fresh_client reads the SAME port file
    // and must connect to the SAME server, finding the bead.
    let client2 = sandbox.fresh_client().await;
    let found = client2.get_bead("reconnect-1", "test").await.unwrap();
    assert!(
        found.is_some(),
        "bead created before reconnect must be visible after reconnect — \
         if this fails, connect() is auto-starting a fresh empty server \
         instead of using the existing one on port {}",
        sandbox.config.port
    );
    assert_eq!(found.unwrap().title, "Reconnect test");
}

/// Regression: connect must bail (not auto-start) when port file points
/// to a dead server.
#[tokio::test]
async fn connect_fails_when_known_port_dead() {
    let tmp = TempDir::new().unwrap();
    let beads = tmp.path();

    // Write a port file pointing to a port nothing is listening on
    std::fs::write(beads.join("dolt-server.port"), "19999").unwrap();
    std::fs::write(beads.join("metadata.json"), r#"{"dolt_database": "beads"}"#).unwrap();

    // Create the dolt data dir so auto-start would try to use it
    let dolt_dir = beads.join("dolt").join("beads");
    std::fs::create_dir_all(&dolt_dir).unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.port, 19999);

    let result = DoltClient::connect(&config).await;
    assert!(
        result.is_err(),
        "connect must FAIL when port file exists but server is dead — \
         not silently auto-start a fresh empty server"
    );
}

// ── Existing tests ──────────────────────────────────────

#[test]
fn parse_dolt_config_from_beads_dir() {
    let dir = TempDir::new().unwrap();
    let beads = dir.path();

    // Write port file
    let mut port_file = std::fs::File::create(beads.join("dolt-server.port")).unwrap();
    write!(port_file, "60621").unwrap();

    // Write metadata
    std::fs::write(
        beads.join("metadata.json"),
        r#"{"dolt_database": "mache", "project_id": "abc-123"}"#,
    )
    .unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.host, "127.0.0.1");
    assert_eq!(config.port, 60621);
    assert_eq!(config.database, "mache");
    assert_eq!(config.url(), "mysql://root@127.0.0.1:60621/mache");
}

#[test]
fn parse_dolt_config_missing_metadata_defaults_to_beads() {
    let dir = TempDir::new().unwrap();
    let beads = dir.path();

    std::fs::write(beads.join("dolt-server.port"), "3306").unwrap();
    // No metadata.json

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.database, "beads");
    assert_eq!(config.port, 3306);
}

#[test]
fn parse_dolt_config_no_port_file_returns_port_zero() {
    let dir = TempDir::new().unwrap();
    let config = DoltConfig::from_beads_dir(dir.path()).unwrap();
    assert_eq!(config.port, 0); // No server — auto-start will handle it
}

#[test]
fn parse_dolt_config_bad_port_errors() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("dolt-server.port"), "not-a-number").unwrap();
    let result = DoltConfig::from_beads_dir(dir.path());
    assert!(result.is_err());
}

/// Integration test — only runs when a real Dolt server is available.
/// Set RSRY_TEST_BEADS_DIR to a .beads/ directory with a running server.
#[tokio::test]
async fn list_beads_from_live_dolt() {
    let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
            return;
        }
    };

    let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
    let client = DoltClient::connect(&config).await.unwrap();
    let beads = client.list_beads("test-repo").await.unwrap();

    // Should get at least one bead from a real database
    assert!(!beads.is_empty(), "expected beads from live Dolt server");
    for bead in &beads {
        assert!(!bead.id.is_empty());
        assert!(!bead.title.is_empty());
        assert_eq!(bead.repo, "test-repo");
    }
}

#[tokio::test]
async fn get_single_bead_from_live_dolt() {
    let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
            return;
        }
    };

    let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
    let client = DoltClient::connect(&config).await.unwrap();

    // First list to get a known ID
    let beads = client.list_beads("test").await.unwrap();
    if beads.is_empty() {
        eprintln!("skipping: no beads in database");
        return;
    }

    let id = &beads[0].id;
    let bead = client.get_bead(id, "test").await.unwrap();
    assert!(bead.is_some());
    assert_eq!(bead.unwrap().id, *id);
}

/// Integration test — creates, searches, comments, and closes a bead.
/// Only runs when a real Dolt server is available.
#[tokio::test]
async fn crud_lifecycle_live_dolt() {
    let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
            return;
        }
    };

    let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
    let client = DoltClient::connect(&config).await.unwrap();

    let test_id = format!(
        "test-crud-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    // Create
    client
        .create_bead(
            &test_id,
            "Test CRUD bead",
            "Integration test description",
            2,
            "task",
        )
        .await
        .unwrap();

    // Verify created
    let bead = client.get_bead(&test_id, "test").await.unwrap();
    assert!(bead.is_some(), "bead should exist after creation");
    let bead = bead.unwrap();
    assert_eq!(bead.title, "Test CRUD bead");
    assert_eq!(bead.status, "open");

    // Search
    let results = client.search_beads("CRUD bead", "test").await.unwrap();
    assert!(
        results.iter().any(|b| b.id == test_id),
        "search should find created bead"
    );

    // Add comment
    client
        .add_comment(&test_id, "Test comment body", "test-runner")
        .await
        .unwrap();

    // Close
    client.close_bead(&test_id).await.unwrap();

    // Verify closed
    let bead = client.get_bead(&test_id, "test").await.unwrap();
    assert!(bead.is_some());
    assert_eq!(bead.unwrap().status, "closed");
}

/// Multi-word search should match words appearing non-contiguously.
/// "human agent" must match "Human vs agent task delineation".
#[tokio::test]
async fn search_multi_word_non_contiguous() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    let client = sandbox.fresh_client().await;

    // Create beads with different title patterns
    client
        .create_bead(
            "mw-1",
            "Human vs agent task delineation",
            "How humans and agents split work",
            2,
            "task",
        )
        .await
        .unwrap();
    client
        .create_bead(
            "mw-2",
            "Pure automation pipeline",
            "No involvement at all",
            2,
            "task",
        )
        .await
        .unwrap();
    client
        .create_bead(
            "mw-3",
            "Agent routing logic",
            "Human review step included",
            2,
            "task",
        )
        .await
        .unwrap();
    drop(client);

    let client = sandbox.fresh_client().await;

    // "human agent" — words are non-contiguous in title of mw-1
    let results = client.search_beads("human agent", "test").await.unwrap();
    assert!(
        results.iter().any(|b| b.id == "mw-1"),
        "should match 'Human vs agent task delineation' (words non-contiguous in title)"
    );
    // mw-3 has "Agent" in title and "Human" in description — should also match
    assert!(
        results.iter().any(|b| b.id == "mw-3"),
        "should match when one word is in title and other in description"
    );
    // mw-2 has neither "human" nor "agent" anywhere
    assert!(
        !results.iter().any(|b| b.id == "mw-2"),
        "should NOT match 'Pure automation pipeline' — missing both search words"
    );

    // Single word search still works
    let results = client.search_beads("pipeline", "test").await.unwrap();
    assert!(
        results.iter().any(|b| b.id == "mw-2"),
        "single word search should still work"
    );

    // Empty query returns all beads
    let results = client.search_beads("", "test").await.unwrap();
    assert!(results.len() >= 3, "empty query should return all beads");
}
