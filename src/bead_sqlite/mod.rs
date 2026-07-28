//! SQLite-backed per-repo bead store.
//!
//! Each repo gets its own `.beads/beads.db` file. No server process needed.
//! Schema mirrors the Dolt `issues`/`dependencies`/`comments`/`events` tables.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::bead::{Bead, BeadUpdate};
use crate::store::{BeadStore, NewBead};

/// Parse files and test_files from the notes JSON column.
/// Shared between SQLite and Dolt implementations.
pub fn parse_files_from_notes(notes: Option<&str>) -> (Vec<String>, Vec<String>) {
    let parsed: Option<serde_json::Value> = notes.and_then(|s| serde_json::from_str(s).ok());
    let files = parsed
        .as_ref()
        .and_then(|v| v.get("files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let test_files = parsed
        .as_ref()
        .and_then(|v| v.get("test_files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    (files, test_files)
}

/// Parse derived_from provenance chain from the notes JSON column.
pub fn parse_derived_from_notes(notes: Option<&str>) -> Vec<bdr::provenance::ProvenanceRef> {
    let parsed: Option<serde_json::Value> = notes.and_then(|s| serde_json::from_str(s).ok());
    parsed
        .as_ref()
        .and_then(|v| v.get("derived_from"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Parse a stored timestamp. Accepts the store's canonical
/// `%Y-%m-%d %H:%M:%S`, the `T`-separated variant, and full **RFC3339** (which
/// carries an offset — the shape `bead export --jsonl` emits, so any external
/// producer round-tripping the contract parses too).
///
/// **Fails loud** on anything else. This previously ended in
/// `.unwrap_or_else(|_| Utc::now())`, which silently rewrote an unreadable
/// timestamp to "now": indistinguishable from a real edit, and corrosive to
/// last-writer-wins sync (rosary-4ebf52), where such a row would win every
/// comparison forever and keep re-winning on each pass. Matches the fail-loud
/// posture this reader already takes for malformed rows (rosary-91e712) —
/// better a visible error than a store that quietly invents history.
fn parse_datetime(s: &str) -> rusqlite::Result<chrono::DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .map(|ndt| Utc.from_utc_datetime(&ndt))
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&Utc)))
        .map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unparseable timestamp {s:?} in bead store"),
                )),
            )
        })
}

/// Read a Bead from a rusqlite Row.
fn bead_from_row(row: &rusqlite::Row<'_>, repo_name: &str) -> rusqlite::Result<Bead> {
    let notes: Option<String> = row.get("notes")?;
    let (files, test_files) = parse_files_from_notes(notes.as_deref());
    let derived_from = parse_derived_from_notes(notes.as_deref());
    let created_str: String = row.get("created_at")?;
    let updated_str: String = row.get("updated_at")?;

    Ok(Bead {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row
            .get::<_, Option<String>>("description")?
            .unwrap_or_default(),
        status: row.get("status")?,
        priority: row.get::<_, i32>("priority").unwrap_or(2) as u8,
        issue_type: row
            .get::<_, Option<String>>("issue_type")?
            .unwrap_or_else(|| "task".into()),
        owner: row.get("assignee")?,
        repo: repo_name.to_string(),
        created_at: parse_datetime(&created_str)?,
        updated_at: parse_datetime(&updated_str)?,
        dependency_count: row.get::<_, i64>("dependency_count").unwrap_or(0) as u32,
        dependent_count: row.get::<_, i64>("dep_count").unwrap_or(0) as u32,
        comment_count: row.get::<_, i64>("comment_count").unwrap_or(0) as u32,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: row.get("external_ref")?,
        files,
        test_files,
        created_by: row.get::<_, Option<String>>("created_by").unwrap_or(None),
        scope: row.get::<_, String>("scope").unwrap_or_default(),
        derived_from,
        acceptance_criteria: row
            .get::<_, String>("acceptance_criteria")
            .unwrap_or_default(),
    })
}

/// Connect to the bead store for a repo's `.beads/` directory.
///
/// Default: Dolt (per-repo version-controlled database — branch-per-agent,
/// cell-level merge, commit history). Falls back to SQLite if Dolt is
/// unavailable (no server, no port file) or if `beads.db` exists and Dolt
/// doesn't.
///
/// SQLite is useful for: tests, offline/lightweight repos, portable exports.
/// Open a bead store for the given `.beads/` directory.
///
/// Two paths, no fallback:
///   1. `dolt/` exists → Dolt is canonical. Rosary is a consumer of the bead
///      store, not the owner. Falling back to SQLite when Dolt is unreachable
///      creates a shadow store that diverges silently. Fail loudly instead —
///      `DoltClient::connect` already auto-starts the server if it's not running.
///   2. No `dolt/` → repo is not yet initialized; use SQLite for bootstrapping.
///
/// Before bootstrapping SQLite, [`unreadable_backend_warning`] screams if the
/// repo actually has a bd store rsry can't read (embedded Dolt), so a
/// registered repo never silently reports 0 beads (rosary-21e2d4).
///
/// Detect a bead backend rsry cannot read, so a registered repo never
/// *silently* reports 0 beads (rosary-21e2d4 — the lectio failure mode).
/// Returns a loud diagnostic only when `.beads/` holds *solely* a store rsry
/// doesn't speak — a bd embedded-Dolt store (`embeddeddolt/`) with no server-mode
/// `dolt/` and no SQLite `beads.db` fallback. If a `beads.db` is present, rsry
/// reads that (ADR-0014) and stays quiet (rosary-65c2ff).
pub(crate) fn unreadable_backend_warning(beads_dir: &Path) -> Option<String> {
    // Server-mode Dolt (`dolt/`) is readable over MySQL — not a concern.
    if beads_dir.join("dolt").is_dir() {
        return None;
    }
    // bd embedded Dolt: data rsry can't read. But if a SQLite `beads.db` is
    // present, THAT is the store rsry actually reads (ADR-0014) — embeddeddolt
    // is unused, so this is not a "0 beads / cannot read" situation. Only warn
    // when embeddeddolt is the *only* store and there's no beads.db fallback.
    if beads_dir.join("embeddeddolt").is_dir() && !beads_dir.join("beads.db").exists() {
        return Some(format!(
            "{} has only a bd embedded-Dolt store (.beads/embeddeddolt) that rsry cannot read, \
             and no `beads.db` to fall back to — this repo will report 0 beads. Export it to a \
             store rsry reads: a SQLite `.beads/beads.db`, or `bd init --server` for Dolt server \
             mode (ADR-0014).",
            beads_dir.display()
        ));
    }
    None
}

