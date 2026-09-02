use super::*;
use std::io::Write;
use std::net::TcpListener;
use tempfile::TempDir;

fn loopback_listener() -> Option<(TcpListener, u16)> {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            let port = listener.local_addr().unwrap().port();
            Some((listener, port))
        }
        Err(e) => {
            eprintln!("[skip] cannot bind loopback listener in this sandbox: {e}");
            None
        }
    }
}

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
                user_id VARCHAR(128),
                created_by VARCHAR(255),
                scope VARCHAR(255) NOT NULL DEFAULT '',
                created_at DATETIME NOT NULL,
                updated_at DATETIME NOT NULL
            )",
            // Schema matches migration 005's post-state (no DEFAULT (uuid())):
            // production INSERTs must supply `id`. Mirroring the real schema
            // here catches regressions like "add_comment doesn't bind id" that
            // would only surface after migration 005 lands.
            "CREATE TABLE comments (
                id CHAR(36) NOT NULL PRIMARY KEY,
                issue_id VARCHAR(128) NOT NULL,
                text TEXT NOT NULL,
                author VARCHAR(128) NOT NULL,
                created_at DATETIME NOT NULL,
                edited_at DATETIME NULL,
                edit_reason TEXT NULL,
                original_text TEXT NULL,
                deleted_at DATETIME NULL,
                delete_reason TEXT NULL
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
        .create_bead_full(crate::store::NewBead {
            id: "vis-1".to_string(),
            title: "Cross-session visibility".to_string(),
            description: "Should survive reconnect".to_string(),
            priority: 1,
            issue_type: "bug".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    drop(client_a);

    // Session B: completely new pool — must see the bead
    let client_b = sandbox.fresh_client().await;
    let found = client_b
        .search_beads("Cross-session", "test", 50)
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

/// The Dolt reader must project `acceptance_criteria`. It silently didn't (its
/// SELECTs omitted the column while the row-mapper `try_get`'d it → ""), so a
/// Dolt→SQLite migration dropped EVERY close condition and `verify` couldn't
/// see it (lossy source == lossy target). This guards that regression:
/// write a close condition, read it back via BOTH get_bead and list_all_beads.
#[tokio::test]
async fn dolt_reader_projects_acceptance_criteria() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };
    let client = sandbox.fresh_client().await;
    client
        .create_bead_full(crate::store::NewBead {
            id: "ac-dolt-1".into(),
            title: "has a close condition".into(),
            description: String::new(),
            priority: 1,
            issue_type: "bug".into(),
            owner: String::new(),
            files: vec![],
            test_files: vec![],
            depends_on: vec![],
            created_by: None,
            scope: String::new(),
            derived_from: vec![],
            acceptance_criteria: "cargo test dolt green".into(),
        })
        .await
        .unwrap();

    let got = client.get_bead("ac-dolt-1", "test").await.unwrap().unwrap();
    assert_eq!(
        got.acceptance_criteria, "cargo test dolt green",
        "get_bead must project acceptance_criteria (migration data-loss guard)"
    );
    let listed = client.list_all_beads("test").await.unwrap();
    let l = listed.iter().find(|b| b.id == "ac-dolt-1").unwrap();
    assert_eq!(
        l.acceptance_criteria, "cargo test dolt green",
        "list_all_beads must agree with get_bead"
    );
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
        .create_bead_full(crate::store::NewBead {
            id: "wp-1".to_string(),
            title: "Write paths test".to_string(),
            description: "desc".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
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
    assert_eq!(bead.status, "done", "close_bead must auto_commit");
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
        .create_bead_full(crate::store::NewBead {
            id: "reconnect-1".to_string(),
            title: "Reconnect test".to_string(),
            description: "must survive".to_string(),
            priority: 1,
            issue_type: "bug".to_string(),
            ..Default::default()
        })
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

/// Regression: dead known ports must be cleaned so connect can auto-start
/// instead of timing out on a stale sidecar forever.
#[tokio::test]
async fn config_cleans_dead_known_port() {
    let tmp = TempDir::new().unwrap();
    let beads = tmp.path();

    // Write a port file pointing to a port nothing is listening on
    std::fs::write(beads.join("dolt-server.port"), "19999").unwrap();
    std::fs::write(beads.join("metadata.json"), r#"{"dolt_database": "beads"}"#).unwrap();

    // Create the dolt data dir so auto-start would try to use it
    let dolt_dir = beads.join("dolt").join("beads");
    std::fs::create_dir_all(&dolt_dir).unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.port, 0);
    assert!(
        !beads.join("dolt-server.port").exists(),
        "dead known port should be removed so connect can auto-start"
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
fn parse_sql_server_info_file() {
    assert_eq!(
        parse_sql_server_info("3854:51052:cb34dcbc-7f75-4e4a-90d7-3a47c2e5df1f"),
        Some((3854, 51052))
    );
    assert_eq!(parse_sql_server_info("not-a-server"), None);
}

#[test]
fn parse_dolt_config_prefers_live_sql_server_info_over_stale_sidecar() {
    let dir = TempDir::new().unwrap();
    let beads = dir.path();
    let db_dir = beads.join("dolt").join("rosary");
    let dot_dolt = db_dir.join(".dolt");
    std::fs::create_dir_all(&dot_dolt).unwrap();

    std::fs::write(
        beads.join("metadata.json"),
        r#"{"dolt_database": "rosary"}"#,
    )
    .unwrap();
    std::fs::write(beads.join("dolt-server.pid"), "999999").unwrap();
    std::fs::write(beads.join("dolt-server.port"), "64628").unwrap();

    let Some((_listener, port)) = loopback_listener() else {
        return;
    };
    let live_pid = std::process::id();
    std::fs::write(
        dot_dolt.join("sql-server.info"),
        format!("{live_pid}:{port}:test-server-uuid"),
    )
    .unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.database, "rosary");
    assert_eq!(config.port, port);
    assert_eq!(
        std::fs::read_to_string(beads.join("dolt-server.pid")).unwrap(),
        live_pid.to_string()
    );
    assert_eq!(
        std::fs::read_to_string(beads.join("dolt-server.port")).unwrap(),
        port.to_string()
    );
}

#[test]
fn parse_dolt_config_prefers_live_root_sql_server_info_over_stale_database_info() {
    let dir = TempDir::new().unwrap();
    let beads = dir.path();
    let root_dot_dolt = beads.join("dolt").join(".dolt");
    let db_dot_dolt = beads.join("dolt").join("rosary").join(".dolt");
    std::fs::create_dir_all(&root_dot_dolt).unwrap();
    std::fs::create_dir_all(&db_dot_dolt).unwrap();

    std::fs::write(
        beads.join("metadata.json"),
        r#"{"dolt_database": "rosary"}"#,
    )
    .unwrap();
    std::fs::write(beads.join("dolt-server.pid"), "3854").unwrap();
    std::fs::write(beads.join("dolt-server.port"), "51052").unwrap();

    let Some((_listener, port)) = loopback_listener() else {
        return;
    };
    let live_pid = std::process::id();
    std::fs::write(
        root_dot_dolt.join("sql-server.info"),
        format!("{live_pid}:{port}:root-server-uuid"),
    )
    .unwrap();
    std::fs::write(
        db_dot_dolt.join("sql-server.info"),
        format!("{live_pid}:51052:stale-db-server-uuid"),
    )
    .unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.database, "rosary");
    assert_eq!(config.port, port);
    assert_eq!(
        std::fs::read_to_string(beads.join("dolt-server.pid")).unwrap(),
        live_pid.to_string()
    );
    assert_eq!(
        std::fs::read_to_string(beads.join("dolt-server.port")).unwrap(),
        port.to_string()
    );
}

#[test]
fn parse_dolt_config_ignores_alive_pid_when_tcp_port_is_closed() {
    let dir = TempDir::new().unwrap();
    let beads = dir.path();
    std::fs::create_dir(beads.join("dolt")).unwrap();

    std::fs::write(
        beads.join("dolt-server.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    std::fs::write(beads.join("dolt-server.port"), "9").unwrap();

    let config = DoltConfig::from_beads_dir(beads).unwrap();
    assert_eq!(config.port, 0);
    assert!(
        !beads.join("dolt-server.port").exists(),
        "closed port should be cleaned so connect can auto-start"
    );
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
        .create_bead_full(crate::store::NewBead {
            id: test_id.to_string(),
            title: "Test CRUD bead".to_string(),
            description: "Integration test description".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

    // Verify created
    let bead = client.get_bead(&test_id, "test").await.unwrap();
    assert!(bead.is_some(), "bead should exist after creation");
    let bead = bead.unwrap();
    assert_eq!(bead.title, "Test CRUD bead");
    assert_eq!(bead.status, "open");

    // Search
    let results = client.search_beads("CRUD bead", "test", 50).await.unwrap();
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

    // Verify closed (canonical terminal form, rosary-44eec8 gap 4)
    let bead = client.get_bead(&test_id, "test").await.unwrap();
    assert!(bead.is_some());
    assert_eq!(bead.unwrap().status, "done");
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
        .create_bead_full(crate::store::NewBead {
            id: "mw-1".to_string(),
            title: "Human vs agent task delineation".to_string(),
            description: "How humans and agents split work".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .create_bead_full(crate::store::NewBead {
            id: "mw-2".to_string(),
            title: "Pure automation pipeline".to_string(),
            description: "No involvement at all".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .create_bead_full(crate::store::NewBead {
            id: "mw-3".to_string(),
            title: "Agent routing logic".to_string(),
            description: "Human review step included".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    drop(client);

    let client = sandbox.fresh_client().await;

    // "human agent" — words are non-contiguous in title of mw-1
    let results = client
        .search_beads("human agent", "test", 50)
        .await
        .unwrap();
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
    let results = client.search_beads("pipeline", "test", 50).await.unwrap();
    assert!(
        results.iter().any(|b| b.id == "mw-2"),
        "single word search should still work"
    );

    // Empty query returns all beads
    let results = client.search_beads("", "test", 50).await.unwrap();
    assert!(results.len() >= 3, "empty query should return all beads");
}

/// Regression for the migration 005 INSERT-time id contract.
///
/// Sandbox schema mirrors migration 005 output (no `DEFAULT (uuid())`),
/// so a missing client-side id would surface as an INSERT failure here
/// rather than silently passing only in dev. We assert:
/// - add_comment succeeds against the no-default schema
/// - the persisted row has a UUID-shaped id (36 chars, four `-`)
/// - list_comments retrieves the same id verbatim
#[tokio::test]
async fn add_comment_supplies_uuid_id() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    let writer = sandbox.fresh_client().await;
    writer
        .create_bead_full(crate::store::NewBead {
            id: "uuid-1".to_string(),
            title: "test for uuid id".to_string(),
            description: "".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    writer
        .add_comment("uuid-1", "first comment body", "test-runner")
        .await
        .expect("add_comment must succeed against migration-005 schema (no DEFAULT (uuid()))");
    drop(writer);

    let reader = sandbox.fresh_client().await;
    let comments = reader.list_comments("uuid-1", false).await.unwrap();
    assert_eq!(comments.len(), 1, "exactly one comment");
    let comment = &comments[0];
    // UUID v4 hex form is 36 chars: 8-4-4-4-12 with 4 hyphens.
    assert_eq!(
        comment.id.len(),
        36,
        "expected UUID-shaped id (36 chars), got: {:?}",
        comment.id
    );
    assert_eq!(
        comment.id.matches('-').count(),
        4,
        "expected UUID-shaped id (4 hyphens), got: {:?}",
        comment.id
    );
    // Bytewise the id should round-trip through uuid::Uuid parsing.
    uuid::Uuid::parse_str(&comment.id).expect("id must parse as a UUID");
}

// ── Migration repair (rosary-b61362) ────────────────────

/// Hard-mark a migration as applied for repair-path tests. Uses a direct
/// INSERT instead of `log_event` (which is best-effort and only prints
/// warnings on failure) so the test cannot false-pass by routing through
/// the normal apply path when the marker silently failed to land.
async fn force_mark_migration_applied(client: &DoltClient, version: &str) {
    client
        .execute_raw(&format!(
            "INSERT INTO events (issue_id, event_type, actor, comment, created_at) \
             VALUES ('_schema', 'migration', 'test-runner', '{version}', NOW())"
        ))
        .await
        .expect("force-mark migration applied must succeed for repair-path test setup");

    // Belt-and-suspenders: also assert the row is queryable, since a
    // silent insert into a misconfigured events table would also let the
    // test through the wrong path.
    let row = sqlx_core::query::query(
        "SELECT COUNT(*) as cnt FROM events WHERE event_type = 'migration' AND comment = ?",
    )
    .bind(version)
    .fetch_one(&client.pool)
    .await
    .expect("count migration marker rows");
    let count: i64 = sqlx_core::row::Row::try_get(&row, "cnt").unwrap_or(0);
    assert!(
        count > 0,
        "test setup invariant: migration marker for `{version}` must be present \
         in events table; got count={count}"
    );
}

/// Reproduces the `rosary-b61362` symptom and pins the auto-repair contract:
/// when migration 005 is marked applied (via `_schema` event) but the live
/// schema is missing one of the audit columns, `migrate()` MUST repair the
/// schema rather than emit a silent warning.
///
/// Pre-fix behavior: the verify-on-already-applied path printed two
/// `[migrate] WARNING:` lines and continued; the column stayed missing
/// across every subsequent CLI invocation, and every `bead comment delete
/// --reason` write would fail at the write boundary.
///
/// Post-fix behavior: verify failure on an already-applied migration
/// triggers an idempotent re-apply (duplicate-column errors tolerated by
/// the apply loop's `already_applied` heuristic), followed by a second
/// verify that loud-fails if the schema is still wrong.
#[tokio::test]
#[ignore = "rosary-4de4b8: load-flaky under full-suite parallelism + coverage \
            instrumentation — multiple real dolt servers contend and the repair \
            timing races. Run explicitly with `cargo test -- --ignored`. Keeps \
            the coverage net (rosary-69fdde) and CI reliable; deterministic-repair \
            fix tracked in rosary-4de4b8."]
async fn migrate_repairs_partial_005_when_marked_applied() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    let client = sandbox.fresh_client().await;

    // Simulate the broken state: drop `delete_reason` while leaving the
    // rest of the migration-005 columns in place — exactly the shape the
    // bug report observed in production.
    client
        .execute_raw("ALTER TABLE comments DROP COLUMN delete_reason")
        .await
        .expect("drop delete_reason for test setup");

    // Mark migration 005 as applied via a direct INSERT (not log_event,
    // which is best-effort and would let the test false-pass through the
    // normal apply path if the marker silently failed to land).
    force_mark_migration_applied(&client, "005_comments_audit_columns").await;

    // Sanity-check the broken state: verify SQL must reject the schema as
    // it stands. Without this assert, a passing migrate() could pass for
    // the wrong reason (e.g. the test setup didn't actually drop the
    // column).
    let verify_sql = "SELECT id, edited_at, edit_reason, original_text, \
                      deleted_at, delete_reason FROM comments LIMIT 0";
    client
        .execute_raw(verify_sql)
        .await
        .expect_err("verify SQL must reject the partial-applied schema");

    // Action under test: migrate() observes 005 marked applied + verify
    // failing, and repairs the schema.
    client
        .migrate()
        .await
        .expect("migrate() must succeed on a repairable partial-apply");

    // The audit columns must exist again after the repair.
    client
        .execute_raw(verify_sql)
        .await
        .expect("verify SQL must accept the repaired schema");
}

/// Pins the loud-fail contract for unrepairable partial-applies. When
/// the schema is in a state the migration's own SQL cannot fix (here:
/// the entire `comments` table is gone), `migrate()` must return Err
/// naming the migration version rather than silently continuing past a
/// known-broken schema.
#[tokio::test]
async fn migrate_errors_when_repair_cannot_restore_schema() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    let client = sandbox.fresh_client().await;

    // Mark 005 applied via a direct INSERT (same robustness reason as the
    // repair-path test), then drop the entire comments table so re-apply
    // surfaces a non-already-applied error and propagates Err.
    force_mark_migration_applied(&client, "005_comments_audit_columns").await;
    client
        .execute_raw("DROP TABLE comments")
        .await
        .expect("drop comments table for test setup");

    let err = client
        .migrate()
        .await
        .expect_err("migrate() must Err when repair cannot restore the schema");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("005_comments_audit_columns"),
        "error must name the failing migration; got: {msg}"
    );
}

/// rosary-dc0a9f: a bd-created (richer) `dependencies` schema uses bd's `type`
/// column plus a NOT-NULL `created_by` with no default. Before the fix,
/// `add_dependency_typed` errored ("Field 'created_by' doesn't have a default
/// value") and `get_children` read a phantom `dep_type` — so the containment
/// gate was blind on exactly the bd stores rosary interoperates with. This pins
/// both: the insert supplies created_by + writes bd's `type`, and get_children
/// reads it back.
#[tokio::test]
async fn typed_dep_works_on_bd_richer_schema() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };
    let client = sandbox.fresh_client().await;

    // Replace rosary's minimal dependencies table with a bd-shaped one:
    // bd's `type` column, NOT-NULL `created_by` (no default), and no `dep_type`.
    client
        .execute_raw("DROP TABLE dependencies")
        .await
        .expect("drop dependencies for bd-schema setup");
    client
        .execute_raw(
            "CREATE TABLE dependencies (
                issue_id VARCHAR(128) NOT NULL,
                depends_on_id VARCHAR(128) NOT NULL,
                `type` VARCHAR(32) NOT NULL DEFAULT 'blocks',
                created_at DATETIME NOT NULL DEFAULT NOW(),
                created_by VARCHAR(255) NOT NULL,
                thread_id VARCHAR(255) DEFAULT '',
                PRIMARY KEY (issue_id, depends_on_id)
            )",
        )
        .await
        .expect("create bd-shaped dependencies table");

    client
        .create_bead_full(crate::store::NewBead {
            id: "parent".to_string(),
            title: "Parent".to_string(),
            description: "".to_string(),
            priority: 1,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .create_bead_full(crate::store::NewBead {
            id: "child".to_string(),
            title: "Child".to_string(),
            description: "".to_string(),
            priority: 1,
            issue_type: "task".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

    // The exact dc0a9f failure: used to error on the missing created_by default.
    client
        .add_dependency_typed("child", "parent", "parent-child")
        .await
        .expect("add_dependency_typed must work on a bd-richer schema");

    // get_children reads bd's `type` (not a phantom dep_type) → sees the edge.
    // This one assertion proves both write-to-`type` and read-from-`type`: if
    // the value had gone anywhere else, get_children would return empty.
    let kids = client.get_children("parent").await.unwrap();
    assert_eq!(kids, vec!["child".to_string()]);
}

/// `migrate()` must be idempotent across repeated invocations: after a
/// clean first run, the second call exercises the verify-on-already-applied
/// path for migrations both with and without a verify clause (005 has one;
/// 001/003/004/006 don't), and must return Ok with no surprises.
///
/// This pins both early-return arms of `verify_or_repair`: the no-verify
/// short-circuit and the verify-passes short-circuit.
#[tokio::test]
async fn migrate_is_idempotent_across_invocations() {
    let sandbox = match SandboxBeads::new().await {
        Some(s) => s,
        None => return,
    };

    let client = sandbox.fresh_client().await;
    client
        .migrate()
        .await
        .expect("first migrate() succeeds against the sandbox post-005 schema");

    // Second invocation: every migration is recorded as applied. 005 has
    // verify=Some and the schema is intact, so verify_or_repair takes the
    // verify-passes short-circuit. 001/003/004/006 have verify=None and
    // take the no-verify short-circuit.
    let applied = client
        .migrate()
        .await
        .expect("second migrate() must succeed without re-applying anything");
    assert!(
        applied.is_empty(),
        "second migrate() must report zero applied migrations; got: {applied:?}"
    );
}

// ---------------------------------------------------------------------------
// Cross-backend guardrail parity (rosary-44eec8)
//
// Each test drives the SAME sequence through both `BeadStore` impls —
// SqliteBeadStore (pure, always runs) and DoltBeadStore (SandboxBeads,
// self-skips without a dolt binary) — and asserts identical guarantees.
// These pin the gap set from the rosary-46e7ff parity matrix; before this
// suite, the headline update_status divergence was unpinned by any test on
// either backend.
// ---------------------------------------------------------------------------

async fn sqlite_store_for_parity() -> crate::bead_sqlite::SqliteBeadStore {
    crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap()
}

/// Both backends must reject a transition the state machine forbids, with the
/// same error shape, leaving status untouched. Dolt previously accepted ANY
/// transition (raw UPDATE, no can_transition_to — bead_crud.rs:271).
#[tokio::test]
async fn parity_update_status_rejects_invalid_transition_on_both_backends() {
    use crate::store::BeadStore;

    // SQLite half — always runs.
    let sq = sqlite_store_for_parity().await;
    sq.create_bead("par-inv-1", "t", "d", 2, "task")
        .await
        .unwrap();
    sq.update_status("par-inv-1", "dispatched").await.unwrap();
    let err = sq
        .update_status("par-inv-1", "done")
        .await
        .expect_err("SQLite must reject dispatched -> done");
    assert!(
        err.to_string().contains("invalid state transition"),
        "{err}"
    );
    assert_eq!(
        sq.get_status("par-inv-1").await.unwrap().as_deref(),
        Some("dispatched"),
        "rejected transition must not mutate status"
    );

    // Dolt half — self-skips without dolt.
    let Some(sandbox) = SandboxBeads::new().await else {
        return;
    };
    let dolt = crate::bead_dolt::DoltBeadStore::new(sandbox.fresh_client().await);
    dolt.create_bead("par-inv-2", "t", "d", 2, "task")
        .await
        .unwrap();
    dolt.update_status("par-inv-2", "dispatched").await.unwrap();
    let err = dolt
        .update_status("par-inv-2", "done")
        .await
        .expect_err("Dolt must reject dispatched -> done exactly like SQLite");
    assert!(
        err.to_string().contains("invalid state transition"),
        "{err}"
    );
    assert_eq!(
        dolt.get_status("par-inv-2").await.unwrap().as_deref(),
        Some("dispatched"),
        "rejected transition must not mutate status on Dolt"
    );
}

/// The bead_correct escape hatch (rosary-e0e19f): set_status_verbatim must
/// KEEP bypassing the transition table on both backends. A guardrail that
/// closes this reintroduces the unrecoverable-terminal-state incident.
#[tokio::test]
async fn parity_set_status_verbatim_bypasses_transitions_on_both_backends() {
    use crate::store::BeadStore;

    let sq = sqlite_store_for_parity().await;
    sq.create_bead("par-vrb-1", "t", "d", 2, "task")
        .await
        .unwrap();
    sq.set_status_verbatim("par-vrb-1", "done")
        .await
        .expect("verbatim write must bypass the transition table (SQLite)");
    assert_eq!(
        sq.get_status("par-vrb-1").await.unwrap().as_deref(),
        Some("done")
    );

    let Some(sandbox) = SandboxBeads::new().await else {
        return;
    };
    let dolt = crate::bead_dolt::DoltBeadStore::new(sandbox.fresh_client().await);
    dolt.create_bead("par-vrb-2", "t", "d", 2, "task")
        .await
        .unwrap();
    dolt.set_status_verbatim("par-vrb-2", "done")
        .await
        .expect("verbatim write must bypass the transition table (Dolt)");
    assert_eq!(
        dolt.get_status("par-vrb-2").await.unwrap().as_deref(),
        Some("done")
    );
}

/// Secret scrubbing must hold on BOTH backends for every text write path:
/// create (title/description), comment add/update, and field updates.
/// Previously Dolt-only (create + comments) and absent everywhere for
/// update_bead_fields — the DEFAULT backend wrote secrets verbatim into the
/// store and the git-tracked JSONL projection.
#[tokio::test]
async fn parity_secrets_scrubbed_on_both_backends() {
    use crate::bead::BeadUpdate;
    use crate::store::BeadStore;
    // A GitHub classic PAT shape the scrubber recognizes (36+ chars after ghp_).
    let leak = "token ghp_0123456789abcdefghijABCDEFGHIJ0123456789 end";

    async fn assert_scrubbed(store: &dyn BeadStore, tag: &str) {
        let id = format!("par-scrub-{tag}");
        store
            .create_bead_full(crate::store::NewBead {
                id: id.clone(),
                title: format!(
                    "t {leak}",
                    leak = "ghp_0123456789abcdefghijABCDEFGHIJ0123456789"
                ),
                description: "token ghp_0123456789abcdefghijABCDEFGHIJ0123456789 end".into(),
                priority: 2,
                issue_type: "task".into(),
                owner: String::new(),
                files: vec![],
                test_files: vec![],
                depends_on: vec![],
                created_by: None,
                scope: String::new(),
                derived_from: vec![],
                acceptance_criteria: String::new(),
            })
            .await
            .unwrap();
        let bead = store.get_bead(&id, "r").await.unwrap().unwrap();
        assert!(
            !bead.title.contains("ghp_0123456789"),
            "title scrubbed [{tag}]: {}",
            bead.title
        );
        assert!(
            !bead.description.contains("ghp_0123456789"),
            "description scrubbed [{tag}]"
        );

        store
            .add_comment(
                &id,
                "note ghp_0123456789abcdefghijABCDEFGHIJ0123456789",
                "tester",
            )
            .await
            .unwrap();
        let comments = store.list_comments(&id, false).await.unwrap();
        assert!(
            !comments.last().unwrap().text.contains("ghp_0123456789"),
            "comment add scrubbed [{tag}]"
        );

        let cid = comments.last().unwrap().id.clone();
        store
            .update_comment(
                &cid,
                "edit ghp_0123456789abcdefghijABCDEFGHIJ0123456789",
                None,
            )
            .await
            .unwrap();
        let comments = store.list_comments(&id, false).await.unwrap();
        assert!(
            !comments.last().unwrap().text.contains("ghp_0123456789"),
            "comment update scrubbed [{tag}]"
        );

        store
            .update_bead_fields(
                &id,
                &BeadUpdate {
                    description: Some("upd ghp_0123456789abcdefghijABCDEFGHIJ0123456789".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let bead = store.get_bead(&id, "r").await.unwrap().unwrap();
        assert!(
            !bead.description.contains("ghp_0123456789"),
            "update_bead_fields scrubbed [{tag}]"
        );
    }
    let _ = leak; // fixture documented above; inlined per call to keep rows independent

    let sq = sqlite_store_for_parity().await;
    assert_scrubbed(&sq, "sq").await;

    let Some(sandbox) = SandboxBeads::new().await else {
        return;
    };
    let dolt = crate::bead_dolt::DoltBeadStore::new(sandbox.fresh_client().await);
    assert_scrubbed(&dolt, "dolt").await;
}

/// close_bead must persist the CANONICAL terminal form on both backends.
/// Both previously stored the alias 'closed'; SQLite healed it on the next
/// connect (canonicalize_statuses) but Dolt never did, so the Linear close
/// sweep (list_closed_linked_beads) saw divergent result sets over time.
#[tokio::test]
async fn parity_close_bead_stores_canonical_done_on_both_backends() {
    use crate::store::BeadStore;

    let sq = sqlite_store_for_parity().await;
    sq.create_bead("par-cls-1", "t", "d", 2, "task")
        .await
        .unwrap();
    sq.close_bead("par-cls-1").await.unwrap();
    assert_eq!(
        sq.get_status("par-cls-1").await.unwrap().as_deref(),
        Some("done"),
        "SQLite close_bead must store canonical 'done', not the alias"
    );

    let Some(sandbox) = SandboxBeads::new().await else {
        return;
    };
    let dolt = crate::bead_dolt::DoltBeadStore::new(sandbox.fresh_client().await);
    dolt.create_bead("par-cls-2", "t", "d", 2, "task")
        .await
        .unwrap();
    dolt.close_bead("par-cls-2").await.unwrap();
    assert_eq!(
        dolt.get_status("par-cls-2").await.unwrap().as_deref(),
        Some("done"),
        "Dolt close_bead must store canonical 'done', not the alias"
    );
}

/// get_status must resolve short ids on both backends. Dolt's raw
/// `WHERE id = ?` (query.rs) returned None for a known bead queried by
/// suffix, while its own close_bead resolves first — an omission, not a
/// missing capability.
#[tokio::test]
async fn parity_get_status_resolves_short_ids_on_both_backends() {
    use crate::store::BeadStore;

    let sq = sqlite_store_for_parity().await;
    sq.create_bead("rsry-abc123", "t", "d", 2, "task")
        .await
        .unwrap();
    assert_eq!(
        sq.get_status("abc123").await.unwrap().as_deref(),
        Some("open"),
        "SQLite resolves the short id"
    );

    let Some(sandbox) = SandboxBeads::new().await else {
        return;
    };
    let dolt = crate::bead_dolt::DoltBeadStore::new(sandbox.fresh_client().await);
    dolt.create_bead("rsry-def456", "t", "d", 2, "task")
        .await
        .unwrap();
    assert_eq!(
        dolt.get_status("def456").await.unwrap().as_deref(),
        Some("open"),
        "Dolt must resolve the short id like SQLite (and like its own close_bead)"
    );
}