pub async fn connect_bead_store(beads_dir: &Path) -> Result<Box<dyn BeadStore>> {
    let dolt_dir = beads_dir.join("dolt");
    if dolt_dir.exists() {
        // Two store shapes in one `.beads/` is AMBIGUOUS, and a read path must
        // not resolve it by guessing (rosary-9103f7). This branch used to
        // silently drain beads.db into Dolt and rename it away — a lossy
        // write triggered by a mere connect: it dropped `notes` (the
        // files/test_files/derived_from container, so file-overlap protection
        // died with it), `external_ref`, `design`, `user_id`, and both
        // timestamps, and never carried comments or dependencies at all.
        //
        // Fail closed and name both paths. The sanctioned conversion is
        // `rsry bead migrate`, which verifies field-level fidelity and keeps a
        // backup — this is the same "materialization must be explicit"
        // principle as rosary-554a74, applied to the open path.
        let sqlite_path = beads_dir.join("beads.db");
        if sqlite_path.exists() {
            anyhow::bail!(
                "ambiguous bead store in {}: both a Dolt store ({}) and a SQLite store ({}) exist, \
                 so rsry cannot tell which is authoritative. Nothing was changed. Resolve it \
                 explicitly: run `rsry bead migrate --to sqlite` (field-level verified, keeps a \
                 backup), or move aside whichever store is stale.",
                beads_dir.display(),
                dolt_dir.display(),
                sqlite_path.display()
            );
        }

        let config = crate::dolt::DoltConfig::from_beads_dir(beads_dir)?;
        let client = crate::dolt::DoltClient::connect(&config).await?;
        if let Err(e) = client.migrate().await {
            eprintln!("[bead] migration warning for {}: {e}", beads_dir.display());
        }

        return Ok(Box::new(crate::bead_dolt::DoltBeadStore::new(client)));
    }

    // No dolt/ dir — repo not yet initialized. SQLite for bootstrapping only.
    // Fail loud if this is actually an unreadable bd store (embedded Dolt),
    // so we don't silently report 0 beads for a populated repo (rosary-21e2d4).
    if let Some(warning) = unreadable_backend_warning(beads_dir) {
        eprintln!("[bead] WARNING: {warning}");
    }
    let sqlite_path = beads_dir.join("beads.db");
    let store = SqliteBeadStore::connect(&sqlite_path)?;
    // Every bead write goes through this one seam, so the tracked-JSONL refresh
    // hangs off it rather than off the ~50 call sites (rosary-8ca6e5). Inert
    // when there is no tracked projection to publish to.
    Ok(Box::new(crate::publish::PublishingBeadStore::new(
        Box::new(store),
        beads_dir,
    )))
}

/// The same store, but with tracked-JSONL publication switched OFF.
///
/// For paths that REPLAY the published record into a local store rather than
/// originate new state — today just `rsry init`'s bootstrap. A fresh clone
/// imports the projection and then overlays terminal state derived from trunk
/// merge commits; that derivation is local inference, not a publication event,
/// and echoing it back would rewrite the shared file in every consumer's
/// working tree on first init. `bootstrap_git_tracked_beads` states the
/// invariant ("never exports the live store, preserving intentionally
/// scrubbed/omitted records") and `tests/init_jsonl_reconciliation.rs` enforces
/// it.
///
/// Reach for this only when a write's effect on the projection would be an ECHO
/// of what the projection already told us. Everything else wants
/// [`connect_bead_store`].
pub async fn connect_bead_store_unpublished(beads_dir: &Path) -> Result<Box<dyn BeadStore>> {
    if beads_dir.join("dolt").exists() {
        return connect_bead_store(beads_dir).await;
    }
    Ok(Box::new(SqliteBeadStore::connect(
        &beads_dir.join("beads.db"),
    )?))
}

pub struct SqliteBeadStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    path: PathBuf,
}

/// Rewrite any non-canonical bead status (aliases like `closed`, `deadletter`)
/// to its `BeadState` canonical form, in place. Derived from the enum's own
/// parser + `as_str`, so the migration can never drift from the type. Runs on
/// connect (idempotent); best-effort — a query failure just leaves rows as-is.
fn canonicalize_statuses(conn: &Connection) {
    use crate::bead::BeadState;
    let distinct: Vec<String> = match conn.prepare("SELECT DISTINCT status FROM issues") {
        Ok(mut stmt) => match stmt.query_map([], |r| r.get::<_, String>(0)) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => return,
        },
        Err(_) => return,
    };
    for raw in distinct {
        let canonical = BeadState::from(raw.as_str()).to_string();
        if canonical != raw {
            let _ = conn.execute(
                "UPDATE issues SET status = ?1 WHERE status = ?2",
                params![canonical, raw],
            );
        }
    }
}

impl SqliteBeadStore {
    /// Resolve a short or full bead ID to the canonical full ID stored in the DB.
    ///
    /// Accepts either an exact match or a suffix match (e.g. "2a3970" → "ley-line-open-2a3970").
    /// Returns an error if zero or multiple beads match — ambiguous short IDs must be lengthened.
    fn resolve_id(conn: &Connection, id: &str) -> Result<String> {
        let exact: Option<String> = conn
            .query_row("SELECT id FROM issues WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(full) = exact {
            return Ok(full);
        }
        // Suffix match: "abc123" matches "repo-abc123"
        let suffix_pattern = format!("%-{id}");
        let mut stmt = conn.prepare("SELECT id FROM issues WHERE id LIKE ?1")?;
        let matches: Vec<String> = stmt
            .query_map(params![suffix_pattern], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        match matches.len() {
            0 => anyhow::bail!("bead not found: {id}"),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => anyhow::bail!("ambiguous bead ID {id:?} — matches: {}", matches.join(", ")),
        }
    }

    /// Open or create the bead database at the given path.
    /// Set a bead's status **verbatim**, bypassing the state-machine transition
    /// guard that `update_status` enforces. For RESTORE contexts only — store
    /// migration and import reconstruct *existing* state (a bead is already
    /// `blocked`/`done`), they don't perform a live transition, so `open →
    /// blocked` must be writable. Canonicalizes the input exactly like
    /// `update_status` so no reader has to absorb aliases. Inherent (not on the
    /// `BeadStore` trait) so the guard-bypass can't leak into normal code paths.
    pub(crate) async fn restore_status(&self, id: &str, status: &str) -> Result<()> {
        let next = crate::bead::BeadState::from(status);
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![next.to_string(), full_id],
        )
        .with_context(|| format!("restoring status for {id}"))?;
        Ok(())
    }

    /// Write `created_at` / `updated_at` **verbatim**. For RESTORE contexts
    /// only. `create_bead_full` and `restore_status` both stamp `now()`, which
    /// is fine for a one-shot migration but breaks *sync* (rosary-4ebf52) two
    /// ways: LWW comparison becomes meaningless (every restored bead looks
    /// locally-newest, so peers flap authority back and forth), and each
    /// machine's re-export differs from its source, churning the git-tracked
    /// JSONL instead of leaving it byte-stable. Preserving the source
    /// timestamps is what makes the export a *convergent* value.
    /// Takes typed instants and formats them in the store's canonical
    /// `%Y-%m-%d %H:%M:%S` — the ONLY shape `parse_datetime` accepts. Passing
    /// an RFC3339 string straight through (with its `+00:00` offset) matches
    /// neither reader pattern, and `parse_datetime` falls back to `Utc::now()`
    /// on failure, so the write would look like it worked and read back as
    /// "now" — silently defeating LWW. Owning the format here keeps that trap
    /// out of callers.
    pub(crate) async fn restore_timestamps(
        &self,
        id: &str,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        const STORE_FMT: &str = "%Y-%m-%d %H:%M:%S";
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET created_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                created_at.format(STORE_FMT).to_string(),
                updated_at.format(STORE_FMT).to_string(),
                full_id
            ],
        )
        .with_context(|| format!("restoring timestamps for {id}"))?;
        Ok(())
    }

    /// Insert a dependency edge **verbatim**, without checking that
    /// `depends_on_id` resolves to a local bead. For RESTORE contexts only —
    /// migration must preserve **cross-repo** edges (a rosary bead blocking on
    /// a mache bead), which the target legitimately holds as a dangling
    /// `depends_on_id` (the table has no FK), but which `add_dependency`'s
    /// existence check rejects. Inherent, so the bypass stays out of normal
    /// code paths.
    pub(crate) async fn restore_dependency(
        &self,
        issue_id: &str,
        depends_on_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?1, ?2)",
            params![issue_id, depends_on_id],
        )
        .with_context(|| format!("restoring dependency {issue_id} -> {depends_on_id}"))?;
        Ok(())
    }

    pub fn connect(path: &Path) -> Result<Self> {
        // rosary-554a74: refuse to MANUFACTURE a store next to a Dolt one.
        //
        // `connect` creates the db + schema when absent, so any command run in
        // a Dolt-backed repo from a SQLite-opening path materialises an empty
        // `beads.db`. That phantom then makes the directory AMBIGUOUS, and
        // `connect_bead_store` correctly refuses to read it (rosary-9103f7) —
        // so a harmless stray write bricks bead access for the whole repo.
        //
        // Observed twice on 2026-07-27, in cloister and signet: a 0-byte
        // `beads.db` beside a live multi-megabyte Dolt store, each blocking
        // reads until moved aside by hand.
        //
        // Catching the ambiguity on READ was the symptom fix. This is the
        // cause: refuse to create the file in the first place. An EXISTING
        // SQLite store is still opened — this only blocks minting a new one
        // where Dolt is clearly authoritative.
        if !path.exists()
            && let Some(beads_dir) = path.parent()
            && beads_dir.join("dolt").is_dir()
        {
            anyhow::bail!(
                "refusing to create a SQLite bead store at {} — {} already holds a Dolt store, \
                 and creating one here would make the directory ambiguous (rosary-554a74). \
                 If Dolt is stale, move it aside; if you meant to migrate, run \
                 `rsry bead migrate --to sqlite`.",
                path.display(),
                beads_dir.display()
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("opening sqlite bead store")?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        // Additive migrations: safe on existing databases.
        // Fail silently if the column already exists.
        let _ = conn.execute_batch("ALTER TABLE issues ADD COLUMN created_by TEXT");
        let _ = conn.execute_batch("ALTER TABLE issues ADD COLUMN scope TEXT NOT NULL DEFAULT ''");
        // rosary-649660: typed dependency edges. Existing rows default to
        // 'blocks', preserving the prior "every edge is a blocker" semantics.
        let _ = conn.execute_batch(
            "ALTER TABLE dependencies ADD COLUMN dep_type TEXT NOT NULL DEFAULT 'blocks'",
        );
        // rosary-a96b06: comment audit-trail columns. Idempotent on existing DBs.
        let _ = conn.execute_batch("ALTER TABLE comments ADD COLUMN edited_at TEXT");
        let _ = conn.execute_batch("ALTER TABLE comments ADD COLUMN edit_reason TEXT");
        let _ = conn.execute_batch("ALTER TABLE comments ADD COLUMN original_text TEXT");
        let _ = conn.execute_batch("ALTER TABLE comments ADD COLUMN deleted_at TEXT");
        let _ = conn.execute_batch("ALTER TABLE comments ADD COLUMN delete_reason TEXT");
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_comments_issue_id ON comments(issue_id)",
        );
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_comments_deleted_at ON comments(deleted_at)",
        );
        // Canonicalize legacy status aliases → BeadState canonical forms. The
        // write boundary (update_status) now stores only canonical values, but
        // rows written before that may hold aliases; heal them here on connect
        // so no reader has to absorb aliases at read time (idempotent). Derived
        // generically from `BeadState` so it can't drift from the enum.
        canonicalize_statuses(&conn);
        // FTS5 index for full-text search with porter stemmer.
        // Separate from issues table — manually kept in sync on create/update.
        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS bead_fts USING fts5(\
                id UNINDEXED, title, description,\
                tokenize='porter unicode61'\
            );",
        );
        Ok(SqliteBeadStore {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT DEFAULT '',
    design TEXT DEFAULT '',
    acceptance_criteria TEXT DEFAULT '',
    notes TEXT DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    priority INTEGER NOT NULL DEFAULT 2,
    issue_type TEXT NOT NULL DEFAULT 'task',
    assignee TEXT,
    external_ref TEXT,
    user_id TEXT,
    created_by TEXT,
    scope TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dependencies (
    issue_id TEXT NOT NULL,
    depends_on_id TEXT NOT NULL,
    dep_type TEXT NOT NULL DEFAULT 'blocks',
    PRIMARY KEY (issue_id, depends_on_id)
);

CREATE TABLE IF NOT EXISTS comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    text TEXT NOT NULL,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL,
    edited_at TEXT,
    edit_reason TEXT,
    original_text TEXT,
    deleted_at TEXT,
    delete_reason TEXT
);
-- Indexes on comments live in the post-ALTER block in `connect()`,
-- not here. On legacy DBs whose comments table predates the audit-trail
-- columns, `CREATE INDEX ON comments(deleted_at)` fails because the
-- column doesn't exist yet — and execute_batch(SCHEMA)? would propagate
-- that. Defining the index after the ALTER ADD COLUMN runs keeps
-- legacy DBs migrating cleanly without spurious no-such-column warnings.

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL DEFAULT 'rosary',
    comment TEXT,
    created_at TEXT NOT NULL
);
";

/// SQL for listing beads with dependency/comment counts (non-closed).
/// Canonical per-issue column projection — every reader selects exactly this,
/// so no query can silently omit a field `bead_from_row` reads (ADR-0021 slice
/// 1). Adding a bead column is one edit here, not four. `acceptance_criteria`
/// lives here because it was the field every reader used to drop.
const BEAD_ISSUE_COLUMNS: &str = "i.id, i.title, i.description, i.status, i.priority, \
    i.issue_type, i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at, \
    i.created_by, i.scope, i.acceptance_criteria";

/// The three count columns `bead_from_row` reads, computed by [`BEAD_COUNT_JOINS`].
const BEAD_COUNT_COLUMNS: &str = "COALESCE(dep.cnt, 0) as dep_count, \
    COALESCE(deps.cnt, 0) as dependency_count, COALESCE(cmt.cnt, 0) as comment_count";

/// Joins backing [`BEAD_COUNT_COLUMNS`]. `dependency_count` counts only open,
/// non-epic blockers (what triage cares about).
const BEAD_COUNT_JOINS: &str = "
LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
     ON dep.depends_on_id = i.id
LEFT JOIN (SELECT d.issue_id, COUNT(*) as cnt
          FROM dependencies d
          JOIN issues dep_i ON dep_i.id = d.depends_on_id
          WHERE dep_i.status NOT IN ('closed', 'done') AND dep_i.issue_type != 'epic'
          GROUP BY d.issue_id) deps
     ON deps.issue_id = i.id
LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
     ON cmt.issue_id = i.id";

/// Build a bead read query from the one canonical projection + joins, with a
/// caller-supplied WHERE and ORDER BY. Single source for every reader (list,
/// list-all, scoped, get) so their column sets can't drift (ADR-0021 slice 1) —
/// this retires the two "keep in sync" `LIST_BEADS_SQL` / `LIST_ALL_BEADS_SQL`
/// constants, which differed only by a WHERE clause.
fn bead_read_sql(where_clause: &str, order_by: &str) -> String {
    format!(
        "SELECT {BEAD_ISSUE_COLUMNS}, {BEAD_COUNT_COLUMNS} FROM issues i {BEAD_COUNT_JOINS} {where_clause} {order_by}"
    )
}

const BEAD_READ_ORDER: &str = "ORDER BY i.priority ASC, i.created_at DESC";

#[async_trait]
impl BeadStore for SqliteBeadStore {
    async fn list_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let sql = bead_read_sql("WHERE i.status NOT IN ('closed', 'done')", BEAD_READ_ORDER);
        let mut stmt = conn.prepare(&sql)?;
        let beads = stmt
            .query_map([], |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn list_all_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let sql = bead_read_sql("", BEAD_READ_ORDER);
        let mut stmt = conn.prepare(&sql)?;
        // Fail loud: export/backup must never silently drop a malformed row
        // (rosary-91e712). Unlike list_beads (triage), we propagate parse errors.
        let beads = stmt
            .query_map([], |row| bead_from_row(row, repo_name))?
            .collect::<rusqlite::Result<Vec<Bead>>>()?;
        Ok(beads)
    }

    async fn list_beads_scoped(&self, repo_name: &str, user_id: Option<&str>) -> Result<Vec<Bead>> {
        match user_id {
            Some(uid) => {
                let conn = self.conn.lock().unwrap();
                let sql_scoped = bead_read_sql(
                    "WHERE i.status NOT IN ('closed', 'done') AND i.user_id = ?1",
                    BEAD_READ_ORDER,
                );
                let mut stmt = conn.prepare(&sql_scoped)?;
                let beads = stmt
                    .query_map(params![uid], |row| bead_from_row(row, repo_name))?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(beads)
            }
            None => self.list_beads(repo_name).await,
        }
    }

    async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<Bead>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let sql = bead_read_sql("WHERE i.id = ?1", "");
        let mut stmt = conn.prepare(&sql)?;
        let bead = stmt
            .query_row(params![full_id], |row| bead_from_row(row, repo_name))
            .optional()?;
        Ok(bead)
    }

    async fn create_bead_full(&self, bead: NewBead) -> Result<()> {
        let NewBead {
            id,
            title,
            description,
            priority,
            issue_type,
            owner,
            files,
            test_files,
            depends_on,
            created_by,
            scope,
            derived_from,
            acceptance_criteria,
        } = &bead;
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, status, priority, issue_type, created_by, scope, created_at, updated_at)
             VALUES (?1, ?2, ?3, '', ?8, '', 'open', ?4, ?5, ?6, ?7, datetime('now'), datetime('now'))",
            params![id, title, description, *priority as i32, issue_type, created_by, scope, acceptance_criteria],
        )?;

        // Only set assignee when an owner is given. An empty owner means
        // "unset" — leaving assignee NULL (reads back as `None`, not
        // `Some("")`) so reconcile's `owner.is_some()` auto-assign still fires.
        if !owner.is_empty() {
            tx.execute(
                "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![owner, id],
            )?;
        }

        if !files.is_empty() || !test_files.is_empty() || !derived_from.is_empty() {
            let notes_json = serde_json::json!({
                "files": files,
                "test_files": test_files,
                "derived_from": derived_from,
            });
            tx.execute(
                "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![notes_json.to_string(), id],
            )?;
        }

        for dep_id in depends_on {
            tx.execute(
                "INSERT OR IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?1, ?2)",
                params![id, dep_id],
            )?;
        }

        // Sync FTS index (best-effort — secondary index, never fails the create)
        let _ = tx.execute(
            "INSERT INTO bead_fts(id, title, description) VALUES (?1, ?2, ?3)",
            params![id, title, description],
        );

        tx.commit()?;
        Ok(())
    }

    async fn update_bead_fields(&self, id: &str, update: &BeadUpdate) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        let mut updated_fields = Vec::new();

        // Build dynamic SET clauses. We execute each field update separately
        // to avoid dynamic bind complexity with rusqlite.
        if let Some(ref title) = update.title {
            conn.execute(
                "UPDATE issues SET title = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![title, full_id],
            )?;
            updated_fields.push("title".to_string());
        }
        if let Some(ref description) = update.description {
            conn.execute(
                "UPDATE issues SET description = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![description, full_id],
            )?;
            updated_fields.push("description".to_string());
        }
        if let Some(priority) = update.priority {
            conn.execute(
                "UPDATE issues SET priority = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![priority as i32, full_id],
            )?;
            updated_fields.push("priority".to_string());
        }
        if let Some(ref issue_type) = update.issue_type {
            conn.execute(
                "UPDATE issues SET issue_type = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![issue_type, full_id],
            )?;
            updated_fields.push("issue_type".to_string());
        }
        if let Some(ref owner) = update.owner {
            conn.execute(
                "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![owner, full_id],
            )?;
            updated_fields.push("owner".to_string());
        }
        if let Some(ref ac) = update.acceptance_criteria {
            conn.execute(
                "UPDATE issues SET acceptance_criteria = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![ac, full_id],
            )?;
            updated_fields.push("acceptance_criteria".to_string());
        }
        if update.files.is_some() || update.test_files.is_some() {
            // Read existing notes to preserve fields not being updated
            let existing_notes: serde_json::Value = conn
                .query_row(
                    "SELECT notes FROM issues WHERE id = ?1",
                    params![full_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));

            let files = update
                .files
                .as_ref()
                .map(|f| serde_json::json!(f))
                .unwrap_or_else(|| {
                    existing_notes
                        .get("files")
                        .cloned()
                        .unwrap_or(serde_json::json!([]))
                });
            let test_files_val = update
                .test_files
                .as_ref()
                .map(|f| serde_json::json!(f))
                .unwrap_or_else(|| {
                    existing_notes
                        .get("test_files")
                        .cloned()
                        .unwrap_or(serde_json::json!([]))
                });
            // Mutate in place so other notes keys (derived_from) survive. (rosary-027940)
            let mut notes_json = if existing_notes.is_object() {
                existing_notes
            } else {
                serde_json::json!({})
            };
            notes_json["files"] = files;
            notes_json["test_files"] = test_files_val;
            conn.execute(
                "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![notes_json.to_string(), full_id],
            )?;
            if update.files.is_some() {
                updated_fields.push("files".to_string());
            }
            if update.test_files.is_some() {
                updated_fields.push("test_files".to_string());
            }
        }

        // Re-sync FTS if title or description changed (delete + re-insert from issues)
        if updated_fields
            .iter()
            .any(|f| f == "title" || f == "description")
        {
            let _ = conn.execute("DELETE FROM bead_fts WHERE id = ?1", params![full_id]);
            let _ = conn.execute(
                "INSERT INTO bead_fts(id, title, description) \
                 SELECT id, title, description FROM issues WHERE id = ?1",
                params![full_id],
            );
        }

        Ok(updated_fields)
    }

    async fn update_status(&self, id: &str, status: &str) -> Result<()> {
        use crate::bead::BeadState;
        let next = BeadState::from(status);
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        // Validate transition before writing. Read current status under the same lock
        // so the check-then-act is atomic within this process.
        let current_str: Option<String> = conn
            .query_row(
                "SELECT status FROM issues WHERE id = ?1",
                params![full_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(ref cs) = current_str {
            let current = BeadState::from(cs.as_str());
            if !current.can_transition_to(next) {
                return Err(anyhow::anyhow!(
                    "invalid state transition: {} -> {} for bead {}",
                    current,
                    next,
                    id
                ));
            }
        }
        // Canonicalize at the write boundary: persist `next.as_str()`, not the
        // raw input. Aliases ("closed", "deadletter") are tolerated on INPUT but
        // never stored — so no reader downstream has to absorb them (the
        // read-time absorption the drift review flagged).
        conn.execute(
            "UPDATE issues SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![next.to_string(), full_id],
        )
        .with_context(|| format!("updating status for {id}"))?;
        Ok(())
    }

    async fn get_status(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let status = conn
            .query_row(
                "SELECT status FROM issues WHERE id = ?1",
                params![full_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status)
    }

    async fn close_bead(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET status = 'closed', updated_at = datetime('now') WHERE id = ?1",
            params![full_id],
        )
        .with_context(|| format!("closing bead {id}"))?;
        Ok(())
    }

    async fn set_assignee(&self, id: &str, assignee: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![assignee, full_id],
        )?;
        Ok(())
    }

    async fn set_user_id(&self, id: &str, user_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET user_id = ?1 WHERE id = ?2",
            params![user_id, full_id],
        )?;
        Ok(())
    }

    async fn set_files(&self, id: &str, files: &[String], test_files: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        // Read-merge: the notes column also holds derived_from (provenance).
        // Rewriting it wholesale would clobber provenance, so preserve the
        // existing object and overwrite only files/test_files. (rosary-027940)
        let mut notes = conn
            .query_row(
                "SELECT notes FROM issues WHERE id = ?1",
                params![full_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        notes["files"] = serde_json::json!(files);
        notes["test_files"] = serde_json::json!(test_files);
        conn.execute(
            "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![notes.to_string(), full_id],
        )?;
        Ok(())
    }

    async fn search_beads(
        &self,
        query_str: &str,
        repo_name: &str,
        limit: u32,
    ) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let words: Vec<String> = query_str
            .split_whitespace()
            .map(|w| format!("%{}%", w.to_lowercase()))
            .collect();

        // Each word must appear in title, description, id, OR a live
        // (non-soft-deleted) comment. Comment-text matching is what
        // rosary-a9bc77 added — most context lives in comments and
        // wasn't previously searchable.
        let where_clause = if words.is_empty() {
            "1=1".to_string()
        } else {
            words
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let p = (i * 4) + 1;
                    format!(
                        "(LOWER(i.title) LIKE ?{p} OR LOWER(i.description) LIKE ?{} \
                          OR i.id LIKE ?{} \
                          OR EXISTS (SELECT 1 FROM comments c \
                                     WHERE c.issue_id = i.id \
                                       AND c.deleted_at IS NULL \
                                       AND LOWER(c.text) LIKE ?{}))",
                        p + 1,
                        p + 2,
                        p + 3,
                    )
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        };

        let sql = format!(
            "SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                    i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                    i.created_by, i.scope, i.acceptance_criteria,
                    COALESCE(dep.cnt, 0) as dep_count,
                    COALESCE(deps.cnt, 0) as dependency_count,
                    COALESCE(cmt.cnt, 0) as comment_count
             FROM issues i
             LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
                  ON dep.depends_on_id = i.id
             LEFT JOIN (SELECT d.issue_id, COUNT(*) as cnt
                       FROM dependencies d
                       LEFT JOIN issues dep_i ON dep_i.id = d.depends_on_id
                       WHERE dep_i.id IS NULL OR (dep_i.status NOT IN ('closed', 'done') AND dep_i.issue_type != 'epic')
                       GROUP BY d.issue_id) deps
                  ON deps.issue_id = i.id
             LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
                  ON cmt.issue_id = i.id
             WHERE {where_clause}
             ORDER BY i.priority ASC, i.created_at DESC
             LIMIT {limit}"
        );

        let mut stmt = conn.prepare(&sql)?;
        // Build params: each word appears four times — title, description,
        // id, comment text (rosary-a9bc77).
        let param_values: Vec<Box<dyn rusqlite::types::ToSql>> = words
            .iter()
            .flat_map(|w| {
                vec![
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                ]
            })
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let beads = stmt
            .query_map(param_refs.as_slice(), |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn search_beads_fts(
        &self,
        query_str: &str,
        repo_name: &str,
        limit: u32,
    ) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        // Wrap each token in double quotes: "word" prevents FTS5 operator interpretation
        // and gives "all words must appear" semantics (implicit AND).
        let fts_query: String = query_str
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");

        if fts_query.is_empty() {
            return Ok(vec![]);
        }

        let sql = "SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                        i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                        i.created_by, i.scope, i.acceptance_criteria,
                        COALESCE(dep.cnt, 0) as dep_count,
                        COALESCE(deps.cnt, 0) as dependency_count,
                        COALESCE(cmt.cnt, 0) as comment_count
                 FROM bead_fts f
                 JOIN issues i ON i.id = f.id
                 LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
                      ON dep.depends_on_id = i.id
                 LEFT JOIN (SELECT d.issue_id, COUNT(*) as cnt
                           FROM dependencies d
                           LEFT JOIN issues dep_i ON dep_i.id = d.depends_on_id
                           WHERE dep_i.id IS NULL OR (dep_i.status NOT IN ('closed', 'done') AND dep_i.issue_type != 'epic')
                           GROUP BY d.issue_id) deps
                      ON deps.issue_id = i.id
                 LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
                      ON cmt.issue_id = i.id
                 WHERE bead_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2";

        let mut stmt = conn.prepare(sql)?;
        let beads = stmt
            .query_map(params![fts_query, limit as i64], |row| {
                bead_from_row(row, repo_name)
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn get_external_ref(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let result = conn
            .query_row(
                "SELECT external_ref FROM issues WHERE id = ?1 AND external_ref IS NOT NULL AND external_ref != ''",
                params![full_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    async fn set_external_ref(&self, id: &str, external_ref: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, id)?;
        conn.execute(
            "UPDATE issues SET external_ref = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![external_ref, full_id],
        )?;
        Ok(())
    }

    async fn find_by_external_ref(&self, external_ref: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT id FROM issues WHERE external_ref = ?1",
                params![external_ref],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    async fn list_closed_linked_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, priority, issue_type,
                    assignee, external_ref, '' as notes, created_at, updated_at,
                    created_by, scope, acceptance_criteria,
                    0 as dep_count, 0 as dependency_count, 0 as comment_count
             FROM issues
             WHERE status = 'closed' AND external_ref IS NOT NULL AND external_ref != ''
             ORDER BY updated_at DESC
             LIMIT 500",
        )?;
        let beads = stmt
            .query_map([], |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        self.add_dependency_typed(issue_id, depends_on_id, "blocks")
            .await
    }

    async fn add_dependency_typed(
        &self,
        issue_id: &str,
        depends_on_id: &str,
        dep_type: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_issue = Self::resolve_id(&conn, issue_id)?;
        let full_dep = Self::resolve_id(&conn, depends_on_id)?;
        // Upsert the type so a re-link can promote a plain blocks edge to a
        // containment edge (parent-child / discovered-from).
        conn.execute(
            "INSERT INTO dependencies (issue_id, depends_on_id, dep_type) VALUES (?1, ?2, ?3)
             ON CONFLICT(issue_id, depends_on_id) DO UPDATE SET dep_type = excluded.dep_type",
            params![full_issue, full_dep, dep_type],
        )?;
        Ok(())
    }

    async fn get_children(&self, issue_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, issue_id) {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        // Children link to their parent via parent-child / discovered-from
        // edges (edge points from child → parent), so children are the
        // dependents filtered to those containment types.
        let mut stmt = conn.prepare(
            "SELECT issue_id FROM dependencies
             WHERE depends_on_id = ?1 AND dep_type IN ('parent-child', 'discovered-from')",
        )?;
        let kids = stmt
            .query_map(params![full_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(kids)
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_issue = Self::resolve_id(&conn, issue_id)?;
        let full_dep = Self::resolve_id(&conn, depends_on_id)?;
        conn.execute(
            "DELETE FROM dependencies WHERE issue_id = ?1 AND depends_on_id = ?2",
            params![full_issue, full_dep],
        )?;
        Ok(())
    }

    async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, issue_id) {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        let mut stmt =
            conn.prepare("SELECT depends_on_id FROM dependencies WHERE issue_id = ?1")?;
        let deps = stmt
            .query_map(params![full_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(deps)
    }

    async fn get_dependents(&self, issue_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, issue_id) {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        let mut stmt =
            conn.prepare("SELECT issue_id FROM dependencies WHERE depends_on_id = ?1")?;
        let deps = stmt
            .query_map(params![full_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(deps)
    }

    async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, issue_id)?;
        conn.execute(
            "INSERT INTO comments (issue_id, text, author, created_at) VALUES (?1, ?2, ?3, datetime('now'))",
            params![full_id, body, author],
        )?;
        Ok(())
    }

    async fn list_comments(
        &self,
        issue_id: &str,
        include_deleted: bool,
    ) -> Result<Vec<crate::bead::Comment>> {
        let conn = self.conn.lock().unwrap();
        let full_id = Self::resolve_id(&conn, issue_id)?;
        let sql = if include_deleted {
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE issue_id = ?1 ORDER BY created_at ASC, id ASC"
        } else {
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE issue_id = ?1 AND deleted_at IS NULL
             ORDER BY created_at ASC, id ASC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![full_id], |row| {
                let created_str: String = row.get("created_at")?;
                let edited_str: Option<String> = row.get("edited_at")?;
                let deleted_str: Option<String> = row.get("deleted_at")?;
                Ok(crate::bead::Comment {
                    id: row.get::<_, i64>("id")?.to_string(),
                    issue_id: row.get("issue_id")?,
                    text: row.get("text")?,
                    author: row.get("author")?,
                    created_at: parse_datetime(&created_str)?,
                    edited_at: edited_str.map(|s| parse_datetime(&s)).transpose()?,
                    edit_reason: row.get("edit_reason")?,
                    original_text: row.get("original_text")?,
                    deleted_at: deleted_str.map(|s| parse_datetime(&s)).transpose()?,
                    delete_reason: row.get("delete_reason")?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    async fn update_comment(
        &self,
        comment_id: &str,
        body: &str,
        reason: Option<&str>,
    ) -> Result<crate::bead::Comment> {
        let conn = self.conn.lock().unwrap();
        // Read current state to decide whether to capture original_text.
        let (prior_text, has_original): (String, bool) = conn
            .query_row(
                "SELECT text, original_text IS NOT NULL FROM comments WHERE id = ?1",
                params![comment_id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("comment {comment_id} not found"))?;

        // First-edit captures original_text; subsequent edits don't rewrite it.
        if has_original {
            conn.execute(
                "UPDATE comments
                 SET text = ?1, edited_at = datetime('now'), edit_reason = ?2
                 WHERE id = ?3",
                params![body, reason, comment_id],
            )?;
        } else {
            conn.execute(
                "UPDATE comments
                 SET text = ?1, edited_at = datetime('now'),
                     edit_reason = ?2, original_text = ?3
                 WHERE id = ?4",
                params![body, reason, prior_text, comment_id],
            )?;
        }

        // Re-fetch to return the canonical post-update row.
        let mut stmt = conn.prepare(
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE id = ?1",
        )?;
        let comment = stmt.query_row(params![comment_id], |row| {
            let created_str: String = row.get("created_at")?;
            let edited_str: Option<String> = row.get("edited_at")?;
            let deleted_str: Option<String> = row.get("deleted_at")?;
            Ok(crate::bead::Comment {
                id: row.get::<_, i64>("id")?.to_string(),
                issue_id: row.get("issue_id")?,
                text: row.get("text")?,
                author: row.get("author")?,
                created_at: parse_datetime(&created_str)?,
                edited_at: edited_str.map(|s| parse_datetime(&s)).transpose()?,
                edit_reason: row.get("edit_reason")?,
                original_text: row.get("original_text")?,
                deleted_at: deleted_str.map(|s| parse_datetime(&s)).transpose()?,
                delete_reason: row.get("delete_reason")?,
            })
        })?;
        Ok(comment)
    }

    async fn delete_comment(&self, comment_id: &str, reason: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM comments WHERE id = ?1",
            params![comment_id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            anyhow::bail!("comment {comment_id} not found");
        }
        // The deletion reason lands in `delete_reason` — its own column —
        // so it survives even when the comment was previously edited (which
        // would have set `edit_reason`). Soft-delete is idempotent: a
        // re-delete refreshes both timestamp and reason.
        conn.execute(
            "UPDATE comments
             SET deleted_at = datetime('now'), delete_reason = ?1
             WHERE id = ?2",
            params![reason, comment_id],
        )?;
        Ok(())
    }

    async fn hard_delete_comment(&self, comment_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM comments WHERE id = ?1", params![comment_id])?;
        if n == 0 {
            anyhow::bail!("comment {comment_id} not found");
        }
        Ok(())
    }

    async fn log_event(&self, issue_id: &str, event_type: &str, detail: &str) {
        let conn = self.conn.lock().unwrap();
        // IDs starting with `_` are synthetic (e.g. `_schema` for migration
        // records) and skip the issues-table resolution step. Without this,
        // every migration logged a noisy "bead not found" warning.
        let full_id = if issue_id.starts_with('_') {
            issue_id.to_string()
        } else {
            match Self::resolve_id(&conn, issue_id) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("warning: failed to resolve bead ID {issue_id} for event log: {e}");
                    return;
                }
            }
        };
        let result = conn.execute(
            "INSERT INTO events (issue_id, event_type, actor, comment, created_at) VALUES (?1, ?2, 'rosary', ?3, datetime('now'))",
            params![full_id, event_type, detail],
        );
        if let Err(e) = result {
            eprintln!("warning: failed to log event for {issue_id}: {e}");
        }
    }

    async fn get_latest_event(&self, issue_id: &str, event_type: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, issue_id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let result = conn
            .query_row(
                "SELECT comment FROM events WHERE issue_id = ?1 AND event_type = ?2 ORDER BY created_at DESC LIMIT 1",
                params![full_id, event_type],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    async fn list_event_details(&self, issue_id: &str, event_type: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let full_id = match Self::resolve_id(&conn, issue_id) {
            Ok(id) => id,
            Err(_) => return Ok(Vec::new()),
        };
        let mut stmt = conn.prepare(
            "SELECT comment FROM events WHERE issue_id = ?1 AND event_type = ?2 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![full_id, event_type], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests;
