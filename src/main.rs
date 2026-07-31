#![recursion_limit = "256"]
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// Generated capnp bindings for the leyline-net IPC wire (rosary-6371e3).
// Source schema: schemas/cloister.capnp, vendored from ley-line-open's
// canonical net.capnp (rosary-086973). Drift gate: leyline_net_vectors.
#[allow(clippy::all, dead_code, unused_imports)]
mod cloister_capnp {
    include!(concat!(env!("OUT_DIR"), "/cloister_capnp.rs"));
}

// leyline-net/v1 conformance-vector drift gate (rosary-086973): rebuilds
// LLO's pinned vectors with the generated bindings above and asserts
// byte-equality + decode. Test-only; keeps schemas/cloister.capnp from
// silently drifting from LLO's net.capnp.
#[cfg(test)]
mod leyline_net_vectors;

mod acp;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod backend;
mod bdr_enrich;
mod bead;
mod bead_backend;
mod bead_backup;
mod bead_correct;
mod bead_diff;
mod bead_dolt;
mod bead_ext;
#[allow(dead_code)]
// identity primitive — wired into create + resolve() in the P1 follow-on (rosary-160bb2)
mod bead_genesis;
mod bead_migrate;
mod bead_move;
mod bead_ops;
mod bead_sqlite;
mod capture;
mod cas;
mod cli;
mod column_rail;
mod config;
// Test-only: the export/import round-trip property (rosary-c45a35).
mod context;
#[cfg(test)]
mod contract_roundtrip;
mod coordination;
mod credential;
mod decompose;
mod dispatch;
mod dolt;
#[allow(dead_code)] // API surface for PM agent (loom-w8c.4); is_dominated_by used by reconciler
mod epic;
#[allow(dead_code)] // API surface — wired into pipeline phase transitions
#[cfg(test)]
mod field_drift;
#[allow(dead_code)] // API surface — PR creation from dispatch pipeline
mod github;
mod github_mirror;
mod gitignore;
mod graph;
mod handoff;
mod handoff_attestation;
mod import;
mod init;
mod jsonl;
mod jsonl_sync;
mod linear;
#[allow(dead_code)]
mod linear_tracker;
#[allow(dead_code)] // API surface — consumed by orchestrator after dispatch
mod manifest;
#[allow(dead_code)]
mod migrate;
mod model_provider;
mod notes;
#[allow(dead_code)] // ADR-0010 substrate; observers wired in obs-* follow-up beads
mod observation;
mod openai_compat;
mod orchestrate;
// Test-only: a declared map of the CLI/MCP surfaces plus the ratchet that
// checks it against what the binary actually exposes. No runtime callers by
// design — it describes the surface, it does not serve it.
#[cfg(test)]
mod parity;
// Test-only: the shared deterministic proptest harness.
mod personal;
mod pipeline;
mod plugin;
mod pool;
mod precommit_yaml;
#[cfg(test)]
mod proptest_support;
mod publish;
mod queue;
mod reconcile;
mod repo_cache;
mod restore;
mod scan_assay;
mod scanner;
mod status;
// `ScopeId` for rosary-b5da2f scope abstraction. Pure type + parsing in
// PR 1; threaded through stores + MCP handlers in later PRs. Allow
// dead_code while the call sites are still on `repo_path: &str`.
#[allow(dead_code)]
mod scope;
mod secrets;
mod serve;
mod session;
mod skills;
#[allow(dead_code)] // API surface — wired in rsry-e599fb (SpritesProvider)
mod sprites;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod sprites_provider;
#[allow(dead_code)] // Phase 1: traits + impl, wired in Phase 2
mod store;
#[allow(dead_code)] // Phase 1: Dolt backend, wired in Phase 2
mod store_dolt;
#[allow(dead_code)] // Phase 1: SQLite backend, wired alongside Dolt
mod store_sqlite;
#[allow(dead_code)]
mod sync;
#[cfg(test)]
mod testutil;
mod text;
// Test-only until equivalence is proven per tool (rosary-08a278).
#[cfg(test)]
mod toolreg;
mod vcs;
mod verify;
#[allow(dead_code)] // API surface — replaces dispatch.rs worktree logic
mod workspace;
mod xref;

#[derive(Parser)]
#[command(
    name = "rsry",
    about = "Strings beads, repos, and review layers into coordinated work",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("RSRY_BUILD_HASH"),
        " ",
        env!("RSRY_BUILD_TIME"),
        ")"
    ),
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan repos for issues, create beads (bottom-up discovery)
    Scan {
        /// Config file listing repos to scan
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Run assay.scan plugins and file P3 chore beads for stale refs
        #[arg(long)]
        assay: bool,
    },
    /// Decompose a Linear ticket into repo-scoped beads (top-down planning)
    Plan {
        /// Linear ticket ID or URL
        ticket: String,
    },
    /// Bidirectional sync: beads ↔ Linear status
    Sync {
        /// Preview changes without executing
        #[arg(long)]
        dry_run: bool,
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Mirror bead context to linked GitHub PRs/issues as structured comments
        #[arg(long)]
        github: bool,
    },
    /// Show aggregated status across all repos
    Status {
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Output as JSON (for scripts/statusline)
        #[arg(long)]
        json: bool,
    },
    /// Dispatch a bead to an agent provider in an isolated worktree
    Dispatch {
        /// Bead ID to work on
        bead_id: String,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Agent provider (claude, gemini, acp, codex experimental)
        #[arg(long, default_value = "claude")]
        provider: String,
        /// Use isolated jj workspace
        #[arg(long, default_value_t = true)]
        isolate: bool,
    },
    /// Run the reconciliation loop (scan → triage → dispatch → verify → report)
    Run {
        /// Config file listing repos
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Max concurrent Claude Code agents
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Seconds between scan iterations
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// Single pass (no loop)
        #[arg(long)]
        once: bool,
        /// Print what would be dispatched without actually spawning agents
        #[arg(long)]
        dry_run: bool,
        /// AI provider to use for dispatch (claude, gemini, acp, codex experimental)
        #[arg(long, default_value = "claude")]
        provider: String,
        /// Overnight mode: prefer small/mechanical beads, concurrency=1, interval=120s
        #[arg(long)]
        overnight: bool,
        /// Target a specific bead (skip triage, dispatch only this bead)
        #[arg(long)]
        bead: Option<String>,
    },
    /// Start the reconciliation daemon in the background
    Start {
        /// Config file listing repos
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Max concurrent agents
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Seconds between scan iterations
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// AI provider (claude, gemini)
        #[arg(long, default_value = "claude")]
        provider: String,
        /// Overnight mode
        #[arg(long)]
        overnight: bool,
    },
    /// Stop the running daemon
    Stop,
    /// Tail the daemon log
    Logs,
    /// Start MCP server exposing rosary as tools
    Serve {
        /// Transport: stdio or http
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Port for HTTP transport
        #[arg(long, default_value = "8383")]
        port: u16,
    },
    /// Start MCP server over a Unix Domain Socket (capnp ToolCall/Result).
    ///
    /// Invoked by cloister's `cluster.capnp` with
    /// `args = ["mcp", "--ipc-socket", "/run/cloister-uds/rosary.sock"]`.
    /// Wire format is the intra-cluster amendment of ADR-0005: plain capnp
    /// ToolCall in, ToolResult out, no Manifest envelope, no AEAD. The UDS
    /// permissions are the trust boundary.
    Mcp {
        /// Path to bind the UDS at. Stale socket files at this path are
        /// removed before bind.
        #[arg(long, value_name = "PATH")]
        ipc_socket: PathBuf,
    },
    /// Send a single capnp ToolCall to a rosary IPC server (smoke + ops).
    ///
    /// Connects to `--ipc-socket`, sends one ToolCall, prints the
    /// `text` content of the ToolResult to stdout. Exit 0 on
    /// `isError = false`, 1 otherwise. Used by `task image:smoke` and
    /// the docker e2e test to verify the wire from inside the same
    /// container/VM namespace as the server (sidesteps the Docker
    /// Desktop macOS host→container AF_UNIX boundary).
    IpcCall {
        /// UDS path to connect to.
        #[arg(long, value_name = "PATH")]
        ipc_socket: PathBuf,
        /// MCP tool name (e.g. `rsry_status`).
        #[arg(long)]
        tool: String,
        /// JSON arguments. Defaults to `{}`.
        #[arg(long, default_value = "{}")]
        args: String,
    },
    /// Register current repo (or path) in the global registry (~/.rsry/config.toml)
    Enable {
        /// Path to repo root (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,
        /// Use a Dolt server store instead of the default single-file SQLite.
        #[arg(long)]
        dolt: bool,
    },
    /// Unregister a repo from the global registry by name or path
    Disable {
        /// Repo name or path to remove
        name_or_path: String,
    },
    /// Approve a repo for agent auto-dispatch (sets approval = approved).
    /// Only consulted when [dispatch] require_approval = true.
    Approve {
        /// Repo name to approve
        name: String,
    },
    /// Reject a repo for agent auto-dispatch (sets approval = rejected).
    Reject {
        /// Repo name to reject
        name: String,
    },
    /// Re-parent a thread under a different decade. Useful for cleanup when
    /// threads end up in `ungrouped` or `auto-discovered` and should be
    /// grouped under a real decade.
    ThreadReparent {
        /// Thread ID (e.g. agentic-provenance/agent-identity)
        thread_id: String,
        /// New decade ID (e.g. agentic-provenance)
        decade_id: String,
        /// Optional new thread name (keeps existing if omitted)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Decompose a markdown document (ADR, README, etc.) into beads
    Decompose {
        /// Path to the markdown file
        path: String,
        /// Title for the decade (defaults to first heading)
        #[arg(short, long)]
        title: Option<String>,
        /// Repo path to create beads in
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Preview without creating beads
        #[arg(long)]
        dry_run: bool,
        /// LLM model for non-ADR docs (haiku, sonnet, or full model ID).
        /// When set and the document is not ADR-shaped, uses the Anthropic
        /// Messages API to extract atoms instead of the heuristic parser.
        /// Requires ANTHROPIC_API_KEY env var.
        #[arg(long)]
        model: Option<String>,
        /// Emit code stubs into this repo path instead of (or in addition to)
        /// creating beads. Generates `.rsry-stubs/<decade>.rs` from
        /// TechnicalSpec and Constraint atoms. Review the stub PR before
        /// implementing to validate the design.
        #[arg(long)]
        stub_output: Option<String>,
    },
    /// Manage beads directly
    Bead {
        #[command(subcommand)]
        action: BeadAction,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Observation-lattice tooling (R4b).
    Lattice {
        #[command(subcommand)]
        action: LatticeAction,
    },
    /// Coordination-tier records, stored in `refs/agents/*` instead of the
    /// working tree (ADR-0022).
    ///
    /// This is the home for agent-dispatch notes, run events, and
    /// feature-local scratch — state that has no business landing in a code
    /// commit. Writes here never touch `.beads/beads.jsonl`, never appear as a
    /// branch, and are not fetched by a default `git clone`. That invisibility
    /// is disqualifying for canonical beads (ADR-0022 Q3) and is exactly the
    /// point for coordination.
    Coord {
        #[command(subcommand)]
        action: CoordAction,
        /// Repo path (defaults to the current directory)
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Emit the bead lattice (decade → thread → bead) as graph text for
    /// visual inspection. Writes DOT (graphviz) or mermaid to stdout — no new
    /// dependencies, the renderer lives outside rosary:
    ///
    ///   rsry graph | dot -Tpng -o lattice.png
    ///
    /// A full bead-level graph of the whole fleet is an unreadable hairball,
    /// so `--depth` is the primary control: `decade` is the fleet shape,
    /// `thread` (default) is the readable full export, `bead` should be
    /// scoped with `--decade` or `--orphans`.
    Graph {
        /// How deep to render: decade | thread | bead
        #[arg(long, default_value = "thread")]
        depth: String,
        /// Restrict to a single decade (e.g. agent-work-continuity)
        #[arg(long)]
        decade: Option<String>,
        /// Render only beads with no thread assignment. Implies --depth bead.
        #[arg(long)]
        orphans: bool,
        /// Output format: dot | mermaid
        #[arg(long, default_value = "dot")]
        format: String,
        /// Repo whose beads supply titles/priorities (default: current dir)
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Garbage-collect merged agent branches from origin
    Sweep {
        /// Repo path (defaults to current directory)
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Preview what would be deleted without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Close beads whose PRs have already merged.
    ///
    /// Walks every open bead with a recorded `pr_url` event, runs
    /// `gh pr view --json state,mergeCommit`, and closes the bead if the PR
    /// is MERGED. Useful for catching up after periods when the reconciler
    /// loop wasn't running and `poll_pr_status` missed merges (the
    /// `scan_vcs` path only looks at the last 50 commits).
    ///
    /// Idempotent. Safe to run any time. Doesn't dispatch agents.
    CloseMerged {
        /// Repo name to limit the sweep to (omit to scan all registered repos)
        #[arg(long)]
        repo: Option<String>,
        /// Preview what would close without making changes
        #[arg(long)]
        dry_run: bool,
        /// rsry-native local mode: detect merges from local `git log`
        /// (`[bead-id] … (#N)` squash commits on the trunk) instead of asking
        /// `gh` per bead. No network / webhook / tunnel — this is what the
        /// git `post-merge` hook runs after `git pull`.
        #[arg(long)]
        local: bool,
    },
    /// Create a GitHub PR with the current branch's `[bead-id]` auto-prefixed
    /// into the title (derived from HEAD's commit — Golden Rule 11 guarantees
    /// one is there), so the squash-merge subject carries the id and the
    /// post-merge hook auto-closes the bead. Thin wrapper over `gh pr create`.
    Pr {
        /// PR title — the `[bead-id]` prefix is added automatically if absent.
        #[arg(long)]
        title: String,
        /// Base branch (defaults to the repo default).
        #[arg(long)]
        base: Option<String>,
        /// Path to a file holding the PR body.
        #[arg(long)]
        body_file: Option<String>,
        /// Open the PR as a draft.
        #[arg(long)]
        draft: bool,
    },
    /// Export orchestrator backend state to JSON backup
    Backup {
        /// Output directory (default: ~/.rsry/backups/<timestamp>)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Migrate orchestrator backend between providers (e.g. dolt → sqlite)
    Migrate {
        /// Target provider: "sqlite" or "dolt"
        #[arg(long)]
        to: String,
        /// Target path (default: ~/.rsry/backend.db for sqlite)
        #[arg(long)]
        path: Option<String>,
        /// Skip post-migration verification
        #[arg(long)]
        skip_verify: bool,
    },
    /// Capture design atoms from a session transcript or source file
    Capture {
        /// Read transcript file (use `-` for stdin)
        #[arg(long, conflicts_with = "from_code")]
        from_session: Option<String>,
        /// Read source file: `<repo> <path>` (e.g. `rosary src/bead.rs`)
        #[arg(long, num_args = 2, conflicts_with = "from_session")]
        from_code: Vec<String>,
        /// Symbol to scope code capture (e.g. `BeadSpec`)
        #[arg(long)]
        symbol: Option<String>,
        /// LLM model: haiku (default), sonnet, or full model ID
        #[arg(long, default_value = "haiku")]
        model: String,
        /// Repo path for --commit (default: current directory)
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Write extracted BeadSpecs as beads (default: dry-run to stdout)
        #[arg(long)]
        commit: bool,
    },
    /// Manage encrypted notes (age-encrypted, scope-organized)
    Notes {
        #[command(subcommand)]
        action: NotesAction,
        /// Repo path containing `notes/`
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Manage git hooks for bead sync (post-push / post-merge)
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
        /// Repo path (defaults to current directory)
        #[arg(short, long, conflicts_with = "all")]
        repo: Option<String>,
        /// Apply the operation to every repo in ~/.rsry/config.toml.
        #[arg(long)]
        all: bool,
    },
    /// Report runtime truth: installed binary vs repo version vs the running MCP
    /// service — surfaces the "stale binary / stale service" drift (rosary-d09889).
    Doctor {
        /// Port of the running HTTP MCP service to probe.
        #[arg(long, default_value_t = 8383)]
        port: u16,
    },
    /// Onboard a repo to rosary bead tracking — the bd-init equivalent
    /// (ADR-0014): create the `.beads/` store, write the managed AGENTS.md
    /// section, install the git hooks, and register the repo. Idempotent.
    Init {
        /// Repo path (defaults to current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Use a Dolt server store instead of the default single-file SQLite.
        #[arg(long)]
        dolt: bool,
        /// Skip adding the repo to global rsry config (repo-local setup only).
        #[arg(long)]
        no_register: bool,
    },
}

#[derive(Subcommand)]
enum NotesAction {
    /// Re-encrypt all notes in a scope after editing the recipient list
    Rotate {
        /// Scope name (becomes `notes/<scope>/`)
        #[arg(long)]
        scope: String,
        /// Recipient(s) to add (repeatable)
        #[arg(long = "add-recipient", value_name = "RECIPIENT")]
        add: Vec<String>,
        /// Recipient(s) to remove (repeatable)
        #[arg(long = "remove-recipient", value_name = "RECIPIENT")]
        remove: Vec<String>,
        /// Identity file for decryption (default: $HOME/.config/age/keys.txt)
        #[arg(long)]
        identity: Option<PathBuf>,
    },
}

#[derive(Subcommand, Clone)]
enum HooksAction {
    /// Splice post-push / post-merge bead-sync blocks into the repo's hooks
    /// directory. The hooks dir is resolved via `git rev-parse --git-path
    /// hooks` so worktrees, submodules, and `core.hooksPath` overrides all
    /// route correctly. Existing user content outside the rsry markers is
    /// preserved.
    Install,
    /// Show whether each rsry-managed hook is installed (and where) and
    /// whether the Dolt remote is configured for bead sync.
    Status,
    /// Mechanically audit whether bead-sync config is REACHABLE and
    /// CONSISTENT, not just installed: `.gitignore` shadowing that blocks
    /// `beads.jsonl` regardless of reinstalls, `.beads/embeddeddolt/`
    /// coexisting with a live rsry backend, and local-store/tracked-export
    /// drift. Exits non-zero if any check fails — safe to script/CI against
    /// (rosary-b5c8a1).
    Audit,
    /// Execute one embedded hook's managed-block logic directly, rather
    /// than reading it out of a hook file on disk. This is the stable
    /// `entry:` target `hooks install` writes into `.pre-commit-config.yaml`
    /// for a pre-commit-framework-owned repo (rosary-00f2b5): the YAML
    /// names this command, never a version-frozen shell snippet, so an
    /// `rsry` upgrade updates the check without touching the YAML.
    Run {
        /// Hook name, e.g. `pre-commit`.
        name: String,
    },
}

#[derive(Subcommand)]
enum CoordAction {
    /// Append a single-line record to a namespace (compare-and-swap)
    Add {
        /// Namespace, conventionally the dispatch id
        name: String,
        /// The record — one line, typically JSON
        record: String,
    },
    /// Print a namespace's records
    Show {
        /// Namespace to read
        name: String,
    },
    /// List namespaces that currently exist
    List,
    /// Delete a namespace (coordination state is GC-able once folded)
    Rm {
        /// Namespace to delete
        name: String,
    },
}

#[derive(Subcommand)]
enum LatticeAction {
    /// Fold every bead's persisted observations and diff the lattice-derived
    /// status against `persist_status`. The corpus evidence that gates the R4b
    /// read-path flip: run it across a real store, and when it reads clean the
    /// fold is proven equivalent and `persist_status` can be deleted.
    Audit {
        /// Repo path containing .beads/ (defaults to current directory).
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Replay the trunk's squash-merge history into the lattice as
    /// `PipelineVerdict::Done` observations — the corpus `audit` needs.
    ///
    /// Behavior-neutral: writes `observation` events ONLY; bead state and
    /// `persist_status` are untouched. Idempotent on the commit sha, so
    /// re-running records nothing. Git witnesses the terminal MERGE, not the
    /// intermediate lifecycle — each backfilled bead gets one Done observation,
    /// not a reconstructed history.
    Backfill {
        /// Repo path containing .beads/ (defaults to current directory).
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// How many first-parent trunk commits to scan.
        #[arg(long, default_value_t = 400)]
        limit: usize,
        /// Report what would be recorded without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum BeadAction {
    /// Create a new bead
    Create {
        /// Bead title
        title: String,
        /// Description
        #[arg(short, long, default_value = "")]
        description: String,
        /// Priority (0=P0 highest, 3=P3 lowest)
        #[arg(short, long, default_value_t = 2)]
        priority: u8,
        /// Issue type
        #[arg(short = 't', long, default_value = "task")]
        issue_type: String,
        /// Source files this bead touches (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        files: Vec<String>,
        /// Test files to validate the change (comma-separated)
        #[arg(long, value_delimiter = ',')]
        test_files: Vec<String>,
        /// Close condition: how "done" is verified (a command or a resolution
        /// statement). The structured close-condition field — preferred over
        /// baking it into the description.
        #[arg(long, default_value = "")]
        acceptance: String,
        /// Skip the close-condition check (for planning/legacy beads)
        #[arg(long)]
        force: bool,
        /// Tier this bead belongs to (ADR-0022 — location derives from role).
        /// `canonical` writes to the repo's bead store and thus the git-tracked
        /// record; `coordination` writes to `refs/agents/*` and never touches
        /// the working tree.
        #[arg(long, default_value = "canonical")]
        role: String,
    },
    /// Close a bead
    Close {
        /// Bead ID
        id: String,
        /// Skip the verifiable-test-command check (for legacy/non-impl beads)
        #[arg(long)]
        force: bool,
    },
    /// Move a bead to another repo's store (cross-repo relocation; never uses bd).
    ///
    /// Reads the bead from the source store (`--repo`, default cwd), re-creates
    /// it in `<dest>`'s store with a dest-prefixed id carrying provenance +
    /// comments + status forward, then tombstones the source (closed, with a
    /// `moved →` comment). See ADR-0014 + docs/problems/rosary-capture-commit-spine.md.
    Move {
        /// Bead ID to move (short or full)
        id: String,
        /// Destination repo path containing `.beads/`
        dest: String,
    },
    /// Back up the repo's bead store to a file (restorable, full-fidelity).
    ///
    /// Distinct from `export --jsonl` (interop only). SQLite repos get a
    /// consistent `VACUUM INTO` snapshot; Dolt server-mode repos are pointed at
    /// Dolt's own backup (full history is Dolt's job). See ADR-0014.
    Backup {
        /// Destination file for the backup (must not already exist)
        output: String,
    },
    /// Restore the repo's SQLite bead store from a backup file.
    Restore {
        /// Backup file to restore from
        input: String,
        /// Overwrite an existing `.beads/beads.db`
        #[arg(long)]
        force: bool,
    },
    /// Migrate this repo's bead store from Dolt to SQLite (ADR-0021). Default is
    /// a DRY RUN: reads the source, builds a throwaway SQLite copy, verifies
    /// field-level fidelity, and reports — it changes NOTHING. `--commit`
    /// performs the atomic swap (dolt → dolt.bak backup, never deleted).
    Migrate {
        /// Target backend. Only `sqlite` is supported.
        #[arg(long, default_value = "sqlite")]
        to: String,
        /// Perform the migration for real: after a verified dry run, atomically
        /// swap `.beads/dolt` → `.beads/dolt.bak` and install the SQLite store.
        /// Without this flag, nothing is changed.
        #[arg(long)]
        commit: bool,
        /// Emit the result as JSON (repeatable diagnostic: backend, counts,
        /// cross-repo edges, stub presence, verify status).
        #[arg(long)]
        json: bool,
    },
    /// List beads with optional filters (rosary-e1c759). Defaults to the active
    /// (open) set; pass `--status all` (or a terminal status like `done`/`closed`)
    /// to include terminal beads — the active-only default is why closed/done
    /// beads were invisible here.
    List {
        /// Filter by status (all, open, in_progress, blocked, ready,
        /// dispatchable, done, closed). Repeat or comma-separate for OR
        /// semantics. `all` matches every status; `ready`, `dispatchable`, and
        /// `blocked` use the canonical `Bead::is_ready`/`is_dispatchable`/
        /// `is_blocked` predicates rather than literal string match (so
        /// `--status blocked` catches both `status="blocked"` and `status="open"`
        /// beads with unresolved deps, and `--status dispatchable` catches only
        /// beads truly safe to fan out). Terminal statuses (done/closed/…) and
        /// `all` pull the full store view instead of the active-only list.
        #[arg(short, long, value_delimiter = ',')]
        status: Vec<String>,
        /// Filter by priority (0=P0 highest, 3=P3 lowest). Repeat or
        /// comma-separate for OR semantics.
        #[arg(short, long, value_delimiter = ',')]
        priority: Vec<u8>,
        /// Filter by issue type (bug, feature, task, chore, epic, design,
        /// research, review). Repeat or comma-separate for OR semantics.
        #[arg(short = 't', long, value_delimiter = ',')]
        issue_type: Vec<String>,
        /// Shortcut for `--status ready`.
        #[arg(long, conflicts_with = "blocked")]
        ready: bool,
        /// Shortcut for `--status dispatchable` — the strict subset of `ready`
        /// that is actually safe to hand to an agent (close condition + bounded
        /// scope + refined). Use this, not `--ready`, to gate fan-out.
        #[arg(long, conflicts_with = "blocked")]
        dispatchable: bool,
        /// Shortcut for `--status blocked`.
        #[arg(long)]
        blocked: bool,
        /// Max results to return (default 50, hard-capped at 200).
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Emit JSON instead of pretty output (matches `rsry status --json`
        /// shape: `{ count, beads }`).
        #[arg(long)]
        json: bool,
    },
    /// Reopen a closed bead (sets status to `open`).
    /// Useful for recovering from accidental closures or revisiting work.
    Reopen {
        /// Bead ID
        id: String,
    },
    /// Correct a bead's recorded status — NOT a transition (rosary-e0e19f).
    ///
    /// `reopen` obeys the state machine and therefore refuses `done`, which left
    /// a wrongly-closed bead uncorrectable from any surface. This asserts the
    /// recorded state was never true, so it bypasses the transition table and
    /// requires a reason, recorded as a comment.
    Correct {
        /// Bead ID
        id: String,
        /// Target status (e.g. `open`)
        #[arg(long)]
        to: String,
        /// Why the recorded status was wrong. Required — this overrides the
        /// state machine, so the audit trail is all that explains it.
        #[arg(long)]
        reason: String,
    },
    /// Compose the agent-native review panel for a bead — bead summary +
    /// comments + workspace state + sliced change-set + evidence rollup —
    /// into one view. Phase 0 of rosary-ccd5a2 (`rsry review` substrate).
    Review {
        /// Bead ID
        id: String,
        /// Emit JSON instead of pretty text (stable schema for piping).
        #[arg(long)]
        json: bool,
    },
    /// Manage comments on a bead (add/update/delete/list)
    Comment {
        #[command(subcommand)]
        action: BeadCommentAction,
    },
    /// Search beads by title/description
    Search {
        /// Search query
        query: String,
    },
    /// Render a human-readable diff between two bead-record snapshots.
    ///
    /// Sources are resolved in this order, so bead state can be read from
    /// somewhere OTHER than the working tree — which is what makes the
    /// in-tree-vs-out-of-tree question (rosary-fa7167 Q1) answerable:
    ///
    ///   - `-`                    stdin
    ///   - `<rev>:<path>`         a git blob, e.g. `HEAD~1:.beads/beads.jsonl`
    ///   - `<ref>`                a git ref holding JSONL, e.g. `refs/beads/main`
    ///   - anything else          a file path
    ///
    /// Emits markdown suitable for a PR comment. A bead REMOVED from the
    /// record is called out loudly — that is the shape of every data-loss
    /// incident this repo has had.
    Diff {
        /// Snapshot to diff FROM (the "before" side)
        #[arg(long)]
        from: String,
        /// Snapshot to diff TO. Defaults to the repo's tracked export.
        #[arg(long, default_value = ".beads/beads.jsonl")]
        to: String,
        /// Exit non-zero when any bead was removed — for use as a CI gate.
        #[arg(long)]
        fail_on_removal: bool,
    },
    /// Export beads as JSON (for import into another rsry instance)
    Export {
        /// Filter by status (open, blocked, all). Default: open
        #[arg(short, long, default_value = "open")]
        status: String,
        /// Emit the bead JSON contract as JSONL (one bead per line, incl.
        /// dependencies + comments, carrying `schema_version`) — the format
        /// `bd init --from-jsonl` ingests. Use this for ecosystem interop /
        /// migration (ADR-0014). It is NOT a backup: like bd's own
        /// `issues.jsonl`, it carries bead *content* only, not VCS state
        /// (Dolt branches/history). For a restorable backup, copy
        /// `.beads/beads.db` (SQLite repos) or use `dolt backup` (server mode).
        /// Without `--jsonl`, the legacy lossy rosary↔rosary JSON array is emitted.
        #[arg(long)]
        jsonl: bool,
        /// Refresh only ids already present in this public JSONL projection.
        /// Local-only store records are never added.
        #[arg(long, requires = "jsonl")]
        published_from: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Import beads from a JSON file or stdin
    Import {
        /// JSON file path (reads stdin if omitted)
        file: Option<String>,
        /// Restore from the contract **JSONL** (the `bead export --jsonl`
        /// format), preserving each bead's ORIGINAL id, status, dependency
        /// edges, and comments — the bd-free `bd init --from-jsonl` equivalent
        /// (ADR-0014, rosary-9d4951). Idempotent: ids already present are
        /// skipped, never clobbered. Without this flag the input is a rosary
        /// JSON *array* and beads are re-keyed. SQLite repos only.
        #[arg(long)]
        jsonl: bool,
    },
    /// git merge driver for the tracked `.beads/beads.jsonl` export
    /// (rosary-f9516f). Not for direct human use — git invokes it via the
    /// `merge=beads-jsonl` attribute, passing `%O %A %B`.
    ///
    /// Merges the three versions **by bead record** rather than by line: a
    /// standard 3-way decision per bead id, id-sorted output written over
    /// `<ours>` (`%A`), as gitattributes(5) requires. Never picks a winner
    /// between two genuinely diverged edits — a bead both sides changed
    /// differently is emitted as a conflict block and the command exits
    /// non-zero, as does an unparseable input. Configure with
    /// `rsry hooks install`.
    MergeJsonl {
        /// `%O` — the common-ancestor version
        ancestor: String,
        /// `%A` — the current/"ours" version; the merge result is written HERE
        ours: String,
        /// `%B` — the other/"theirs" version
        theirs: String,
    },
}

/// Subcommands of `rsry bead comment` (rosary-a96b06).
///
/// `Add` is the legacy primitive (was `rsry bead comment <id> <body>` flat);
/// `List`/`Update`/`Delete` were added with the audit-trail columns. Hard
/// delete is CLI-only and gated behind `--hard` to preserve the audit-trail
/// invariant for normal flows.
#[derive(Subcommand)]
enum BeadCommentAction {
    /// Append a new comment to a bead.
    Add {
        /// Bead ID
        id: String,
        /// Comment body
        body: String,
    },
    /// List comments on a bead with their comment_ids (needed for update/delete).
    List {
        /// Bead ID
        id: String,
        /// Include soft-deleted comments in the listing
        #[arg(long)]
        include_deleted: bool,
    },
    /// Update the body of an existing comment.
    Update {
        /// Bead ID (informational; comment_id is the addressable key)
        id: String,
        /// Stable comment id (see `rsry bead comment list`)
        comment_id: String,
        /// New comment body
        #[arg(long)]
        body: String,
        /// Optional reason recorded in the audit trail
        #[arg(long)]
        reason: Option<String>,
    },
    /// Delete a comment. Soft-delete by default (preserves audit trail);
    /// `--hard` removes the row entirely.
    Delete {
        /// Bead ID (informational; comment_id is the addressable key)
        id: String,
        /// Stable comment id (see `rsry bead comment list`)
        comment_id: String,
        /// Optional reason recorded in the audit trail (soft-delete only)
        #[arg(long)]
        reason: Option<String>,
        /// Hard-delete: physically remove the row. Destroys audit trail.
        #[arg(long)]
        hard: bool,
    },
}

/// Normalize a raw repo-derived string into a safe bead-ID prefix.
///
/// Guarantees the result is non-empty and contains only `[a-z0-9_-]` with no
/// leading/trailing separators — so `generate_bead_id` can never emit a
/// malformed ID like `.-a9910e` (rosary-3f8515: an empty/`.`/path-like repo
/// name used to pass straight through). Falls back to `bead` when nothing
/// usable remains.
pub fn sanitize_prefix(input: &str) -> String {
    try_sanitize_prefix(input).unwrap_or_else(|| "bead".to_string())
}

/// Fallible core of [`sanitize_prefix`]: returns the cleaned prefix, or `None`
/// when `input` yields nothing usable (empty / `.` / whitespace / path-only).
/// Used by [`resolve_bead_prefix`] to fall through candidate sources.
fn try_sanitize_prefix(input: &str) -> Option<String> {
    // Use the last non-empty path segment so a path-like repo id ("/a/b/foo")
    // becomes "foo", not a hyphen-mangled whole path.
    let base = input.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let mut out = String::new();
    let mut prev_sep = false;
    for c in base.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() || lc == '_' {
            out.push(lc);
            prev_sep = false;
        } else if !out.is_empty() && !prev_sep {
            // collapse any run of invalid chars (incl. '.', space, unicode) to one '-'
            out.push('-');
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '_');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve a repo's bead-ID prefix SOURCE by precedence (rosary-3fcd02):
/// explicit config `bead_prefix` → repo name → git remote name → dir basename.
/// Returns the first candidate that sanitizes to something usable, already
/// cleaned; falls back to `bead` if none do. Pair with `generate_bead_id`,
/// which sanitizes again (idempotent), so callers can pass raw values.
pub fn resolve_bead_prefix(
    explicit: Option<&str>,
    repo_name: &str,
    git_remote: Option<&str>,
    dir_basename: &str,
) -> String {
    [
        explicit.unwrap_or(""),
        repo_name,
        git_remote.unwrap_or(""),
        dir_basename,
    ]
    .into_iter()
    .find_map(try_sanitize_prefix)
    .unwrap_or_else(|| "bead".to_string())
}

/// Generate a bead ID: `{prefix}-{lower 6 hex chars of millis}` (~16M values before collision).
/// The prefix is sanitized first so callers can pass raw repo names/paths safely.
pub fn generate_bead_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Per-process monotonic counter. `millis + counter` is strictly increasing
    // per process, so consecutive creates never collide — fixes the same-
    // millisecond collision (rsry-5af158's sibling rosary-b62d5f: batch import
    // / tight-loop creates hit a UNIQUE key failure). `pid` disambiguates two
    // processes that start in the same millisecond. Suffix stays 6 hex chars.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    let mixed = millis.wrapping_add(n).wrapping_add(pid << 8);
    format!("{}-{:06x}", sanitize_prefix(prefix), mixed & 0xffffff)
}

fn pr_title_with_head_bead(head_subject: &str, title: &str) -> Option<String> {
    vcs::extract_bead_ids(head_subject)
        .into_iter()
        .next()
        .map(|id| format!("[{id}] {title}"))
}

/// Capture git username from `git config user.name` at bead creation time.
/// Returns None if git is not available or user.name is not set.
/// Imprecise (self-reported) but Good Enough for team attribution.
/// Build an enriched bead description from a `BeadSpec`, appending structured
/// success criteria, cross-references, and provenance as markdown sections.
fn enrich_bead_description(spec: &bdr::decompose::BeadSpec) -> String {
    let mut desc = spec.description.clone();

    if !spec.success_criteria.is_empty() {
        desc.push_str("\n\n## Success Criteria\n\n");
        for sc in &spec.success_criteria {
            match (&sc.command, &sc.threshold) {
                (Some(cmd), _) => {
                    desc.push_str(&format!("- `{cmd}` — {}\n", sc.description));
                }
                (None, Some(threshold)) => {
                    desc.push_str(&format!("- {} (threshold: {threshold})\n", sc.description));
                }
                (None, None) => {
                    desc.push_str(&format!("- {}\n", sc.description));
                }
            }
        }
    }

    if !spec.references.is_empty() {
        desc.push_str("\n\n## References\n\n");
        for r in &spec.references {
            desc.push_str(&format!("- {r}\n"));
        }
    }

    if !spec.derived_from.is_empty() {
        desc.push_str("\n\n## Derived From\n\n");
        for src in &spec.derived_from {
            desc.push_str(&format!("- {}\n", src.label()));
        }
        if let Some(ref trace) = spec.inferred_from {
            desc.push_str(&format!(
                "\n_Classification assisted by `{}`{}_\n",
                trace.model,
                trace
                    .rationale
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ));
        }
    }

    desc
}

fn git_config_user_name(repo_root: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the .beads/ directory for a repo, handling git/jj worktrees.
/// In a worktree, .beads/ lives in the main worktree — resolve via git commondir.
/// Search every registered repo's store for `query` — the fallback when a
/// bead search runs outside any bead-tracked repo (rosary-560953). Read-only:
/// repos whose store is missing or unopenable are skipped with a warning,
/// never created.
async fn cross_repo_search(query: &str) -> Result<()> {
    let cfg = config::load_global()?;
    if cfg.repo.is_empty() {
        anyhow::bail!(
            "not inside a bead-tracked repo and no repos are registered — \
             cd into a repo, pass --repo <path>, or onboard one with `rsry init <path>`"
        );
    }
    let mut all: Vec<bead::Bead> = Vec::new();
    let mut searched = 0usize;
    for r in &cfg.repo {
        let root = scanner::resolve_repo_path(&r.path);
        let beads_dir = resolve_beads_dir(&root);
        if !beads_dir.exists() {
            continue;
        }
        match bead_sqlite::connect_bead_store(&beads_dir).await {
            Ok(store) => match store.search_beads(query, &r.name, 50).await {
                Ok(mut beads) => {
                    searched += 1;
                    all.append(&mut beads);
                }
                Err(e) => eprintln!("  [warn] search failed for {}: {e}", r.name),
            },
            Err(e) => eprintln!("  [warn] could not open store for {}: {e}", r.name),
        }
    }
    if searched == 0 {
        // Every registered repo was missing or unopenable — "nothing was
        // searched" must not masquerade as "nothing matched" (exit 0).
        anyhow::bail!(
            "no registered repo could be searched ({} registered, 0 reachable) — \
             `rsry doctor` shows per-repo store health",
            cfg.repo.len()
        );
    }
    eprintln!("(not in a bead-tracked repo — searched {searched} registered repos)");
    cli::bead_search_results(&all, query);
    Ok(())
}

/// ADR-0021 slice 4 — Dolt→SQLite bead-store migration.
///
/// Always builds the SQLite store and runs field-level `verify_migration`
/// first. Without `commit` it's a **dry run**: the built store is a throwaway
/// temp file, nothing on disk changes. With `commit`, the SAME verified build is
/// atomically swapped in (`.beads/dolt` → `.beads/dolt.bak`, never deleted) and
/// the dolt-server stopped — verify gates the swap, so a mismatch aborts leaving
/// the source untouched. `json` emits the result as a repeatable diagnostic.
async fn bead_migrate_run(
    beads_dir: &Path,
    repo_root: &Path,
    repo: &str,
    to: &str,
    commit: bool,
    json: bool,
) -> Result<()> {
    if to != "sqlite" {
        anyhow::bail!("only `--to sqlite` is supported (got `{to}`)");
    }
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| repo.to_string());

    if bead_backup::classify(beads_dir) != bead_backup::Backend::Dolt {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "repo": repo_name, "source_backend": "sqlite",
                    "migratable": false, "reason": "already SQLite",
                })
            );
        } else {
            println!(
                "bead store at {} is already SQLite — nothing to migrate",
                beads_dir.display()
            );
        }
        return Ok(());
    }

    // Build the SQLite store: for a commit, at `.beads/beads.db.new` (swapped in
    // on success); for a dry run, a throwaway temp discarded after.
    let built = if commit {
        beads_dir.join("beads.db.new")
    } else {
        std::env::temp_dir().join(format!("rsry-migrate-dryrun-{}.db", std::process::id()))
    };
    let cleanup_built = || {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", built.display()));
        }
    };
    cleanup_built(); // clear any stale build before starting

    let source = bead_sqlite::connect_bead_store(beads_dir).await?;
    let target = bead_sqlite::SqliteBeadStore::connect(&built)?;
    let result = async {
        let report = bead_migrate::migrate_store(source.as_ref(), &target, &repo_name).await?;
        bead_migrate::verify_migration(source.as_ref(), &target, &repo_name).await?;
        Ok::<_, anyhow::Error>(report)
    }
    .await;
    drop(source);
    drop(target);

    let report = match result {
        Ok(r) => r,
        Err(e) => {
            cleanup_built();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "repo": repo_name, "source_backend": "dolt",
                        "verify": "failed", "committed": false,
                        "error": format!("{e:#}"),
                    })
                );
                return Ok(());
            }
            anyhow::bail!("✗ migration verify failed — NOT migrated: {e:#}");
        }
    };
    let stub_present = crate::bead_backend::sqlite_path(beads_dir)
        .metadata()
        .map(|m| m.len() == 0)
        .unwrap_or(false);

    if commit {
        // Verify passed → perform the atomic swap. Stop the dolt-server first so
        // it isn't serving a renamed directory, then swap, then flip metadata.
        stop_dolt_server(beads_dir);
        bead_migrate::swap_dolt_to_sqlite(beads_dir, &built).context("atomic swap")?;
        if let Err(e) = bead_migrate::flip_metadata_to_sqlite(beads_dir) {
            eprintln!("[migrate] warning: metadata.json not updated: {e:#}");
        }
    } else {
        cleanup_built();
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "repo": repo_name,
                "source_backend": "dolt",
                "target_backend": "sqlite",
                "verify": "ok",
                "committed": commit,
                "beads": report.beads,
                "dependencies": report.dependencies,
                "cross_repo_dependencies": report.cross_repo_dependencies,
                "comments": report.comments,
                "beads_with_acceptance": report.beads_with_acceptance,
                "stub_present": stub_present,
            })
        );
    } else if commit {
        println!(
            "✓ MIGRATED `{repo_name}` Dolt → SQLite: {} beads, {} dependencies \
             ({} cross-repo), {} comments. Dolt store backed up at .beads/dolt.bak \
             (not deleted); dolt-server stopped.",
            report.beads, report.dependencies, report.cross_repo_dependencies, report.comments
        );
    } else {
        println!(
            "DRY RUN ✓ Dolt → SQLite verified for `{repo_name}`: {} beads, {} dependencies \
             ({} cross-repo), {} comments — field-level fidelity OK. Nothing was changed.",
            report.beads, report.dependencies, report.cross_repo_dependencies, report.comments
        );
        println!("  Run again with `--commit` to perform the swap (backs up dolt → dolt.bak).");
    }
    Ok(())
}

/// Stop the per-repo dolt-server (best-effort) after a migration swap, and clear
/// its pid/port files so nothing reconnects to the dead port.
fn stop_dolt_server(beads_dir: &Path) {
    if let Ok(pid_str) = std::fs::read_to_string(beads_dir.join("dolt-server.pid"))
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        // SIGTERM the server; ignore failure (already dead / not ours).
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    for f in ["dolt-server.pid", "dolt-server.port"] {
        let _ = std::fs::remove_file(beads_dir.join(f));
    }
}

#[derive(Debug, Default)]
struct InitSyncSummary {
    restored: usize,
    updated: usize,
    skipped_existing: usize,
    merged_closed: usize,
}

/// Rebuild a clone-local SQLite store from the tracked public projection, then
/// overlay terminal state derived from trunk merge commits.
///
/// The projection necessarily predates the merge that closes its own PR, so
/// importing JSONL alone can leave a fresh clone permanently `open`. Merge
/// history is the terminal authority. This reads only records present in the
/// projection and never exports the live store, preserving intentionally
/// scrubbed/omitted records.
async fn bootstrap_git_tracked_beads(
    repo_root: &Path,
    repo_entry: &config::RepoConfig,
) -> Result<InitSyncSummary> {
    let beads_dir = resolve_beads_dir(repo_root);
    let jsonl = beads_dir.join("beads.jsonl");
    if crate::bead_backend::is_dolt_backed(&beads_dir) || !jsonl.is_file() {
        return Ok(InitSyncSummary::default());
    }

    let store =
        bead_sqlite::SqliteBeadStore::connect(&crate::bead_backend::sqlite_path(&beads_dir))?;
    let records = restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?;
    let restored = restore::restore_beads_from_contract(&records, &store, &repo_entry.name).await?;
    let cfg = config::Config {
        repo: vec![repo_entry.clone()],
        ..Default::default()
    };
    // Suppress: these closures are re-derived from trunk history the projection
    // already reflects; republishing them would rewrite the shared file in every
    // consumer's tree on first init (tests/init_jsonl_reconciliation.rs).
    let closed = run_close_merged_local_with_config(
        &cfg,
        Some(&repo_entry.name),
        false,
        Publication::Suppress,
    )
    .await?;

    Ok(InitSyncSummary {
        restored: restored.restored,
        updated: restored.updated,
        skipped_existing: restored.skipped_existing,
        merged_closed: closed.merged_closed,
    })
}

/// Resolve a `rsry lattice` `--repo` argument to `(repo_path, repo_name)`.
///
/// The name is the lattice's `WorkRef.repo`, so `audit` and `backfill` MUST
/// derive it identically — a mismatch would make the backfill's observations
/// invisible to the fold. Sharing one helper is what enforces that.
fn resolve_lattice_repo(repo: &str) -> (PathBuf, String) {
    let repo_path = scanner::resolve_repo_path(Path::new(repo));
    let repo_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    (repo_path, repo_name)
}

pub fn resolve_beads_dir(repo_root: &Path) -> PathBuf {
    if repo_root.join(".beads").exists() {
        return repo_root.join(".beads");
    }
    // Try to find the main worktree's .beads/ via git commondir
    let git_common = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| PathBuf::from(s.trim()));
    if let Some(common) = git_common {
        let main_root = common.parent().unwrap_or(repo_root);
        main_root.join(".beads")
    } else {
        repo_root.join(".beads")
    }
}

fn daemon_pid_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rsry")
        .join("rsry.pid")
}

fn daemon_log_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rsry")
        .join("rsry.log")
}

fn read_daemon_pid() -> Option<u32> {
    let path = daemon_pid_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    // Check if process is alive via kill -0
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if status.success() {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Resolve config path: if user passed "rosary.toml" (default), check global first.
fn resolve_config(config: &str) -> String {
    if config == "rosary.toml" {
        config::resolve_config_path()
    } else {
        config.to_string()
    }
}

/// Parse a comma-separated repo filter into a set of repo names.
fn parse_repo_filter(filter: &Option<String>) -> Option<Vec<String>> {
    filter.as_ref().map(|f| {
        f.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Filter repo configs to only those matching the filter.
fn filter_repos(
    repos: &[config::RepoConfig],
    filter: &Option<Vec<String>>,
) -> Vec<config::RepoConfig> {
    match filter {
        Some(names) => repos
            .iter()
            .filter(|r| names.contains(&r.name))
            .cloned()
            .collect(),
        None => repos.to_vec(),
    }
}

#[tokio::main]
// Grandfathered (888 lines): top-level CLI command dispatch. Refactor +
// remove this allow under rosary-626db2.
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan {
            config,
            repo,
            assay,
        } => {
            let cfg = config::load_merged(&resolve_config(&config))?;
            let repo_filter = parse_repo_filter(&repo);
            let repos = filter_repos(&cfg.repo, &repo_filter);

            if assay {
                let all_plugins: Vec<_> = cfg
                    .plugins
                    .iter()
                    .cloned()
                    .chain(config::discover_plugins(None))
                    .collect();
                let registry = plugin::PluginRegistry::new(all_plugins);
                let n = scan_assay::run_assay_scan(&repos, &registry).await?;
                eprintln!("[assay] filed {n} chore bead(s) for stale refs");
            } else {
                let beads = scanner::scan_repos(&repos).await?;
                cli::scan_summary(&beads);
            }
        }
        Command::Plan { ticket } => {
            linear::plan(&ticket).await?;
        }
        Command::Sync {
            dry_run,
            repo,
            github,
        } => {
            let repo_filter = parse_repo_filter(&repo);

            if github {
                let cfg = config::load_merged(&config::resolve_config_path())?;
                let repos = filter_repos(&cfg.repo, &repo_filter);
                let beads = scanner::scan_repos(&repos).await?;
                let token = cfg
                    .github
                    .as_ref()
                    .and_then(|g| g.token.clone())
                    .or_else(|| std::env::var("GITHUB_TOKEN").ok())
                    .context("GITHUB_TOKEN not set and no github.token in config")?;
                let posted = github_mirror::sync_beads_to_github(&beads, &token).await?;
                println!("github: posted {posted} bead-context comment(s)");
                return Ok(());
            }

            // Connect hierarchy store for thread → sub-issue projection
            let sync_cfg = config::load_merged(&config::resolve_config_path())?;
            let hierarchy: Option<Box<dyn store::HierarchyStore>> =
                if let Some(ref backend_cfg) = sync_cfg.backend {
                    match backend_cfg.connect().await {
                        Ok(b) => Some(b as Box<dyn store::HierarchyStore>),
                        Err(e) => {
                            eprintln!("[sync] hierarchy unavailable ({e}), no sub-issue grouping");
                            None
                        }
                    }
                } else {
                    None
                };
            linear::sync(dry_run, repo_filter.as_deref(), hierarchy.as_deref()).await?;
        }
        Command::Status { repo, json } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let repo_filter = parse_repo_filter(&repo);
            let repos = filter_repos(&cfg.repo, &repo_filter);
            // Include terminal beads so `done`/`closed` are counted for real.
            // `scan_repos` (open-only) structurally reported done=0 — the store's
            // active filter hides closed/done, so status lied about the backlog.
            let beads = scanner::scan_repos_all(&repos).await?;
            if json {
                // Single source (ADR-0021/0006): the CLI and `rsry_status` (MCP)
                // both emit `status::status_json`, so the two surfaces can't
                // drift. `scan_repos_all` (above) includes terminal beads so
                // `done` is real, not structurally zero.
                println!("{}", status::status_json(&beads));
            } else {
                cli::print_status_summary(&beads);
                cli::print_ready_beads(&beads, 10);
            }
        }
        Command::Dispatch {
            bead_id,
            repo,
            provider,
            isolate,
        } => {
            dispatch::run(&bead_id, std::path::Path::new(&repo), isolate, &provider).await?;
        }
        Command::Run {
            config,
            concurrency,
            interval,
            once,
            dry_run,
            provider,
            overnight,
            bead,
        } => {
            // --overnight sets defaults, but explicit --concurrency/--interval override
            let concurrency = if overnight && concurrency == 3 {
                1
            } else {
                concurrency
            };
            let interval = if overnight && interval == 30 {
                120
            } else {
                interval
            };
            reconcile::run(
                &resolve_config(&config),
                concurrency,
                interval,
                once,
                dry_run,
                &provider,
                overnight,
                bead.as_deref(),
            )
            .await?;
        }
        Command::Start {
            config,
            concurrency,
            interval,
            provider,
            overnight,
        } => {
            if let Some(pid) = read_daemon_pid() {
                cli::daemon_already_running(pid);
                return Ok(());
            }

            let log_path = daemon_log_path();
            if let Some(parent) = log_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let mut args = vec![
                "run".to_string(),
                "--config".to_string(),
                resolve_config(&config),
                "--concurrency".to_string(),
                concurrency.to_string(),
                "--interval".to_string(),
                interval.to_string(),
                "--provider".to_string(),
                provider,
            ];
            if overnight {
                args.push("--overnight".to_string());
            }

            let log_file = std::fs::File::create(&log_path)?;
            // nosemgrep: blocking-subprocess-in-async — .spawn() returns immediately (non-blocking)
            let child = std::process::Command::new(std::env::current_exe()?)
                .args(&args)
                .stdout(log_file.try_clone()?)
                .stderr(log_file)
                .stdin(std::process::Stdio::null())
                .spawn()?;

            let pid = child.id();
            std::fs::write(daemon_pid_path(), pid.to_string())?;
            cli::daemon_started(pid, &log_path.to_string_lossy());
        }
        Command::Stop => {
            if let Some(pid) = read_daemon_pid() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                let _ = std::fs::remove_file(daemon_pid_path());
                cli::daemon_stopped(pid);
            } else {
                println!("No daemon running");
            }
        }
        Command::Logs => {
            let log_path = daemon_log_path();
            if log_path.exists() {
                // nosemgrep: blocking-subprocess-in-async — intentionally blocking: interactive tail -f
                let status = std::process::Command::new("tail")
                    .args(["-f", &log_path.to_string_lossy()])
                    .status()?;
                std::process::exit(status.code().unwrap_or(1));
            } else {
                println!("No log file at {}", log_path.display());
            }
        }
        Command::Serve { transport, port } => {
            serve::run(&transport, port).await?;
        }
        Command::Mcp { ipc_socket } => {
            serve::run_ipc(&ipc_socket).await?;
        }
        Command::IpcCall {
            ipc_socket,
            tool,
            args,
        } => {
            let (text, is_error) = serve::run_ipc_call(&ipc_socket, &tool, args.as_bytes()).await?;
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            if is_error {
                std::process::exit(1);
            }
        }
        Command::Enable { path, dolt } => {
            let entry = config::enable_repo(Path::new(&path))?;
            // Create the store the SAME way `rsry init` does — SQLite by
            // default (rosary-05fbe0 "SQLite = local"), Dolt only on --dolt.
            // Was: an unconditional hardcoded dolt::init_beads_db that spawned a
            // dolt-server per enabled repo regardless of intent (rosary-75af4d).
            if !entry.path.join(".beads").exists() {
                init::run(&entry.path, dolt).await?;
            }
            cli::repo_enabled(&entry.name, &entry.path.to_string_lossy());
        }
        Command::Disable { name_or_path } => match config::disable_repo(&name_or_path)? {
            Some(name) => cli::repo_disabled(&name),
            None => println!("Not found: {name_or_path}"),
        },
        Command::Approve { name } => {
            match config::set_repo_approval(&name, crate::config::DispatchApproval::Approved)? {
                Some(_) => println!("approved {name} for dispatch"),
                None => println!("Not found: {name}"),
            }
        }
        Command::Reject { name } => {
            match config::set_repo_approval(&name, crate::config::DispatchApproval::Rejected)? {
                Some(_) => {
                    println!("rejected {name} — beads from this repo will not auto-dispatch")
                }
                None => println!("Not found: {name}"),
            }
        }
        Command::ThreadReparent {
            thread_id,
            decade_id,
            name,
        } => {
            let backend_cfg = config::load_global()
                .ok()
                .and_then(|c| c.backend)
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let backend = backend_cfg
                .connect()
                .await
                .context("opening orchestrator backend")?;
            store::reparent_thread(&*backend, &thread_id, &decade_id, name.as_deref()).await?;
            println!("reparented {thread_id} → {decade_id}");
        }
        Command::Decompose {
            path,
            title,
            repo,
            dry_run,
            model,
            stub_output,
        } => {
            let markdown =
                std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;

            // Route: ADR-shaped docs use the heuristic parser.
            // Non-ADR docs with --model set use LLM extraction.
            let (atoms, meta) = if model.is_some() && !bdr::parse::is_adr_shaped(&markdown) {
                let model_name = model.as_deref().unwrap();
                let atoms = bdr_enrich::extract_atoms_with_llm(&markdown, model_name).await?;
                let meta = bdr::parse::DocMeta {
                    provenance: Some(bdr::provenance::ProvenanceRef::Doc { path: path.clone() }),
                    ..Default::default()
                };
                (atoms, meta)
            } else {
                let parsed = bdr::parse::parse_doc_full(&markdown, &path);
                (parsed.atoms, parsed.meta)
            };

            if atoms.is_empty() {
                println!("No decomposable atoms found in {path}");
                return Ok(());
            }

            let adr_title = title.unwrap_or_else(|| {
                markdown
                    .lines()
                    .find(|l: &&str| l.starts_with("# "))
                    .map(|l: &str| l.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| path.clone())
            });

            let mut decade = bdr::thread::build_decade_with_meta(&path, &adr_title, &atoms, &meta);

            // When LLM extraction was used, stamp inferred_from on every BeadSpec.
            if let Some(ref model_name) = model {
                let trace = bdr::provenance::InferenceTrace {
                    model: bdr_enrich::resolve_model_id(model_name).to_string(),
                    rationale: None,
                };
                for thread in &mut decade.threads {
                    for spec in &mut thread.beads {
                        spec.inferred_from = Some(trace.clone());
                    }
                }
            }

            cli::decompose_decade(
                &decade.title,
                &decade.id,
                &format!("{:?}", decade.status),
                decade.threads.len(),
            );
            for thread in &decade.threads {
                cli::decompose_thread(&thread.name, thread.beads.len());
                for bead_spec in &thread.beads {
                    cli::decompose_bead(
                        &bead_spec.channel.to_string(),
                        &bead_spec.title,
                        &bead_spec.issue_type,
                        bead_spec.priority,
                    );
                }
                if !thread.cross_repo_refs.is_empty() {
                    cli::decompose_refs(&thread.cross_repo_refs);
                }
            }

            if !dry_run {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let beads_dir = repo_root.join(".beads");
                let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
                let decompose_repo_name = repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                let created_by = git_config_user_name(&repo_root);

                // Connect to backend store for thread/decade assignment (best-effort).
                let backend: Option<Box<dyn store::BackendStore>> =
                    match config::load_global().ok().and_then(|c| c.backend) {
                        Some(cfg) => cfg.connect().await.ok(),
                        None => None,
                    };

                // Seed decade + threads in the backend so beads land in the lattice.
                if let Some(ref b) = backend {
                    let _ = b
                        .upsert_decade(&store::DecadeRecord {
                            id: decade.id.clone(),
                            title: decade.title.clone(),
                            source_path: path.clone(),
                            status: "active".to_string(),
                        })
                        .await;
                    for thread in &decade.threads {
                        let prefix = config::load_global()
                            .ok()
                            .and_then(|c| c.github)
                            .map(|g| g.agent_branch_prefix)
                            .unwrap_or_else(|| "rosary".to_string());
                        let feature_branch = workspace::thread_branch_name(&prefix, &thread.name);
                        let _ = b
                            .upsert_thread(&store::ThreadRecord {
                                id: thread.id.clone(),
                                name: thread.name.clone(),
                                decade_id: decade.id.clone(),
                                feature_branch: Some(feature_branch),
                            })
                            .await;
                    }
                }

                let mut created = 0;
                let mut skipped = 0;
                for thread in &decade.threads {
                    for spec in &thread.beads {
                        // Dedup: skip if a bead with the exact same title already exists.
                        let existing = client
                            .search_beads(&spec.title, &decompose_repo_name, 10)
                            .await
                            .unwrap_or_default();
                        if existing.iter().any(|b| b.title == spec.title) {
                            eprintln!("  [skip] '{}' — already exists", spec.title);
                            skipped += 1;
                            continue;
                        }

                        // Cross-repo routing: warn when target differs from --repo.
                        if let Some(ref target) = spec.target_repo
                            && target != &decompose_repo_name
                        {
                            eprintln!(
                                "  [route] '{}' → suggested repo: {target} \
                                 (creating in {decompose_repo_name})",
                                spec.title
                            );
                        }

                        // Enrich description with success criteria and references.
                        let desc = enrich_bead_description(spec);

                        let id = generate_bead_id(&decompose_repo_name);
                        let owner = dispatch::default_agent(&spec.issue_type);
                        client
                            .create_bead_full(store::NewBead {
                                id: id.clone(),
                                title: spec.title.clone(),
                                description: desc,
                                priority: spec.priority,
                                issue_type: spec.issue_type.clone(),
                                owner: owner.to_string(),
                                // file scopes set by code-reader agent post-dispatch;
                                // depends_on: ADR-level refs can't map to bead IDs yet
                                created_by: created_by.clone(),
                                derived_from: spec.derived_from.clone(),
                                acceptance_criteria: spec.close_condition_text(),
                                ..Default::default()
                            })
                            .await?;

                        // Assign to thread in backend lattice.
                        if let Some(ref b) = backend {
                            let _ = b
                                .add_bead_to_thread(
                                    &thread.id,
                                    &store::WorkRef {
                                        repo: decompose_repo_name.clone(),
                                        bead_id: id.clone(),
                                        scope: String::new(),
                                    },
                                )
                                .await;
                        }

                        created += 1;
                    }
                }
                if skipped > 0 {
                    eprintln!("  [dedup] skipped {skipped} already-existing beads");
                }
                cli::decompose_summary(created, &repo_root.to_string_lossy());
            } else {
                println!();
                println!(
                    "  {}",
                    owo_colors::OwoColorize::dimmed(&"(dry run — no beads created)")
                );
            }

            // Stub output: emit code skeletons for design review.
            if let Some(stub_repo) = stub_output {
                let target = scanner::resolve_repo_path(Path::new(&stub_repo));
                match decompose::write_stubs(&target, &decade.title, &atoms)? {
                    Some(stub_path) => {
                        println!("  stub output → {}", stub_path.display());
                        println!("  hint: commit, push, and open a draft PR for design review");
                    }
                    None => {
                        println!("  (no TechnicalSpec or Constraint atoms — nothing to stub)");
                    }
                }
            }
        }
        Command::Capture {
            from_session,
            from_code,
            symbol,
            model,
            repo,
            commit,
        } => {
            let specs = if let Some(ref transcript) = from_session {
                let opts = capture::SessionCaptureOpts {
                    transcript_path: transcript,
                    model: &model,
                };
                capture::capture_from_session(&opts).await?
            } else if from_code.len() == 2 {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let opts = capture::CodeCaptureOpts {
                    repo: &from_code[0],
                    path: &from_code[1],
                    symbol: symbol.as_deref(),
                    model: &model,
                    repo_root: &repo_root,
                };
                capture::capture_from_code(&opts).await?
            } else {
                anyhow::bail!("use --from-session <path> or --from-code <repo> <path>");
            };

            if !commit {
                println!("{}", serde_json::to_string_pretty(&specs)?);
            } else {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let beads_dir = resolve_beads_dir(&repo_root);
                let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
                let repo_name = repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                let created_by = git_config_user_name(&repo_root);
                let mut created = 0;
                for spec in &specs {
                    let desc = enrich_bead_description(spec);
                    let id = generate_bead_id(&repo_name);
                    let owner = dispatch::default_agent(&spec.issue_type);
                    client
                        .create_bead_full(store::NewBead {
                            id: id.clone(),
                            title: spec.title.clone(),
                            description: desc,
                            priority: spec.priority,
                            issue_type: spec.issue_type.clone(),
                            owner: owner.to_string(),
                            created_by: created_by.clone(),
                            derived_from: spec.derived_from.clone(),
                            acceptance_criteria: spec.close_condition_text(),
                            ..Default::default()
                        })
                        .await?;
                    created += 1;
                }
                cli::decompose_summary(created, &repo_root.to_string_lossy());
            }
        }
        Command::Bead { action, repo } => {
            // The git merge driver (rosary-f9516f) operates purely on the three
            // temp files git hands it — no store, no repo resolution. Handle it
            // before ANY repo/store machinery so it works in a bare-ish merge
            // context (rebase, `git merge-file`, CI) where cwd may not resolve
            // to a bead-tracked repo at all.
            if let BeadAction::MergeJsonl {
                ancestor,
                ours,
                theirs,
            } = &action
            {
                let out = restore::merge::merge_jsonl_files(
                    Path::new(ancestor),
                    Path::new(ours),
                    Path::new(theirs),
                )?;
                eprintln!(
                    "[merge-jsonl] {} added, {} ours-changed, {} theirs-changed, {} resurrected, {} conflicted",
                    out.added,
                    out.ours_changed,
                    out.theirs_changed,
                    out.resurrected,
                    out.conflicts.len()
                );
                if !out.is_clean() {
                    // gitattributes(5): non-zero exit = conflict (>128 would be
                    // read as the driver crashing). `%A` has already been
                    // written with both sides inside conflict blocks, so git
                    // leaves a resolvable working-tree file and stops.
                    anyhow::bail!(
                        "{} bead(s) changed on BOTH sides — refusing to discard either: {}. \
                         Resolve {} (or regenerate it with `rsry bead export --jsonl --status all \
                         -o .beads/beads.jsonl` after reconciling the stores).",
                        out.conflicts.len(),
                        out.conflicts.join(", "),
                        ours
                    );
                }
                return Ok(());
            }

            let repo_was_defaulted = repo == ".";
            let repo_root = scanner::resolve_repo_path(Path::new(&repo));
            let beads_dir = resolve_beads_dir(&repo_root);

            // rosary-560953: a bead op must never fabricate a store. Without
            // this gate, `connect_bead_store` creates an empty beads.db at
            // whatever `resolve_beads_dir` fell back to — so `bead search`
            // from a non-repo cwd silently searched a phantom store (exit 0),
            // and `bead create` could black-hole work items into a store no
            // scan reads. Store creation is explicit: `rsry init` / `enable`.
            // Backup/restore operate at the file level and must run BEFORE
            // both the store-existence gate and the store open: backup fails
            // loud on a missing store itself, and restore must be able to
            // bootstrap a missing .beads/ (fresh clone / disaster recovery) —
            // gating it would strand exactly the user it exists for.
            match &action {
                BeadAction::Backup { output } => {
                    let out = bead_backup::backup(&beads_dir, Path::new(output))?;
                    println!(
                        "backed up {} bead store → {}",
                        out.backend,
                        out.path.display()
                    );
                    return Ok(());
                }
                BeadAction::Restore { input, force } => {
                    bead_backup::restore(&beads_dir, Path::new(input), *force)?;
                    println!("restored bead store from {input}");
                    return Ok(());
                }
                BeadAction::Migrate { to, commit, json } => {
                    return bead_migrate_run(&beads_dir, &repo_root, &repo, to, *commit, *json)
                        .await;
                }
                // Id-preserving restore from contract JSONL (rosary-9d4951) —
                // handled here (like Migrate) because it needs a concrete
                // SqliteBeadStore, not the `dyn` store the post-connect path uses.
                BeadAction::Import { file, jsonl: true } => {
                    if crate::bead_backend::is_dolt_backed(&beads_dir) {
                        anyhow::bail!(
                            "import --jsonl (id-preserving restore) is SQLite-only; \
                             Dolt repos recover via `dolt backup` / branches"
                        );
                    }
                    let store = bead_sqlite::SqliteBeadStore::connect(
                        &crate::bead_backend::sqlite_path(&beads_dir),
                    )?;
                    let beads = restore::read_beads_jsonl(file.clone())?;
                    let r = restore::restore_beads_from_contract(&beads, &store, &repo).await?;
                    println!(
                        "restored {} new, updated {} (newer incoming), skipped {} (local same-or-newer) — {} deps, {} comments",
                        r.restored, r.updated, r.skipped_existing, r.dependencies, r.comments
                    );
                    return Ok(());
                }
                // Diff reads snapshots (files / git revs / refs), never the
                // store — so it must run BEFORE the store gate below. A CI
                // checkout has no `beads.db`, and requiring one would defeat
                // the point (rosary-fa7167 Q1).
                BeadAction::Diff {
                    from,
                    to,
                    fail_on_removal,
                } => {
                    let before =
                        bead_diff::parse_snapshot(&bead_diff::read_snapshot(from, &repo_root)?)
                            .with_context(|| format!("reading --from {from}"))?;
                    let after =
                        bead_diff::parse_snapshot(&bead_diff::read_snapshot(to, &repo_root)?)
                            .with_context(|| format!("reading --to {to}"))?;
                    let d = bead_diff::diff(&before, &after);
                    print!("{}", bead_diff::render_markdown(&d, from, to));
                    if *fail_on_removal && !d.removed.is_empty() {
                        anyhow::bail!(
                            "{} bead(s) removed from the record — refusing to pass",
                            d.removed.len()
                        );
                    }
                    return Ok(());
                }
                _ => {}
            }

            if !beads_dir.exists() {
                if repo_was_defaulted {
                    // Search degrades gracefully: fall back to the global
                    // registry, matching `rsry status`'s cross-repo posture.
                    if let BeadAction::Search { query } = &action {
                        return cross_repo_search(query).await;
                    }
                    anyhow::bail!(
                        "no bead store here ({} does not exist) — cd into a bead-tracked repo, \
                         pass --repo <path>, or onboard this repo with `rsry init`",
                        beads_dir.display()
                    );
                }
                anyhow::bail!(
                    "no bead store at {} — run `rsry init {}` to onboard it first",
                    beads_dir.display(),
                    repo_root.display()
                );
            }

            let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
            let repo_name = repo_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| repo.clone());

            match action {
                BeadAction::Create {
                    title,
                    description,
                    priority,
                    issue_type,
                    files,
                    test_files,
                    acceptance,
                    force,
                    role,
                } => {
                    let id = generate_bead_id(&repo_name);
                    let created_by = git_config_user_name(&repo_root);
                    // HCI adapter: clap flags → the shared op core (bead_ops).
                    let args = bead_ops::BeadCreateArgs {
                        title,
                        description,
                        priority,
                        issue_type,
                        owner: None, // CLI has no --owner; defaults to the agent
                        files,
                        test_files,
                        depends_on: vec![], // CLI doesn't support depends_on yet
                        acceptance_criteria: acceptance,
                        force,
                        role: bead_ops::parse_role(&role)?,
                    };
                    let role = args.role;
                    bead_ops::create_bead(
                        client.as_ref(),
                        &repo_root,
                        &id,
                        &args,
                        created_by.as_deref(),
                    )
                    .await?;
                    // ADR-0022: publishing is a CANONICAL-tier operation. A
                    // coordination bead lives in refs/agents/* precisely so it
                    // never enters the git-tracked record; publishing it here
                    // would undo the routing two lines above.
                    if role == bead_genesis::Role::Canonical {
                        jsonl_sync::publish_created_bead_to_tracked_jsonl(
                        client.as_ref(),
                        &id,
                        &repo_name,
                        &repo_root,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "bead {id} created locally, but publishing it to tracked .beads/beads.jsonl"
                        )
                    })?;
                    }
                    cli::bead_created(&id, &args.title);
                }
                BeadAction::Close { id, force } => {
                    bead_ops::close_bead(client.as_ref(), &id, &repo_name, force).await?;
                    jsonl_sync::refresh_tracked_beads_jsonl(
                        client.as_ref(),
                        &repo_name,
                        &repo_root,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "bead {id} closed locally, but refreshing tracked .beads/beads.jsonl"
                        )
                    })?;
                    cli::bead_closed(&id);
                }
                BeadAction::Move { id, dest } => {
                    let dest_root = scanner::resolve_repo_path(Path::new(&dest));
                    let dest_dir = resolve_beads_dir(&dest_root);
                    // Same fabrication gate as the source store (rosary-560953):
                    // moving into an un-onboarded repo would black-hole the bead
                    // into a store no scan or registry knows about.
                    if !dest_dir.exists() {
                        anyhow::bail!(
                            "destination has no bead store at {} — run `rsry init {}` first",
                            dest_dir.display(),
                            dest_root.display()
                        );
                    }
                    let dest_client = bead_sqlite::connect_bead_store(&dest_dir).await?;
                    let dest_name = dest_root
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| dest.clone());
                    let new_id = generate_bead_id(&dest_name);
                    let outcome = bead_move::move_bead(
                        client.as_ref(),
                        &repo_name,
                        dest_client.as_ref(),
                        &dest_name,
                        &id,
                        &new_id,
                    )
                    .await?;
                    println!(
                        "moved {id} → {} ({dest_name}) [status={}, {} comment(s) copied]",
                        outcome.new_id, outcome.status, outcome.comments_copied
                    );
                    if !outcome.dangling_dependencies.is_empty()
                        || !outcome.orphaned_dependents.is_empty()
                    {
                        eprintln!(
                            "⚠ cross-repo dependency edges to re-link: depends_on={:?} dependents={:?}",
                            outcome.dangling_dependencies, outcome.orphaned_dependents
                        );
                    }
                }
                // Backup/Restore/Migrate are handled before the store is opened (above).
                BeadAction::Backup { .. }
                | BeadAction::Restore { .. }
                | BeadAction::Migrate { .. } => {
                    unreachable!("backup/restore/migrate handled before connect_bead_store")
                }
                BeadAction::List {
                    mut status,
                    priority,
                    issue_type,
                    ready,
                    dispatchable,
                    blocked,
                    limit,
                    json,
                } => {
                    // Expand `--ready` / `--dispatchable` / `--blocked` into the
                    // unified `status` filter set so filter_beads has one input
                    // vector to walk.
                    if ready {
                        status.push("ready".to_string());
                    }
                    if dispatchable {
                        status.push("dispatchable".to_string());
                    }
                    if blocked {
                        status.push("blocked".to_string());
                    }
                    // Terminal beads (done/closed/rejected/stale) live outside the
                    // active list. Fetch the FULL set when the caller asks for any
                    // of them — or `all` — so `--status done` / `--status all`
                    // return rows instead of the silently-empty result the
                    // active-only list gave (the "beads you can't see" bug).
                    let wants_terminal = status.iter().any(|s| {
                        matches!(s.as_str(), "all" | "done" | "closed" | "rejected" | "stale")
                    });
                    let all = if wants_terminal {
                        client.list_all_beads(&repo_name).await?
                    } else {
                        client.list_beads(&repo_name).await?
                    };
                    let filtered = cli::filter_beads(all, &status, &priority, &issue_type, limit);
                    let capped = limit.min(200);
                    if json {
                        cli::bead_list_json(&filtered);
                    } else {
                        cli::bead_list(&filtered);
                        // Never silently truncate — say so if we hit the cap.
                        if filtered.len() == capped {
                            eprintln!("(showing first {capped}; pass --limit to see more)");
                        }
                    }
                }
                BeadAction::Reopen { id } => {
                    client.update_status(&id, "open").await?;
                    client.log_event(&id, "reopened", "via rsry-cli").await;
                    println!("reopened {id}");
                }
                BeadAction::Review { id, json } => {
                    let panel = serve::review::collect_review_for_bead(
                        client.as_ref(),
                        &repo_name,
                        &repo_root,
                        &id,
                        vec![],
                    )
                    .await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&panel)?);
                    } else {
                        cli::review_render(&panel);
                    }
                }
                BeadAction::Comment { action } => match action {
                    BeadCommentAction::Add { id, body } => {
                        bead_ops::validate_comment_body(&body)?;
                        client.add_comment(&id, &body, "rsry-cli").await?;
                        cli::bead_commented(&id);
                    }
                    BeadCommentAction::List {
                        id,
                        include_deleted,
                    } => {
                        let comments = client.list_comments(&id, include_deleted).await?;
                        if comments.is_empty() {
                            println!("(no comments on {id})");
                        } else {
                            for c in comments {
                                let edited = if c.is_edited() { " (edited)" } else { "" };
                                let deleted = if c.is_deleted() { " (deleted)" } else { "" };
                                println!(
                                    "  #{id_:<6} {when} {author}{edited}{deleted}\n      {text}",
                                    id_ = c.id,
                                    when = c.created_at.format("%Y-%m-%d %H:%M:%S"),
                                    author = c.author,
                                    text = c.text.lines().next().unwrap_or(&c.text),
                                );
                                if c.is_edited()
                                    && let Some(orig) = &c.original_text
                                {
                                    println!(
                                        "      original: {}",
                                        orig.lines().next().unwrap_or(orig)
                                    );
                                }
                                if let Some(reason) = &c.edit_reason {
                                    println!("      edit reason: {reason}");
                                }
                                if let Some(reason) = &c.delete_reason {
                                    println!("      delete reason: {reason}");
                                }
                            }
                        }
                    }
                    BeadCommentAction::Update {
                        id: _,
                        comment_id,
                        body,
                        reason,
                    } => {
                        let updated = client
                            .update_comment(&comment_id, &body, reason.as_deref())
                            .await?;
                        println!(
                            "updated comment #{comment_id} on {} (edited at {})",
                            updated.issue_id,
                            updated
                                .edited_at
                                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                                .unwrap_or_else(|| "?".to_string())
                        );
                    }
                    BeadCommentAction::Delete {
                        id: _,
                        comment_id,
                        reason,
                        hard,
                    } => {
                        if hard {
                            // Hard-delete is irreversible and destroys audit
                            // trail. Require explicit terminal confirmation.
                            use std::io::Write;
                            print!("hard-delete comment #{comment_id}? type 'yes' to confirm: ");
                            std::io::stdout().flush().ok();
                            let mut buf = String::new();
                            std::io::stdin().read_line(&mut buf)?;
                            if buf.trim() != "yes" {
                                println!("aborted (audit trail preserved)");
                                return Ok(());
                            }
                            client.hard_delete_comment(&comment_id).await?;
                            println!("hard-deleted comment #{comment_id}");
                        } else {
                            client
                                .delete_comment(&comment_id, reason.as_deref())
                                .await?;
                            println!("soft-deleted comment #{comment_id}");
                        }
                    }
                },
                // Handled before the store gate above (it reads snapshots,
                // not the store) — unreachable here, but the match must be
                // exhaustive.
                BeadAction::Diff { .. } => unreachable!("Diff is dispatched pre-store"),
                BeadAction::Search { query } => {
                    let beads = client.search_beads(&query, &repo_name, 50).await?;
                    cli::bead_search_results(&beads, &query);
                }
                BeadAction::Export {
                    status,
                    jsonl,
                    published_from,
                    output,
                } => {
                    // Full enumeration (incl. closed) so export/backup is
                    // lossless — list_beads would silently drop closed beads
                    // (rosary-91e712).
                    let beads = client.list_all_beads(&repo_name).await?;
                    let filtered: Vec<_> = match status.as_str() {
                        "all" => beads,
                        "blocked" => beads.into_iter().filter(|b| b.is_blocked()).collect(),
                        s => beads.into_iter().filter(|b| b.status == s).collect(),
                    };
                    let out = if let Some(path) = published_from {
                        let published = restore::read_beads_jsonl(Some(path))?;
                        jsonl_sync::export_published_beads_contract_jsonl(
                            &*client, &published, &repo_name,
                        )
                        .await?
                    } else if jsonl {
                        import::export_beads_contract_jsonl(&*client, &filtered).await?
                    } else {
                        serde_json::to_string_pretty(&import::export_beads_json(&filtered))?
                    };
                    match output {
                        Some(path) => {
                            std::fs::write(&path, &out)
                                .with_context(|| format!("writing export to {path}"))?;
                            eprintln!("exported {} beads to {path}", filtered.len());
                        }
                        // JSONL already terminates its last record; only the
                        // pretty-JSON branch still needs a newline added, and
                        // `println!` on the JSONL branch would emit a blank
                        // line that `wc -l` and `jq -s` both count.
                        None if out.ends_with('\n') => print!("{out}"),
                        None => println!("{out}"),
                    }
                }
                // jsonl:true (id-preserving restore) is handled pre-connect; only
                // the array/re-key path reaches here.
                BeadAction::Import { file, jsonl: _ } => {
                    let beads_json = import::read_beads_json(file)?;
                    let r = import::import_beads(&beads_json, &*client, &repo_name).await?;
                    println!(
                        "Imported {}, skipped {} (duplicate titles)",
                        r.imported, r.skipped
                    );
                }
                // Handled pre-connect (it needs no store at all) — see the
                // early return at the top of this arm.
                BeadAction::MergeJsonl { .. } => unreachable!("handled pre-connect"),
                BeadAction::Correct { id, to, reason } => {
                    bead_correct::correct_status(client.as_ref(), &id, &to, &reason).await?;
                    println!("corrected {id} → {to}");
                }
            }
        }
        Command::Lattice { action } => match action {
            LatticeAction::Audit { repo } => {
                let (repo_path, repo_name) = resolve_lattice_repo(&repo);
                let store = bead_sqlite::connect_bead_store(&resolve_beads_dir(&repo_path)).await?;
                let report = crate::observation::audit::audit_store(&*store, &repo_name).await?;
                print!("{}", report.render(&repo_name));
            }
            LatticeAction::Backfill {
                repo,
                limit,
                dry_run,
            } => {
                let (repo_path, repo_name) = resolve_lattice_repo(&repo);
                let store = bead_sqlite::connect_bead_store(&resolve_beads_dir(&repo_path)).await?;
                let report = crate::observation::backfill::backfill_repo(
                    &*store, &repo_path, &repo_name, limit, dry_run,
                )
                .await?;
                print!("{}", report.render(&repo_name));
            }
        },
        Command::Graph {
            depth,
            decade,
            orphans,
            format,
            repo,
        } => {
            let depth = match depth.as_str() {
                "decade" => graph::Depth::Decade,
                "thread" => graph::Depth::Thread,
                "bead" => graph::Depth::Bead,
                other => anyhow::bail!("unknown --depth {other} (expected decade|thread|bead)"),
            };
            let format = match format.as_str() {
                "dot" => graph::Format::Dot,
                "mermaid" => graph::Format::Mermaid,
                other => anyhow::bail!("unknown --format {other} (expected dot|mermaid)"),
            };
            let backend_cfg = config::load_global()
                .ok()
                .and_then(|c| c.backend)
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let backend = backend_cfg
                .connect()
                .await
                .context("opening orchestrator backend")?;

            // Bead metadata is best-effort — a bead whose store we can't read
            // still renders, labelled by id, rather than failing the whole
            // graph. But a degraded graph must SAY it is degraded: silently
            // swallowing the store error here would mute even the ambiguous-
            // store failure from rosary-9103f7, and the resulting id-only
            // graph would look like an accurate one.
            let mut facts = std::collections::BTreeMap::new();
            let mut warnings: Vec<String> = Vec::new();
            let repo_path = scanner::resolve_repo_path(std::path::Path::new(&repo));
            match bead_sqlite::connect_bead_store(&resolve_beads_dir(&repo_path)).await {
                Ok(store) => {
                    let repo_name = repo_path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| repo.clone());
                    match store.list_beads(&repo_name).await {
                        Ok(beads) => {
                            for b in beads {
                                facts.insert(
                                    b.id.clone(),
                                    graph::BeadFacts {
                                        title: b.title.clone(),
                                        priority: b.priority,
                                        status: b.status.clone(),
                                    },
                                );
                            }
                        }
                        Err(e) => warnings.push(format!("bead titles unavailable: {e}")),
                    }
                }
                Err(e) => warnings.push(format!("bead store unreadable: {e}")),
            }
            for w in &warnings {
                eprintln!("warning: {w}");
            }

            let spec = graph::Spec {
                depth: if orphans { graph::Depth::Bead } else { depth },
                decade,
                orphans,
            };
            let mut model = graph::build(&*backend, &spec, &facts).await?;
            model.warnings.extend(warnings);
            if model.is_empty() {
                eprintln!("warning: graph is empty (no matching decades/beads)");
            }
            print!("{}", model.render(format));
        }
        Command::Coord { action, repo } => {
            let repo_root = scanner::resolve_repo_path(std::path::Path::new(&repo));
            match action {
                CoordAction::Add { name, record } => {
                    coordination::append(&repo_root, &name, &record)?;
                    println!("appended to {}/{name}", coordination::NAMESPACE);
                }
                CoordAction::Show { name } => match coordination::read(&repo_root, &name)? {
                    Some(text) => print!("{text}"),
                    None => {
                        // "never written" is not "written and empty" — the same
                        // distinction the store/ledger drift kept collapsing.
                        eprintln!("no such coordination namespace: {name}");
                        std::process::exit(1);
                    }
                },
                CoordAction::List => {
                    let names = coordination::list(&repo_root)?;
                    if names.is_empty() {
                        eprintln!("no coordination namespaces in {}", repo_root.display());
                    }
                    for n in names {
                        println!("{n}");
                    }
                }
                CoordAction::Rm { name } => {
                    if coordination::delete(&repo_root, &name)? {
                        println!("deleted {}/{name}", coordination::NAMESPACE);
                    } else {
                        eprintln!("no such coordination namespace: {name}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::Sweep { repo, dry_run } => {
            let repo_path = scanner::resolve_repo_path(std::path::Path::new(&repo));
            let result = workspace::sweep_agent_branches(&repo_path, dry_run).await;
            if dry_run {
                println!(
                    "dry-run: {} checked, {} would delete, {} skipped (active), {} skipped (unmerged)",
                    result.checked, result.deleted, result.skipped_active, result.skipped_unmerged
                );
            } else {
                println!(
                    "sweep: {} checked, {} deleted, {} skipped (active), {} skipped (unmerged)",
                    result.checked, result.deleted, result.skipped_active, result.skipped_unmerged
                );
            }
        }
        Command::CloseMerged {
            repo,
            dry_run,
            local,
        } => {
            let verb = if dry_run { "would close" } else { "closed" };
            if local {
                let summary = run_close_merged_local(repo.as_deref(), dry_run).await?;
                println!(
                    "close-merged --local: {} {} (checked={}, held_open={})",
                    summary.merged_closed, verb, summary.checked, summary.held_open,
                );
                for id in &summary.bead_ids_closed {
                    println!("  {id}");
                }
            } else {
                let summary = run_close_merged(repo.as_deref(), dry_run).await?;
                println!(
                    "close-merged: {} {} (checked={}, no_pr_url={}, not_merged={}, gh_errors={})",
                    summary.merged_closed,
                    verb,
                    summary.checked,
                    summary.no_pr_url,
                    summary.not_merged,
                    summary.gh_errors,
                );
                for id in &summary.bead_ids_closed {
                    println!("  {id}");
                }
            }
        }
        Command::Pr {
            title,
            base,
            body_file,
            draft,
        } => {
            // Derive the `[bead-id]` from HEAD's commit subject (Golden Rule 11
            // guarantees one) and prefix the title unless it already leads with a
            // bracket — so the squash-merge subject carries the id and the
            // post-merge hook can auto-close the bead.
            let head_subject = tokio::process::Command::new("git")
                .args(["log", "-1", "--format=%s"])
                .output()
                .await
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let full_title = match pr_title_with_head_bead(&head_subject, &title) {
                Some(prefixed) if !title.trim_start().starts_with('[') => prefixed,
                _ => {
                    if title.trim_start().starts_with('[') {
                        // already carries a bracket — trust the author
                    } else {
                        eprintln!(
                            "warning: no [bead-id] found on HEAD's commit; PR title has none either"
                        );
                    }
                    title.clone()
                }
            };
            let mut args: Vec<String> = vec![
                "pr".into(),
                "create".into(),
                "--title".into(),
                full_title.clone(),
            ];
            if let Some(b) = &base {
                args.push("--base".into());
                args.push(b.clone());
            }
            if let Some(bf) = &body_file {
                args.push("--body-file".into());
                args.push(bf.clone());
            } else {
                args.push("--fill".into()); // body from commits when no file given
            }
            if draft {
                args.push("--draft".into());
            }
            let status = tokio::process::Command::new("gh")
                .args(&args)
                .status()
                .await?;
            if !status.success() {
                anyhow::bail!("gh pr create failed");
            }
            eprintln!("opened PR — title: {full_title}");
        }
        Command::Backup { output } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let backend_cfg = cfg
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let source = backend_cfg.connect_exportable().await?;
            let snapshot = migrate::export_snapshot(&*source, &backend_cfg.provider).await?;
            let rsry_dir = config::rsry_dir();
            let dir = output.unwrap_or_else(|| {
                rsry_dir
                    .join("backups")
                    .join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string())
                    .to_string_lossy()
                    .into_owned()
            });
            migrate::save_backup(&snapshot, std::path::Path::new(&dir))?;
            let counts = snapshot.counts();
            eprintln!("Backup saved to {dir}");
            eprintln!(
                "  decades={} threads={} members={} pipelines={} dispatches={} deps={} links={} repos={}",
                counts.decades,
                counts.threads,
                counts.thread_members,
                counts.pipelines,
                counts.dispatches,
                counts.dependencies,
                counts.linear_links,
                counts.user_repos
            );
        }
        Command::Migrate {
            to,
            path,
            skip_verify,
        } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let backend_cfg = cfg
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let source = backend_cfg.connect_exportable().await?;

            let rsry_dir = config::rsry_dir();
            let target_path = path.unwrap_or_else(|| match to.as_str() {
                "sqlite" => rsry_dir.join("backend.db").to_string_lossy().into_owned(),
                _ => rsry_dir.join("dolt/rosary").to_string_lossy().into_owned(),
            });
            let target_cfg = config::BackendConfig {
                provider: to.clone(),
                path: target_path.clone().into(),
            };
            let target = target_cfg.connect_or_create().await?;

            // Auto-backup before migration
            let backup_dir = rsry_dir
                .join("backups")
                .join(format!(
                    "pre-migrate-{}",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                ))
                .to_string_lossy()
                .into_owned();

            eprintln!("Migrating {} → {} ...", backend_cfg.provider, to);
            let report = migrate::migrate(
                &*source,
                &*target,
                &backend_cfg.provider,
                Some(std::path::Path::new(&backup_dir)),
            )
            .await?;

            eprintln!("Source: {:?}", report.source_counts);
            eprintln!("Target: {:?}", report.target_counts);

            if skip_verify {
                eprintln!("Verification: SKIPPED (--skip-verify)");
                eprintln!();
                eprintln!("To switch, edit ~/.rsry/config.toml:");
                eprintln!("  [backend]");
                eprintln!("  provider = \"{}\"", to);
                eprintln!("  path = \"{}\"", target_path);
            } else if report.verified {
                eprintln!("Verification: PASSED");
                eprintln!();
                eprintln!("To switch, edit ~/.rsry/config.toml:");
                eprintln!("  [backend]");
                eprintln!("  provider = \"{}\"", to);
                eprintln!("  path = \"{}\"", target_path);
            } else {
                eprintln!("Verification: FAILED — counts mismatch!");
                eprintln!("Backup at: {backup_dir}");
                std::process::exit(1);
            }
        }
        Command::Notes { action, repo } => {
            let repo_root = scanner::resolve_repo_path(Path::new(&repo));
            match action {
                NotesAction::Rotate {
                    scope,
                    add,
                    remove,
                    identity,
                } => {
                    let opts = notes::RotateOpts {
                        repo_root: &repo_root,
                        scope: &scope,
                        add_recipients: &add,
                        remove_recipients: &remove,
                        identity: identity.as_deref(),
                    };
                    let result = notes::rotate_scope(&opts).await?;
                    println!(
                        "rotated {} file(s) in notes/{} (recipients: {})",
                        result.files_rotated,
                        scope,
                        result.final_recipients.len()
                    );
                }
            }
        }
        Command::Hooks { action, repo, all } => {
            if all {
                let config = config::load_global()?;
                anyhow::ensure!(
                    !config.repo.is_empty(),
                    "no repos registered in ~/.rsry/config.toml"
                );
                let mut failures = Vec::new();
                for registered in &config.repo {
                    let repo_root = scanner::resolve_repo_path(&registered.path);
                    println!("\n== {} ({}) ==", registered.name, repo_root.display());
                    let result = match &action {
                        HooksAction::Install => hooks::install(&repo_root),
                        HooksAction::Status => hooks::status(&repo_root),
                        HooksAction::Audit => hooks::audit(&repo_root),
                        HooksAction::Run { name } => hooks::run(&repo_root, name),
                    };
                    if let Err(error) = result {
                        eprintln!("[hooks] {} failed: {error:#}", registered.name);
                        failures.push(registered.name.clone());
                    }
                }
                anyhow::ensure!(
                    failures.is_empty(),
                    "hook operation failed for: {}",
                    failures.join(", ")
                );
            } else {
                let repo = repo.unwrap_or_else(|| ".".to_string());
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                match &action {
                    HooksAction::Install => hooks::install(&repo_root)?,
                    HooksAction::Status => hooks::status(&repo_root)?,
                    HooksAction::Audit => hooks::audit(&repo_root)?,
                    HooksAction::Run { name } => hooks::run(&repo_root, name)?,
                }
            }
        }
        Command::Doctor { port } => {
            let installed = env!("CARGO_PKG_VERSION");
            println!("rsry doctor — runtime truth");
            println!(
                "  installed binary : {installed} ({})",
                env!("RSRY_BUILD_HASH")
            );

            let mut drift = false;

            // Repo version — only meaningful inside the rosary crate.
            if let Ok(toml) = std::fs::read_to_string("Cargo.toml")
                && toml.contains("name = \"rosary\"")
            {
                let repo_ver = toml
                    .lines()
                    .find_map(|l| l.strip_prefix("version = "))
                    .map(|v| v.trim().trim_matches('"'));
                if let Some(rv) = repo_ver {
                    if rv == installed {
                        println!("  repo (Cargo.toml): {rv}  ✓");
                    } else {
                        drift = true;
                        println!(
                            "  repo (Cargo.toml): {rv}  ⚠ installed binary is behind — run `task install`"
                        );
                    }
                }
            }

            // Running HTTP MCP service — probe GET / (JSON).
            let url = format!("http://localhost:{port}/");
            match reqwest::Client::new()
                .get(&url)
                .header("accept", "application/json")
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(j) => {
                        let sv = j.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                        if sv == installed {
                            println!("  running service  : {sv} on :{port}  ✓");
                        } else {
                            drift = true;
                            println!(
                                "  running service  : {sv} on :{port}  ⚠ stale — `task install` restarts it on the new binary"
                            );
                        }
                    }
                    Err(_) => {
                        println!("  running service  : reachable on :{port}, no version field")
                    }
                },
                Err(_) => println!("  running service  : not reachable on :{port} (not running?)"),
            }

            if drift {
                println!(
                    "\nDrift detected — run `task install` to bring the binary + MCP service current."
                );
            } else {
                println!("\nNo drift.");
            }

            // Config + store health (rosary-560953). Filesystem-only checks —
            // no store connects, no server spawns, no mutations: a doctor
            // that heals by accident is the bug this section exists to catch.
            let global = config::load_global();
            println!(
                "\nconfig health — {} registered repos",
                global.as_ref().map(|c| c.repo.len()).unwrap_or(0)
            );
            match global {
                Err(e) => println!("  ✗ global config unreadable: {e}"),
                Ok(cfg) if cfg.repo.is_empty() => {
                    println!("  no repos registered — `rsry init <path>` onboards one");
                }
                Ok(cfg) => {
                    let mut seen = std::collections::HashSet::new();
                    for r in &cfg.repo {
                        if !seen.insert(r.name.clone()) {
                            println!("  {:<16} ✗ duplicate name in registry", r.name);
                            continue;
                        }
                        let root = scanner::resolve_repo_path(&r.path);
                        if !root.exists() {
                            println!("  {:<16} ✗ path missing: {}", r.name, root.display());
                            continue;
                        }
                        let beads_dir = resolve_beads_dir(&root);
                        if !beads_dir.exists() {
                            println!(
                                "  {:<16} ⚠ registered but no .beads store — `rsry init {}`",
                                r.name,
                                root.display()
                            );
                            continue;
                        }
                        let detected = crate::bead_backend::detect_backend(&beads_dir);
                        let has_dolt = matches!(detected, crate::bead_backend::BeadBackend::Dolt);
                        let backend = match detected {
                            crate::bead_backend::BeadBackend::Dolt => "dolt-server",
                            crate::bead_backend::BeadBackend::Sqlite => "sqlite",
                            crate::bead_backend::BeadBackend::Ambiguous => {
                                println!(
                                    "  {:<16} ✗ ambiguous backend: both dolt/ and beads.db exist — \
                                     rsry cannot tell which is authoritative (this repo will fail \
                                     every read)",
                                    r.name
                                );
                                continue;
                            }
                            crate::bead_backend::BeadBackend::UnreadableEmbeddedOnly
                            | crate::bead_backend::BeadBackend::Uninitialized => {
                                println!(
                                    "  {:<16} ✗ .beads exists but holds neither dolt/ nor beads.db",
                                    r.name
                                );
                                continue;
                            }
                        };
                        let mut warns: Vec<String> = Vec::new();
                        if crate::bead_backend::embedded_dolt_dir(&beads_dir).exists() && !has_dolt
                        {
                            warns.push(
                                "bd-era embeddeddolt/ cruft present (store itself is fine)"
                                    .to_string(),
                            );
                        }
                        if !has_dolt
                            && let Ok(meta) =
                                std::fs::read_to_string(beads_dir.join("metadata.json"))
                            && meta.contains("\"dolt\"")
                        {
                            warns.push("metadata.json claims dolt but store is sqlite".to_string());
                        }
                        if warns.is_empty() {
                            println!("  {:<16} ✓ {backend}", r.name);
                        } else {
                            println!("  {:<16} ⚠ {backend} — {}", r.name, warns.join("; "));
                        }
                    }
                }
            }
        }
        Command::Init {
            path,
            dolt,
            no_register,
        } => {
            let repo_root = scanner::resolve_repo_path(Path::new(&path));

            // 1–3: repo-local store + metadata + managed AGENTS.md section.
            let outcome = init::run(&repo_root, dolt).await?;

            // 4: git hooks (post-merge close-merged, post-push sync, commit-msg
            // contract). Shares the same install path as `rsry hooks install`.
            hooks::install(&repo_root)?;

            // 5: register in global config so `rsry status`/scan/dispatch see it,
            // unless the caller wants a repo-local-only setup.
            let registered = if no_register {
                None
            } else {
                Some(config::enable_repo(&repo_root)?)
            };

            // 6: rebuild a clone-local SQLite store from the tracked public
            // projection, then let merge history close the PR's own bead. The
            // checked-in snapshot cannot contain a transition caused by the
            // commit that carries it (rosary-64494d).
            let repo_entry = registered.clone().unwrap_or_else(|| config::RepoConfig {
                name: repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unnamed".to_string()),
                path: repo_root.clone(),
                lang: None,
                self_managed: false,
                approval: config::DispatchApproval::Approved,
            });
            let bootstrap = bootstrap_git_tracked_beads(&repo_root, &repo_entry).await?;

            // 7: report.
            println!("\nrsry init — {}", repo_root.display());
            let store_line = match outcome.store {
                init::StoreOutcome::CreatedSqlite => "created SQLite store (.beads/beads.db)",
                init::StoreOutcome::CreatedDolt => "created Dolt store (.beads/dolt/)",
                init::StoreOutcome::AlreadyPresent => "store already present — left as-is",
            };
            println!("  store   : {store_line}");
            let agents_line = match outcome.agents {
                init::AgentsOutcome::Created => "created AGENTS.md",
                init::AgentsOutcome::SectionUpdated => "refreshed managed section in AGENTS.md",
                init::AgentsOutcome::ReplacedBdBlock => {
                    "replaced legacy bd block in AGENTS.md (routed to rsry)"
                }
                init::AgentsOutcome::AppendedSection => "appended managed section to AGENTS.md",
                init::AgentsOutcome::Unchanged => "AGENTS.md already current",
            };
            println!("  agents  : {agents_line}");
            let sync_line = match outcome.sync {
                init::SyncOutcome::Seeded => {
                    "seeded .beads/beads.jsonl — git-tracked bead sync (commit it to turn on)"
                }
                init::SyncOutcome::AlreadyPresent => {
                    "export already present (.beads/beads.jsonl) — bead sync on"
                }
                init::SyncOutcome::NotApplicableDolt => {
                    "n/a — Dolt syncs over its own remote, not git"
                }
            };
            println!("  sync    : {sync_line}");
            match registered {
                Some(entry) => println!("  config  : registered as '{}'", entry.name),
                None => println!("  config  : not registered (--no-register)"),
            }
            if bootstrap.restored + bootstrap.updated + bootstrap.skipped_existing > 0 {
                println!(
                    "  restore : {} new, {} updated, {} already current",
                    bootstrap.restored, bootstrap.updated, bootstrap.skipped_existing
                );
            }
            if bootstrap.merged_closed > 0 {
                println!(
                    "  merges  : reconciled {} merged bead(s) from trunk history",
                    bootstrap.merged_closed
                );
            }
            println!(
                "\nDone. This repo's work is now tracked as beads via rsry. Commit `.beads/` and\n\
                 AGENTS.md so collaborators get the store on clone; they run `rsry init` to wire\n\
                 up their own hooks. Create your first bead with `rsry bead create`."
            );
            if outcome.sync == init::SyncOutcome::Seeded {
                println!(
                    "\nBead sync: `.beads/beads.db` is git-IGNORED (a binary store has no 3-way\n\
                     merge), so bead state travels as `.beads/beads.jsonl` — one line per bead,\n\
                     reviewable and line-mergeable. `git add .beads/beads.jsonl` to switch it on:\n\
                     pre-commit then keeps it current and post-merge ingests peers' changes."
                );
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `rsry hooks` — git hook management for bead sync
// ---------------------------------------------------------------------------

mod hooks {
    //! Git hook management for bead sync (post-push / post-merge).
    //!
    //! Templates are embedded into the binary at compile time via `include_str!`
    //! so installation works in any environment — release images, packaged
    //! binaries, contributor checkouts — without depending on the source tree
    //! being adjacent to the executable.
    //!
    //! Installation is merge-aware: rather than clobbering existing hooks, the
    //! rsry block is spliced between the literal marker lines defined as
    //! [`MARKER_START`] and [`MARKER_END`] below. User content outside those
    //! markers is preserved across re-installs and the operation is
    //! idempotent. `MARKER_END` is intentionally short so the closing line is
    //! easy to grep for; `MARKER_START` carries the do-not-edit hint so
    //! anyone opening the hook file sees the convention without a separate
    //! README round-trip.
    //!
    //! The hooks directory is resolved via `git rev-parse --git-path hooks`
    //! so worktrees, submodules (`.git` is a file pointer to the real
    //! gitdir, not a directory), and `core.hooksPath` overrides all route to
    //! the right place.
    use crate::gitignore::{
        GitignoreShadowShape, allowlist_fix_suggestion, classify_gitignore_shadow_shape,
        remove_gitignore_line,
    };
    use crate::precommit_yaml::{is_precommit_framework_owned, merge_precommit_yaml};
    use anyhow::{Context, Result};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Begin marker line for the rsry-managed shell block inside a hook file.
    /// Anything between this line and [`MARKER_END`] is regenerated on each
    /// install. Including the do-not-edit hint on the start marker keeps the
    /// convention visible inside the file itself.
    pub(crate) const MARKER_START: &str = "# >>> rsry-managed (do not edit between these markers; `rsry hooks install` regenerates) >>>";
    /// End marker line — closes the rsry-managed section.
    pub(crate) const MARKER_END: &str = "# <<< rsry-managed <<<";

    /// Hooks rsry manages and their canonical shell-body content.
    ///
    /// Content lives in `docs/git-hooks/*` so it's reviewable alongside the
    /// code; `include_str!` bakes it into the binary so installation works
    /// in released images without `find_template_dir` style filesystem
    /// guessing.
    pub(crate) const HOOKS: &[(&str, &str)] = &[
        ("post-push", include_str!("../docs/git-hooks/post-push")),
        ("post-merge", include_str!("../docs/git-hooks/post-merge")),
        // Export half of JSONL bead sync (rosary-4ebf52): refresh the
        // git-tracked export so bead state rides with the commit. Inert unless
        // `.beads/beads.jsonl` is already tracked — opt-in by tracking.
        ("pre-commit", include_str!("../docs/git-hooks/pre-commit")),
        // The commit contract (Rule 11 + Conventional Commits) — the same body
        // that enforces at commit-msg time. Embedded so a fresh `rsry hooks
        // install` configures it without any manual symlink to ~/.rsry/hooks.
        ("commit-msg", include_str!("../docs/git-hooks/commit-msg")),
    ];

    /// Resolve the actual hooks directory for `repo_root`.
    ///
    /// Uses `git rev-parse --git-path hooks` so worktrees and submodules
    /// (where `.git` is a file pointing at the real gitdir) work correctly —
    /// the previous `repo_root.join(".git").join("hooks")` shortcut was wrong
    /// for both cases.
    pub(crate) fn resolve_hooks_dir(repo_root: &Path) -> Result<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", "--git-path", "hooks"])
            .output()
            .with_context(|| format!("invoking `git` in {}", repo_root.display()))?;
        if !out.status.success() {
            anyhow::bail!(
                "{} is not a git repo: {}",
                repo_root.display(),
                String::from_utf8_lossy(&out.stderr).trim(),
            );
        }
        let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let candidate = Path::new(&rel);
        Ok(if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            repo_root.join(candidate)
        })
    }

    /// Render install-time metadata into a hook template.
    ///
    /// Generated hooks deliberately do not contain the path of the installing
    /// binary: `cargo test`, worktrees, and staged release directories are all
    /// ephemeral. Templates resolve rsry at execution time. The version is
    /// still embedded so a hook can warn when its runtime binary differs from
    /// the binary whose templates installed it.
    pub(crate) fn render_block(block: &str) -> String {
        block.replace("__RSRY_VERSION__", env!("CARGO_PKG_VERSION"))
    }

    /// Provenance line embedded inside each managed hook block.
    ///
    /// The version identifies the binary that installed the hook. The digest
    /// identifies the exact compiled-in template, so a hook can become stale
    /// without a crate version bump and still be detected.
    fn hook_stamp(name: &str, block: &str) -> String {
        let digest = hex::encode(Sha256::digest(block.as_bytes()));
        format!(
            "# rsry-hook {name} v{} sha256:{digest}",
            env!("CARGO_PKG_VERSION")
        )
    }

    /// Render a template as the complete rsry-managed block written to disk.
    fn render_managed_block(name: &str, block: &str) -> String {
        format!("{}\n{}", hook_stamp(name, block), render_block(block))
    }

    /// Build a fresh hook file from scratch (no existing file at the path).
    /// Wraps the rsry block in `#!/bin/sh` + a brief header + markers.
    pub(crate) fn fresh_hook(block: &str) -> String {
        let mut out = String::new();
        out.push_str("#!/bin/sh\n");
        out.push_str("# Installed by `rsry hooks install`. Edit outside the rsry-managed\n");
        out.push_str("# section below to add your own logic; re-running install will\n");
        out.push_str("# regenerate only the marked block and preserve everything else.\n\n");
        out.push_str(MARKER_START);
        out.push('\n');
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(MARKER_END);
        out.push('\n');
        out
    }

    /// Splice `block` into an existing hook file's contents.
    ///
    /// - If the file already has an rsry marker section, replace just that
    ///   section. Content outside the markers (including the shebang and any
    ///   user-written shell logic) is preserved verbatim.
    /// - If the file has no marker section, append one at the end so the
    ///   user's pre-existing hook continues to run AND the rsry block runs
    ///   after it.
    pub(crate) fn merge_hook(existing: &str, block: &str) -> String {
        if let Some(start) = existing.find(MARKER_START) {
            let after_start = start + MARKER_START.len();
            // Find the matching end marker; if missing (corrupted file),
            // replace through end-of-file rather than leaving stale content.
            let end_inclusive = existing[after_start..]
                .find(MARKER_END)
                .map(|i| after_start + i + MARKER_END.len())
                .unwrap_or(existing.len());
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..start]);
            out.push_str(MARKER_START);
            out.push('\n');
            out.push_str(block);
            if !block.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER_END);
            out.push_str(&existing[end_inclusive..]);
            out
        } else {
            // Append. Leave a blank line between user content and our block
            // so the boundary is visually clear when someone `cat`s the file.
            let mut out = existing.to_string();
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(MARKER_START);
            out.push('\n');
            out.push_str(block);
            if !block.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER_END);
            out.push('\n');
            out
        }
    }

    /// Pre-commit framework hooks commonly terminate their dispatch branches
    /// with `exec`. Appending our block makes it unreachable, so install it
    /// immediately after the shebang. Existing managed blocks are relocated
    /// on reinstall while all user/framework content is preserved.
    fn merge_pre_commit_hook(existing: &str, block: &str) -> String {
        let without_managed = strip_managed_block(existing).unwrap_or_else(|| existing.to_string());
        let insertion = if without_managed.starts_with("#!") {
            without_managed
                .find('\n')
                .map(|offset| offset + 1)
                .unwrap_or(without_managed.len())
        } else {
            0
        };
        let mut out = String::with_capacity(without_managed.len() + block.len() + 4);
        out.push_str(&without_managed[..insertion]);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(MARKER_START);
        out.push('\n');
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(MARKER_END);
        out.push_str("\n\n");
        out.push_str(without_managed[insertion..].trim_start_matches('\n'));
        out
    }

    fn managed_block_is_after_exec(content: &str) -> bool {
        let Some(marker) = content.find(MARKER_START) else {
            return false;
        };
        content[..marker]
            .lines()
            .any(|line| line.split_whitespace().next() == Some("exec"))
    }

    /// Remove only the rsry-managed section from a hook, preserving user
    /// content before and after it. Used to neutralize dormant standard hooks
    /// when `core.hooksPath` points somewhere else.
    fn strip_managed_block(existing: &str) -> Option<String> {
        let start = existing.find(MARKER_START)?;
        let after_start = start + MARKER_START.len();
        let end = existing[after_start..]
            .find(MARKER_END)
            .map(|offset| after_start + offset + MARKER_END.len())
            .unwrap_or(existing.len());
        let mut stripped = String::with_capacity(existing.len());
        stripped.push_str(&existing[..start]);
        stripped.push_str(&existing[end..]);
        Some(stripped)
    }

    /// Resolve the conventional hooks directory independently of an active
    /// `core.hooksPath` override.
    fn standard_hooks_dir(repo_root: &Path) -> Result<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .with_context(|| format!("invoking `git` in {}", repo_root.display()))?;
        if !out.status.success() {
            anyhow::bail!(
                "{} is not a git repo: {}",
                repo_root.display(),
                String::from_utf8_lossy(&out.stderr).trim(),
            );
        }
        let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let common = Path::new(&rel);
        Ok(if common.is_absolute() {
            common.join("hooks")
        } else {
            repo_root.join(common).join("hooks")
        })
    }

    /// If hooks execute from a custom `core.hooksPath`, remove stale rsry
    /// managed blocks from the dormant conventional `.git/hooks` copies.
    /// User-authored content outside the markers is preserved.
    fn neutralize_inactive_standard_hooks(repo_root: &Path, active_hooks_dir: &Path) -> Result<()> {
        let standard = standard_hooks_dir(repo_root)?;
        let active = active_hooks_dir
            .canonicalize()
            .unwrap_or_else(|_| active_hooks_dir.to_path_buf());
        let standard_cmp = standard.canonicalize().unwrap_or_else(|_| standard.clone());
        if active == standard_cmp {
            return Ok(());
        }

        for (name, _) in HOOKS {
            let path = standard.join(name);
            if path
                .symlink_metadata()
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(existing) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(stripped) = strip_managed_block(&existing) else {
                continue;
            };
            std::fs::write(&path, stripped)
                .with_context(|| format!("neutralizing dormant hook at {}", path.display()))?;
            eprintln!(
                "[hooks] neutralized dormant rsry block in {} (active hooks dir: {})",
                path.display(),
                active_hooks_dir.display()
            );
        }
        Ok(())
    }

    /// Install rsry hooks into `repo_root`.
    ///
    /// Merge-aware: existing user hooks are preserved. The rsry block is
    /// (re)inserted between markers in each managed hook file. Idempotent —
    /// running install twice produces the same file content the second time.
    /// Unset a stale bd-era `core.hooksPath` (a `.beads/hooks` fossil left by
    /// `bd init`) so hooks resolve to `.git/hooks`. No-op when hooksPath is
    /// unset or points elsewhere (e.g. rosary's own `.rsry-hooks`).
    fn migrate_bd_hooks_path(repo_root: &Path) {
        let Ok(out) = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "core.hooksPath"])
            .output()
        else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let hp = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if hp.contains(".beads/hooks") {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(repo_root)
                .args(["config", "--unset", "core.hooksPath"])
                .status();
            eprintln!("[hooks] migrated off bd-era core.hooksPath ({hp}) → using .git/hooks");
        }
    }

    /// Name of the git merge driver for `.beads/beads.jsonl` — the token the
    /// root `.gitattributes` references as `merge=beads-jsonl`.
    pub(crate) const MERGE_DRIVER: &str = "beads-jsonl";

    /// Build the `merge.beads-jsonl.driver` command line (rosary-f9516f).
    ///
    /// `%O`/`%A`/`%B` are git's ancestor/ours/theirs temp paths; the driver
    /// overwrites `%A` with the result. Resolution happens when Git invokes the
    /// driver: an explicit `RSRY_BIN`, then PATH, then the conventional
    /// per-user install. No checkout/build-specific absolute path is stored in
    /// repository config.
    pub(crate) fn merge_driver_command() -> String {
        "sh -c 'r=\"${RSRY_BIN:-}\"; \
         if [ -z \"$r\" ]; then r=$(command -v rsry 2>/dev/null || true); fi; \
         if [ -z \"$r\" ] && [ -x \"$HOME/.local/bin/rsry\" ]; then r=\"$HOME/.local/bin/rsry\"; fi; \
         if [ -z \"$r\" ]; then echo \"rsry merge driver: rsry not found\" >&2; exit 1; fi; \
         exec \"$r\" bead merge-jsonl \"$@\"' - \"%O\" \"%A\" \"%B\""
            .to_string()
    }

    /// Read a single git config value from `repo_root`, `None` if unset.
    fn git_config_get(repo_root: &Path, key: &str) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "--get", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!v.is_empty()).then_some(v)
    }

    /// The `.gitattributes` line that routes the tracked export to the driver.
    pub(crate) const MERGE_ATTR_PATH: &str = ".beads/beads.jsonl";

    /// Ensure `.gitattributes` routes `.beads/beads.jsonl` at the merge driver.
    ///
    /// OPT-IN BY TRACKING, matching the pre-commit hook: only repos that have
    /// `git add`ed the export get the line. Installing hooks must never start
    /// creating a `.gitattributes` in a repo that never opted into JSONL sync.
    ///
    /// Non-clobbering: appends one line, never rewrites existing content. Uses
    /// `git check-attr` rather than grepping, so an existing rule that already
    /// routes the path — by any pattern, in any `.gitattributes` — counts.
    fn ensure_jsonl_merge_attribute(repo_root: &Path) -> Result<bool> {
        let tracked = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["ls-files", "--error-unmatch", MERGE_ATTR_PATH])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !tracked {
            return Ok(false);
        }

        // Already routed (possibly via a broader pattern)? Nothing to do.
        let routed = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["check-attr", "merge", "--", MERGE_ATTR_PATH])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(MERGE_DRIVER))
            .unwrap_or(false);
        if routed {
            return Ok(false);
        }

        let path = repo_root.join(".gitattributes");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let mut next = existing.clone();
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&format!(
            "\n# Bead export merges by RECORD, not by line (rosary-f9516f). The driver\n\
             # itself is defined in git config by `rsry hooks install` — this line only\n\
             # names it, and without it git would line-merge and shred bead records.\n\
             {MERGE_ATTR_PATH} merge={MERGE_DRIVER}\n"
        ));
        std::fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
        println!("  ✓ routed {MERGE_ATTR_PATH} → {MERGE_DRIVER} in .gitattributes");
        Ok(true)
    }

    /// Auto-fix the SIMPLE gitignore-shadow shape found by `hooks audit`;
    /// refuse-and-suggest for the ALLOWLIST shape. No-op if `.beads/`
    /// doesn't exist or `.beads/beads.jsonl` isn't actually shadowed
    /// (rosary-e97360). Called from `install()`.
    ///
    /// Self-verifying: after a real SIMPLE-shape write, re-checks
    /// `git check-ignore -q` and hard-errors if the path is STILL shadowed
    /// (e.g. a second ignore source like `.git/info/exclude` or a global
    /// gitignore is also matching) — never trusts the edit blindly.
    fn fix_gitignore_shadow(repo_root: &Path) -> Result<()> {
        let beads_dir = repo_root.join(".beads");
        if !beads_dir.exists() {
            return Ok(());
        }

        let quiet = Command::new("git")
            .current_dir(repo_root)
            .args(["check-ignore", "-q", MERGE_ATTR_PATH])
            .output();
        let GitignoreCheck::Shadowed(_) = classify_gitignore_check(quiet, None) else {
            return Ok(());
        };

        let gitignore_path = repo_root.join(".gitignore");
        let Ok(content) = std::fs::read_to_string(&gitignore_path) else {
            println!(
                "  ? {MERGE_ATTR_PATH} is gitignore-shadowed, but no readable top-level \
                 .gitignore was found to fix"
            );
            return Ok(());
        };

        match classify_gitignore_shadow_shape(&content) {
            GitignoreShadowShape::Simple { pattern } => {
                let fixed = remove_gitignore_line(&content, pattern);
                std::fs::write(&gitignore_path, &fixed)
                    .with_context(|| format!("writing {}", gitignore_path.display()))?;

                let recheck = Command::new("git")
                    .current_dir(repo_root)
                    .args(["check-ignore", "-q", MERGE_ATTR_PATH])
                    .output();
                match classify_gitignore_check(recheck, None) {
                    GitignoreCheck::Shadowed(_) => anyhow::bail!(
                        "removed `{pattern}` from .gitignore but {MERGE_ATTR_PATH} is STILL \
                         shadowed after the fix (another ignore source, e.g. \
                         .git/info/exclude or a global gitignore, is also matching) — \
                         refusing to claim success"
                    ),
                    GitignoreCheck::Unknown(e) => anyhow::bail!(
                        "removed `{pattern}` from .gitignore but could not re-verify with \
                         `git check-ignore`: {e}"
                    ),
                    GitignoreCheck::Reachable => {
                        println!(
                            "  ✓ removed shadowing rule `{pattern}` from {}",
                            gitignore_path.display()
                        );
                    }
                }
            }
            GitignoreShadowShape::Allowlist => {
                println!(
                    "  ! {MERGE_ATTR_PATH} is gitignore-shadowed by a default-deny allowlist \
                     (.gitignore has a bare `*` plus `!` exceptions) — refusing to guess \
                     which negation to add. Append to .gitignore:"
                );
                print!("{}", allowlist_fix_suggestion());
            }
            GitignoreShadowShape::Unrecognized => {
                println!(
                    "  ? {MERGE_ATTR_PATH} is gitignore-shadowed by an unrecognized \
                     .gitignore shape — not auto-fixed. Run `rsry hooks audit` for detail, \
                     then fix by hand."
                );
            }
        }
        Ok(())
    }

    /// Install the `beads-jsonl` merge driver into the repo's git config
    /// (rosary-f9516f).
    ///
    /// gitattributes(5): the driver DEFINITION must live in git config — a
    /// `.gitattributes` entry only *references* it by name. That is also why
    /// this can't be committed: config is per-clone, so every clone has to run
    /// `rsry hooks install` (the `.gitattributes` comment says so).
    ///
    /// `merge.<name>.recursive` is deliberately left unset: unset means "use
    /// this driver for the internal merges between multiple common ancestors
    /// too", which is exactly right for a driver that is itself a total,
    /// never-conflicting function of three inputs.
    ///
    /// Idempotent: `git config <key> <value>` overwrites in place, so a
    /// re-install converges on the same two keys.
    fn install_merge_driver(repo_root: &Path) -> Result<()> {
        let entries = [
            (
                format!("merge.{MERGE_DRIVER}.name"),
                "rosary bead JSONL export — union by bead id, last-writer-wins on updated_at"
                    .to_string(),
            ),
            (
                format!("merge.{MERGE_DRIVER}.driver"),
                merge_driver_command(),
            ),
        ];
        for (key, value) in entries {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo_root)
                .args(["config", &key, &value])
                .status()
                .with_context(|| format!("invoking `git config {key}`"))?;
            if !status.success() {
                anyhow::bail!("`git config {key}` failed (exit {status})");
            }
        }
        println!("[hooks] configured merge driver merge.{MERGE_DRIVER} (.beads/beads.jsonl)");
        Ok(())
    }

    pub fn install(repo_root: &Path) -> Result<()> {
        // Migrate off the stale bd-era hooks path before resolving. `bd init`
        // installed hooks into `.beads/hooks` and pointed `core.hooksPath`
        // there; ADR-0014 decoupled rosary from bd, so that dir is a fossil.
        // Unset it (local .git/config) so hooks resolve to the standard
        // `.git/hooks` instead of a tracked bd directory. (rosary's own
        // `.rsry-hooks` is left alone — only `.beads/hooks` is migrated.)
        migrate_bd_hooks_path(repo_root);

        let hooks_dir = resolve_hooks_dir(repo_root)?;
        std::fs::create_dir_all(&hooks_dir)
            .with_context(|| format!("creating {}", hooks_dir.display()))?;
        neutralize_inactive_standard_hooks(repo_root, &hooks_dir)?;

        // Detected ONCE, before any writes: the Python pre-commit framework
        // owns and regenerates .git/hooks/pre-commit on every `pre-commit
        // install`/`autoupdate`, silently dropping rsry's spliced block with
        // nothing to warn that it happened (rosary-00f2b5, found live in
        // mache). `.pre-commit-config.yaml` is the durable place a
        // pre-commit-framework repo expects a check to live instead.
        let precommit_config_path = repo_root.join(".pre-commit-config.yaml");
        let precommit_framework_owned = is_precommit_framework_owned(
            precommit_config_path.is_file(),
            std::fs::read_to_string(hooks_dir.join("pre-commit"))
                .ok()
                .as_deref(),
        );

        for (name, block) in HOOKS {
            if *name == "pre-commit" && precommit_framework_owned {
                // Instead of appending to a file the framework will
                // regenerate out from under us, redirect entirely to
                // .pre-commit-config.yaml below.
                println!(
                    "[hooks] {name} is pre-commit-framework-owned — writing to \
                     .pre-commit-config.yaml instead of the raw hook file (rosary-00f2b5)"
                );
                continue;
            }
            let dst = hooks_dir.join(name);
            // Replace a stale symlink (e.g. a hand-made commit-msg →
            // ~/.rsry/hooks/commit-msg) with a self-contained managed file —
            // never follow it and clobber the link target.
            if dst
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&dst);
            }
            let block = render_managed_block(name, block);
            let content = if dst.exists() {
                let existing = std::fs::read_to_string(&dst)
                    .with_context(|| format!("reading existing hook at {}", dst.display()))?;
                if *name == "pre-commit" {
                    merge_pre_commit_hook(&existing, &block)
                } else {
                    merge_hook(&existing, &block)
                }
            } else {
                fresh_hook(&block)
            };
            std::fs::write(&dst, &content)
                .with_context(|| format!("writing hook at {}", dst.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&dst)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&dst, perms)?;
            }
            println!("[hooks] installed {} → {}", name, dst.display());
        }

        if precommit_framework_owned {
            let existing = std::fs::read_to_string(&precommit_config_path).unwrap_or_default();
            let updated = merge_precommit_yaml(&existing);
            std::fs::write(&precommit_config_path, &updated)
                .with_context(|| format!("writing {}", precommit_config_path.display()))?;
            println!(
                "[hooks] ✓ added rsry's local hook entry to {} (entry: `rsry hooks run \
                 pre-commit`)",
                precommit_config_path.display()
            );
        }

        // The tracked `.beads/beads.jsonl` export is merged by record, not by
        // line (rosary-f9516f). The driver definition lives in git config, so
        // it must be (re)installed per clone — `.gitattributes` only names it.
        install_merge_driver(repo_root)?;

        // ...and the `.gitattributes` line that ROUTES the export to it. The
        // driver definition alone is INERT: gitattributes(5) only runs a driver
        // for paths carrying the matching `merge=` attribute, so a repo with the
        // config but no attribute silently falls back to git's LINE merge — the
        // exact record-shredding `merge_jsonl` exists to prevent.
        //
        // `hooks status` already reported this ("driver is inert"), but install
        // never wrote what it diagnosed, so every repo that didn't hand-commit a
        // `.gitattributes` was unprotected. Measured: 5 of 7 tracked repos.
        ensure_jsonl_merge_attribute(repo_root)?;

        // `hooks audit` DETECTS gitignore shadowing (rosary-b5c8a1); this
        // ACTUALLY FIXES the common case, so a human/agent never again
        // hand-edits another repo's .gitignore under time pressure
        // (rosary-e97360 — found live in 9 of 22 repos in one sweep).
        fix_gitignore_shadow(repo_root)?;

        // Warn if Dolt remote is not configured — hooks will silently no-op otherwise.
        let repo_name = repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo");
        let dolt_dir = repo_root.join(".beads").join("dolt").join(repo_name);
        if dolt_dir.exists() {
            match check_dolt_remote(&dolt_dir) {
                DoltRemoteStatus::Configured(_) => {}
                DoltRemoteStatus::NotConfigured => {
                    eprintln!(
                        "[hooks] WARNING: no dolt remote configured in {}",
                        dolt_dir.display()
                    );
                    eprintln!(
                        "[hooks] Run: cd {} && dolt remote add origin <url>",
                        dolt_dir.display()
                    );
                }
                DoltRemoteStatus::Errored { exit, stderr } => {
                    eprintln!(
                        "[hooks] WARNING: `dolt remote -v` failed in {} (exit {exit}): {}",
                        dolt_dir.display(),
                        stderr.trim()
                    );
                }
                DoltRemoteStatus::NotInvokable(e) => {
                    eprintln!("[hooks] WARNING: couldn't invoke dolt: {e}");
                }
            }
        }

        Ok(())
    }

    /// Show which rsry hooks are installed and whether Dolt remotes are configured.
    pub fn status(repo_root: &Path) -> Result<()> {
        let hooks_dir = resolve_hooks_dir(repo_root)?;
        println!("repo: {}", repo_root.display());
        println!("hooks dir: {}", hooks_dir.display());
        println!();
        println!("git hooks:");
        for (name, block) in HOOKS {
            let path = hooks_dir.join(name);
            if !path.exists() {
                println!("  ✗ not installed  {name}");
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                println!("  ! unreadable  {name}");
                continue;
            };
            if !content.contains(MARKER_START) {
                println!(
                    "  △ exists, no rsry markers  {name} (run `rsry hooks install` to merge in)"
                );
                continue;
            }

            let expected = hook_stamp(name, block);
            if name == &"pre-commit" && managed_block_is_after_exec(&content) {
                println!(
                    "  ! UNREACHABLE  {name} (managed block follows `exec`) — run `rsry hooks install`"
                );
            } else if content.lines().any(|line| line == expected) {
                println!(
                    "  ✓ current  {name} (v{}, sha256:{})",
                    env!("CARGO_PKG_VERSION"),
                    &expected[expected.len() - 64..expected.len() - 52]
                );
            } else {
                let installed = content
                    .lines()
                    .find(|line| line.starts_with("# rsry-hook "))
                    .unwrap_or("unversioned managed block");
                println!(
                    "  ! STALE  {name} ({installed}; expected v{} sha256:{}) — run `rsry hooks install`",
                    env!("CARGO_PKG_VERSION"),
                    &expected[expected.len() - 64..expected.len() - 52]
                );
            }
        }

        // Merge driver (rosary-f9516f) — config-resident, so it's per-clone
        // state that `.gitattributes` alone can't carry.
        println!();
        println!("merge driver ({MERGE_DRIVER}):");
        match git_config_get(repo_root, &format!("merge.{MERGE_DRIVER}.driver")) {
            Some(cmd) => println!("  ✓ configured: {cmd}"),
            None => println!("  ✗ not configured (run `rsry hooks install`)"),
        }
        let attrs = repo_root.join(".gitattributes");
        let referenced = std::fs::read_to_string(&attrs)
            .map(|c| c.contains(&format!("merge={MERGE_DRIVER}")))
            .unwrap_or(false);
        if referenced {
            println!("  ✓ referenced by .gitattributes");
        } else {
            println!("  △ no `merge={MERGE_DRIVER}` line in .gitattributes (driver is inert)");
        }

        let repo_name = repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo");
        let dolt_dir = repo_root.join(".beads").join("dolt").join(repo_name);
        println!();
        println!("dolt remote:");
        if !dolt_dir.exists() {
            println!("  ? no .beads/dolt/{repo_name} found");
            return Ok(());
        }
        match check_dolt_remote(&dolt_dir) {
            DoltRemoteStatus::Configured(stdout) => print!("{stdout}"),
            DoltRemoteStatus::NotConfigured => println!("  ✗ no remote configured"),
            DoltRemoteStatus::Errored { exit, stderr } => {
                println!(
                    "  ! `dolt remote -v` failed (exit {exit}): {}",
                    stderr.trim()
                );
            }
            DoltRemoteStatus::NotInvokable(e) => println!("  ? dolt not available: {e}"),
        }
        Ok(())
    }

    /// Mechanically audit whether this repo's bead-sync config is actually
    /// correct — not just installed, but REACHABLE and CONSISTENT. Three
    /// checks `status()` doesn't cover, each found live during the
    /// 2026-07-29 fleet sweep (rosary-b5c8a1):
    ///
    /// 1. **gitignore shadowing**: a top-level `.gitignore` rule (`.beads/`,
    ///    `*`, etc.) can silently block `.beads/beads.jsonl` from ever being
    ///    tracked, no matter how many times `hooks install` runs. Found live
    ///    in `lectio` (`.beads/` at line 12) and `notme.bot` (`*` at line 2).
    /// 2. **backend ambiguity**: `.beads/embeddeddolt/` (bd-era) coexisting
    ///    with `.beads/beads.db` or `.beads/dolt/` means two stores both
    ///    claim authority — rosary-909bec's exact defect.
    /// 3. **store/export drift**: the local `beads.db` row count vs the
    ///    tracked `beads.jsonl` line count. A large gap means real bead data
    ///    has never been exported and has zero durable copy. Found live: 366
    ///    beads across 9 repos, sitting only on one machine's disk in a
    ///    gitignored file.
    ///
    /// Execute one embedded hook's managed-block logic directly, by
    /// rendering the SAME `docs/git-hooks/<name>` template `install`
    /// splices into a raw hook file and running it via `sh -c` in
    /// `repo_root` (rosary-00f2b5). Propagates the script's exit status —
    /// any non-zero code is returned as an error, matching how git itself
    /// would treat a failing hook.
    ///
    /// This is the stable target `hooks install` writes into
    /// `.pre-commit-config.yaml`'s `entry:` for a pre-commit-framework-owned
    /// repo: the YAML names this COMMAND, never a version-frozen shell
    /// snippet, so an `rsry` upgrade updates the check without ever
    /// touching the YAML again.
    pub fn run(repo_root: &Path, name: &str) -> Result<()> {
        let (_, block) = HOOKS
            .iter()
            .find(|(n, _)| *n == name)
            .with_context(|| format!("unknown hook: {name} (known: {:?})", hook_names()))?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(render_block(block))
            .current_dir(repo_root)
            .status()
            .with_context(|| format!("running hook `{name}`"))?;
        if !status.success() {
            anyhow::bail!(
                "hook `{name}` exited {}",
                status
                    .code()
                    .map_or("with a signal".to_string(), |c| c.to_string())
            );
        }
        Ok(())
    }

    fn hook_names() -> Vec<&'static str> {
        HOOKS.iter().map(|(n, _)| *n).collect()
    }

    /// Unlike `status()` (purely informational), this is a GATE: returns
    /// `Err` naming every failing check if any check fails, so `rsry hooks
    /// audit` exits non-zero and is safe to script/CI against.
    pub fn audit(repo_root: &Path) -> Result<()> {
        let mut problems = Vec::new();
        let beads_dir = repo_root.join(".beads");
        let jsonl_rel = ".beads/beads.jsonl";

        // --- 1. gitignore shadowing ----------------------------------------
        if beads_dir.exists() {
            let quiet = Command::new("git")
                .current_dir(repo_root)
                .args(["check-ignore", "-q", jsonl_rel])
                .output();
            // `-v`'s exit code means "some rule (possibly a negation) decided
            // the path" — NOT "is ignored" (verified live against notme.bot's
            // default-deny allowlist: `-v` exits 0 on the deciding `!pattern`
            // line even though the path is NOT ignored). Only `-q`'s exit code
            // has gitignore(5)'s real ignored/not-ignored semantics; `-v` is
            // fetched purely for the human-readable detail, only when needed.
            let verbose_detail = if matches!(&quiet, Ok(o) if o.status.success()) {
                Command::new("git")
                    .current_dir(repo_root)
                    .args(["check-ignore", "-v", jsonl_rel])
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            };
            match classify_gitignore_check(quiet, verbose_detail) {
                GitignoreCheck::Shadowed(detail) => {
                    println!("  ✗ GITIGNORE-SHADOWED: {jsonl_rel} is blocked — {detail}");
                    problems.push(format!("{jsonl_rel} is gitignore-shadowed: {detail}"));
                }
                GitignoreCheck::Reachable => {
                    println!("  ✓ {jsonl_rel} reachable through .gitignore");
                }
                GitignoreCheck::Unknown(e) => {
                    println!("  ? could not run git check-ignore: {e}");
                }
            }
        }

        // --- 2. backend ambiguity -------------------------------------------
        let sqlite_db = crate::bead_backend::sqlite_path(&beads_dir);
        let backend = crate::bead_backend::detect_backend(&beads_dir);
        let has_sqlite = matches!(backend, crate::bead_backend::BeadBackend::Sqlite);
        if backend.is_ambiguous() {
            println!(
                "  ✗ BACKEND-AMBIGUOUS: {} and {} both exist — two stores claim authority",
                crate::bead_backend::dolt_dir(&beads_dir).display(),
                sqlite_db.display()
            );
            problems.push(format!(
                "{} and {} both exist — ambiguous backend",
                crate::bead_backend::dolt_dir(&beads_dir).display(),
                sqlite_db.display()
            ));
        } else if beads_dir.exists() {
            println!("  ✓ no ambiguous backend coexistence");
        }

        // --- 3. store/export drift -------------------------------------------
        if has_sqlite {
            match count_sqlite_issues(&sqlite_db) {
                Ok(db_count) => {
                    let jsonl_lines = std::fs::read_to_string(beads_dir.join("beads.jsonl"))
                        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                        .unwrap_or(0);
                    if store_export_drifted(db_count, jsonl_lines) {
                        println!(
                            "  ✗ STORE/EXPORT DRIFT: beads.db has {db_count} bead(s), beads.jsonl has {jsonl_lines} line(s) — no durable copy"
                        );
                        problems.push(format!(
                            "{db_count} bead(s) in beads.db, only {jsonl_lines} in beads.jsonl"
                        ));
                    } else {
                        println!(
                            "  ✓ store/export roughly agree (beads.db={db_count}, beads.jsonl={jsonl_lines} line(s))"
                        );
                    }
                }
                Err(e) => println!("  ? could not read beads.db to check drift: {e}"),
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "{} problem(s) found: {}",
                problems.len(),
                problems.join("; ")
            )
        }
    }

    /// Result of classifying a `git check-ignore -v` probe on
    /// `.beads/beads.jsonl`. Mirrors [`DoltRemoteStatus`]'s shape: a pure
    /// classifier over `io::Result<Output>` so it's unit- and
    /// property-testable without spawning git.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum GitignoreCheck {
        /// `check-ignore` matched — the export is unreachable regardless of
        /// how many times hooks are (re)installed. Carries the matching
        /// rule (`<file>:<line>:<pattern>\t<path>`).
        Shadowed(String),
        /// `check-ignore` found no match — the path is trackable.
        Reachable,
        /// The `git` binary itself couldn't be spawned.
        Unknown(String),
    }

    /// Classify a `git check-ignore -q <path>` invocation — `-q`'s exit code
    /// is the one with real gitignore(5) ignored/not-ignored semantics (0 =
    /// ignored, 1 = not ignored, anything higher is an error). `-v`'s exit
    /// code is NOT equivalent: it reports 0 whenever any rule, including a
    /// `!negation`, decided the path — so a `-v`-only classifier misreports
    /// an explicitly un-ignored path as shadowed (found live against
    /// notme.bot's default-deny allowlist, 2026-07-29). `verbose_detail` is
    /// the caller's separately-fetched `-v` output, attached to `Shadowed`
    /// purely for the human-readable rule; never used to make the decision.
    pub(crate) fn classify_gitignore_check(
        result: std::io::Result<std::process::Output>,
        verbose_detail: Option<String>,
    ) -> GitignoreCheck {
        match result {
            Err(e) => GitignoreCheck::Unknown(e.to_string()),
            Ok(out) if out.status.success() => {
                GitignoreCheck::Shadowed(verbose_detail.unwrap_or_default())
            }
            Ok(_) => GitignoreCheck::Reachable,
        }
    }

    /// Has the local store outrun its tracked export badly enough that real
    /// bead data has no durable copy? Lines needn't match exactly (status
    /// filters, in-flight writes change the count run to run) — this states
    /// the boundary as a threshold shape, not a hardcoded magic number, so
    /// the property tests characterize it independent of the exact ratio.
    ///
    /// Laws: an empty store never drifts (nothing to lose); a nonempty store
    /// with zero exported lines always drifts (the exact incident this
    /// check exists for — 366 beads, 9 repos, 2026-07-29); drift is
    /// monotonic in `jsonl_lines` — exporting more can only cure a flagged
    /// state, never cause one; an export meeting or exceeding the store
    /// count never drifts.
    pub(crate) fn store_export_drifted(db_count: i64, jsonl_lines: usize) -> bool {
        db_count > 0 && (jsonl_lines == 0 || jsonl_lines * 2 < db_count as usize)
    }

    /// Row count of the `issues` table in a bead SQLite store, opened
    /// read-only so an audit run can never itself mutate or lock the store.
    fn count_sqlite_issues(path: &Path) -> Result<i64> {
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("opening {}", path.display()))?;
        conn.query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
            .context("counting issues")
    }

    /// Result of probing `dolt remote -v` in a Dolt-backed bead directory.
    ///
    /// Distinguishes "command ran cleanly with no remote" from "command
    /// failed" — the previous code lumped both into "no remote configured"
    /// and hid real errors (e.g. corrupted repo, unsupported Dolt version).
    pub(crate) enum DoltRemoteStatus {
        /// `dolt remote -v` exited 0 with non-empty stdout.
        Configured(String),
        /// `dolt remote -v` exited 0 with empty stdout — truly no remote.
        NotConfigured,
        /// `dolt remote -v` exited non-zero; preserve stderr for the user.
        Errored { exit: i32, stderr: String },
        /// The `dolt` binary couldn't be spawned (missing, no exec perm).
        NotInvokable(String),
    }

    /// Classify a `dolt remote -v` invocation result. Pure function over the
    /// command output so it can be unit-tested without an actual `dolt`
    /// binary.
    pub(crate) fn classify_dolt_remote(
        result: std::io::Result<std::process::Output>,
    ) -> DoltRemoteStatus {
        match result {
            Err(e) => DoltRemoteStatus::NotInvokable(e.to_string()),
            Ok(out) if !out.status.success() => DoltRemoteStatus::Errored {
                exit: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Ok(out) if out.stdout.is_empty() => DoltRemoteStatus::NotConfigured,
            Ok(out) => {
                DoltRemoteStatus::Configured(String::from_utf8_lossy(&out.stdout).into_owned())
            }
        }
    }

    /// Run `dolt remote -v` in the given directory and classify the result.
    fn check_dolt_remote(dolt_dir: &Path) -> DoltRemoteStatus {
        let result = Command::new("dolt")
            .args(["remote", "-v"])
            .current_dir(dolt_dir)
            .output();
        classify_dolt_remote(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use proptest::prelude::*;
        use std::process::Command;

        /// Run `git` with a genuinely-isolated env so the host's gitconfig
        /// can't leak into the test (commit.gpgsign, core.hooksPath, user
        /// identity, etc.). We override:
        ///
        /// - `HOME` → empty tempdir so `$HOME/.gitconfig` is a fresh file
        /// - `GIT_CONFIG_GLOBAL` → /dev/null on Unix so the global file is
        ///   forced empty regardless of HOME
        /// - `GIT_CONFIG_NOSYSTEM=1` → skip `/etc/gitconfig`
        ///
        /// Each call gets its own scratch HOME so tests don't share state.
        fn git(dir: &Path, args: &[&str]) -> std::process::Output {
            // Use the existing dir for HOME; git only writes to ~/.gitconfig
            // when called with `config --global`, which we never do. Pointing
            // HOME at a tempdir under our control is sufficient isolation.
            let home = tempfile::tempdir().expect("HOME tempdir");
            Command::new("git")
                .current_dir(dir)
                .env_clear()
                .env("HOME", home.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                // Preserve PATH so git itself and its subcommands can be found.
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .args(args)
                .output()
                .expect("spawn git")
        }

        fn init_repo(dir: &Path) {
            assert!(git(dir, &["init", "-q", "-b", "main"]).status.success());
            // user.email / user.name go into THIS repo's local config, not
            // global — so they don't need the env scaffolding above to take
            // effect, but they also don't hurt.
            assert!(
                git(dir, &["config", "user.email", "test@example.invalid"])
                    .status
                    .success()
            );
            assert!(git(dir, &["config", "user.name", "test"]).status.success());
            assert!(
                git(dir, &["config", "commit.gpgsign", "false"])
                    .status
                    .success()
            );
        }

        fn seed_commit(dir: &Path) {
            std::fs::write(dir.join("seed"), "x").unwrap();
            assert!(git(dir, &["add", "seed"]).status.success());
            assert!(git(dir, &["commit", "-q", "-m", "seed"]).status.success());
        }

        // --- .gitattributes routing ---------------------------------------

        /// The driver config alone is INERT — git only runs it for paths
        /// carrying the `merge=` attribute. This asserts the attribute is what
        /// actually flips, via `git check-attr` (the thing git consults), not
        /// by grepping the file we just wrote.
        #[test]
        fn routes_tracked_export_and_is_idempotent() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);

            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

            // Untracked export => opt-in not taken => no file written.
            assert!(!ensure_jsonl_merge_attribute(root).unwrap());
            assert!(!root.join(".gitattributes").exists());

            assert!(git(root, &["add", MERGE_ATTR_PATH]).status.success());
            assert!(ensure_jsonl_merge_attribute(root).unwrap());

            let attr = git(root, &["check-attr", "merge", "--", MERGE_ATTR_PATH]);
            assert!(
                String::from_utf8_lossy(&attr.stdout).contains(MERGE_DRIVER),
                "export must route to the driver, else git line-merges it"
            );

            // Second run is a no-op — no duplicate lines.
            assert!(!ensure_jsonl_merge_attribute(root).unwrap());
            let body = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
            assert_eq!(body.matches(MERGE_DRIVER).count(), 1);
        }

        /// Pre-existing content is appended to, never clobbered.
        #[test]
        fn preserves_existing_gitattributes() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".gitattributes"), "*.png binary\n").unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();
            assert!(git(root, &["add", MERGE_ATTR_PATH]).status.success());

            assert!(ensure_jsonl_merge_attribute(root).unwrap());
            let body = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
            assert!(body.contains("*.png binary"), "clobbered user content");
            assert!(body.contains(MERGE_DRIVER));
        }

        /// The canonical `.beads/.gitignore` must deny the migration backup —
        /// `migrate --commit` renames dolt/ to dolt.bak/ and never deletes it,
        /// and it was staged accidentally once.
        #[test]
        fn beads_gitignore_denies_migration_backup() {
            assert!(crate::init::BEADS_GITIGNORE.contains("dolt.bak/"));
        }

        // --- gitignore-shadow auto-fix (rosary-e97360) ---------------------
        //
        // Pure classification/removal logic (classify_gitignore_shadow_shape,
        // remove_gitignore_line, allowlist_fix_suggestion) lives in
        // src/gitignore.rs with its own unit tests — kept as a standalone
        // file specifically so it can be mutation-tested in isolation
        // (`task mutants:gitignore`) without main.rs's unrelated noise.
        // What stays here is the I/O-level integration: does
        // `fix_gitignore_shadow` actually read/write/re-verify correctly.

        /// REAL FIXTURE 1 (lectio, SIMPLE shape) — the actual lines found at
        /// .gitignore:10-12 before the fix, hand-applied this session.
        /// Verified live via `git check-ignore -q` at the time; this test
        /// pins the automated version of that same fix.
        #[test]
        fn fix_gitignore_shadow_simple_shape_removes_rule_and_self_verifies() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(
                root.join(".gitignore"),
                "/target\n\
                 **/target/\n\
                 \n\
                 # Local rosary bead store (Dolt DB + daemon token) — local only, never publish.\n\
                 # (Also covered by ~/.gitignore_global; repeated here so the repo is self-contained.)\n\
                 .beads/\n",
            )
            .unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

            // Precondition: genuinely shadowed before the fix.
            assert!(
                Command::new("git")
                    .current_dir(root)
                    .args(["check-ignore", "-q", MERGE_ATTR_PATH])
                    .status()
                    .unwrap()
                    .success(),
                "fixture must start shadowed"
            );

            fix_gitignore_shadow(root).unwrap();

            let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(!body.lines().any(|l| l.trim() == ".beads/"), "{body}");
            // Comments and unrelated rules survive untouched.
            assert!(body.contains("/target"));
            assert!(body.contains("Local rosary bead store"));

            // Self-verification means this must ACTUALLY be reachable now,
            // not just "the line is gone" — proves the fix, not the edit.
            assert!(
                !Command::new("git")
                    .current_dir(root)
                    .args(["check-ignore", "-q", MERGE_ATTR_PATH])
                    .status()
                    .unwrap()
                    .success(),
                "must be reachable after the fix"
            );
        }

        /// REAL FIXTURE 2 (notme.bot, ALLOWLIST shape) — the actual
        /// default-deny structure found this session. Must NOT be
        /// auto-edited; must print the exact two-step suggestion and leave
        /// the file untouched.
        #[test]
        fn fix_gitignore_shadow_allowlist_shape_refuses_and_does_not_write() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            let original = "# Default deny: ignore everything unless explicitly allowlisted below.\n\
                 *\n\
                 \n\
                 # Keep gitignore itself tracked.\n\
                 !.gitignore\n\
                 \n\
                 # Allowlist project source and metadata.\n\
                 !LICENSE\n\
                 !README.md\n\
                 !src/\n\
                 !src/**\n\
                 \n\
                 # Explicitly ignore local/runtime artifacts.\n\
                 .dolt/\n\
                 *.db\n\
                 .DS_Store\n";
            std::fs::write(root.join(".gitignore"), original).unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

            fix_gitignore_shadow(root).unwrap();

            let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert_eq!(body, original, "allowlist .gitignore must be untouched");
        }

        /// Self-verification is the point, not decoration: a SIMPLE-shape
        /// removal that STILL leaves the path shadowed (here, via a second
        /// ignore source — `.git/info/exclude` — carrying the same rule)
        /// must hard-error rather than report success.
        #[test]
        fn fix_gitignore_shadow_hard_errors_if_still_shadowed_after_fix() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".gitignore"), "# comment\n.beads/\n").unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();
            // A second, independent ignore source the fix cannot touch.
            std::fs::create_dir_all(root.join(".git/info")).unwrap();
            std::fs::write(root.join(".git/info/exclude"), ".beads/\n").unwrap();

            let err = fix_gitignore_shadow(root).unwrap_err();
            assert!(format!("{err:#}").contains("STILL shadowed"), "{err:#}");
            // The .gitignore edit itself still happened (the fix isn't
            // rolled back) — the hard error is about not CLAIMING success,
            // not about leaving the repo in a worse state.
            let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(!body.lines().any(|l| l.trim() == ".beads/"));
        }

        #[test]
        fn fix_gitignore_shadow_noop_when_not_shadowed() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

            fix_gitignore_shadow(root).unwrap();
            let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert_eq!(body, "target/\n");
        }

        #[test]
        fn fix_gitignore_shadow_noop_when_no_beads_dir() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".gitignore"), ".beads/\n").unwrap();
            // No .beads/ directory created — nothing to fix, must not panic
            // or create one as a side effect.
            fix_gitignore_shadow(root).unwrap();
            assert!(!root.join(".beads").exists());
        }

        /// `install()` end-to-end: the fix runs as part of the normal
        /// install flow, not just when called directly.
        #[test]
        fn install_fixes_simple_gitignore_shadow() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".gitignore"), ".beads/\n").unwrap();
            std::fs::create_dir_all(root.join(".beads")).unwrap();
            std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

            install(root).unwrap();

            let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(!body.lines().any(|l| l.trim() == ".beads/"), "{body}");
        }

        // --- pre-commit-framework integration (rosary-00f2b5) --------------
        //
        // Pure detection/YAML-editing logic (is_precommit_framework_owned,
        // merge_precommit_yaml) lives in src/precommit_yaml.rs with its own
        // unit tests, for the same mutation-testing reason as gitignore.rs.
        // What stays here is I/O-level: does `install()` actually redirect
        // correctly, and does `hooks run` actually execute.

        /// The core behavior change: a framework-owned repo (signaled by a
        /// present `.pre-commit-config.yaml`) gets its `pre-commit` entry
        /// redirected to the YAML, never spliced into the raw hook file —
        /// while every OTHER managed hook still installs normally.
        #[test]
        fn install_redirects_precommit_to_yaml_when_framework_owned() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(
                root.join(".pre-commit-config.yaml"),
                "repos:\n  - repo: https://github.com/psf/black\n    rev: 24.0\n",
            )
            .unwrap();

            install(root).unwrap();

            let hooks_dir = root.join(".git").join("hooks");
            let raw_precommit =
                std::fs::read_to_string(hooks_dir.join("pre-commit")).unwrap_or_default();
            assert!(
                !raw_precommit.contains(MARKER_START),
                "must NOT splice into the raw file when framework-owned: {raw_precommit}"
            );

            let yaml = std::fs::read_to_string(root.join(".pre-commit-config.yaml")).unwrap();
            assert!(yaml.contains("entry: rsry hooks run pre-commit"), "{yaml}");
            assert!(
                yaml.contains("psf/black"),
                "pre-existing entry lost: {yaml}"
            );

            // Every OTHER hook is unaffected — still gets the normal raw-file
            // treatment.
            let post_push = std::fs::read_to_string(hooks_dir.join("post-push")).unwrap();
            assert!(post_push.contains(MARKER_START));
        }

        /// Regression pin: an ordinary (non-framework) repo is completely
        /// unaffected by this bead — ensures the new detection didn't
        /// change existing behavior for the common case.
        #[test]
        fn install_still_uses_raw_file_when_not_framework_owned() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);

            install(root).unwrap();

            let raw_precommit =
                std::fs::read_to_string(root.join(".git/hooks/pre-commit")).unwrap();
            assert!(raw_precommit.contains(MARKER_START));
            assert!(!root.join(".pre-commit-config.yaml").exists());
        }

        /// The fallback signal: no `.pre-commit-config.yaml` at all, but the
        /// EXISTING raw hook already shows the framework's generated shape
        /// (the exact live signature observed in mache). `install()` must
        /// still redirect — and since no config file existed, creates a
        /// minimal one rather than erroring.
        #[test]
        fn install_detects_framework_ownership_via_existing_hook_marker() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            let hooks_dir = root.join(".git").join("hooks");
            std::fs::create_dir_all(&hooks_dir).unwrap();
            std::fs::write(
                hooks_dir.join("pre-commit"),
                "#!/usr/bin/env bash\n\
                 # File generated by pre-commit: https://pre-commit.com\n\
                 # ID: 138fd403232d2ddd5efb44317e38bf03\n\
                 exec pre-commit hook-impl --hook-type=pre-commit \"$@\"\n",
            )
            .unwrap();

            install(root).unwrap();

            let yaml = std::fs::read_to_string(root.join(".pre-commit-config.yaml")).unwrap();
            assert!(yaml.contains("entry: rsry hooks run pre-commit"), "{yaml}");
            assert!(yaml.starts_with("repos:\n"), "{yaml}");
        }

        /// The acceptance criterion, directly: simulate a fresh
        /// `pre-commit install` regenerating `.git/hooks/pre-commit` from
        /// scratch AFTER rsry's install ran. `.pre-commit-config.yaml` is a
        /// separate file regeneration never touches — this proves rsry's
        /// check survives, rather than asserting anything about pre-commit's
        /// own dispatch (which would require the real `pre-commit` binary).
        #[test]
        fn precommit_yaml_survives_a_simulated_framework_regeneration() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            std::fs::write(root.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();

            install(root).unwrap();
            let after_install =
                std::fs::read_to_string(root.join(".pre-commit-config.yaml")).unwrap();
            assert!(after_install.contains("entry: rsry hooks run pre-commit"));

            // Simulate `pre-commit install` regenerating the raw hook file —
            // the exact framework-only shape observed live, zero rsry
            // markers.
            let hooks_dir = root.join(".git").join("hooks");
            std::fs::write(
                hooks_dir.join("pre-commit"),
                "#!/usr/bin/env bash\n\
                 # File generated by pre-commit: https://pre-commit.com\n\
                 exec pre-commit hook-impl --hook-type=pre-commit \"$@\"\n",
            )
            .unwrap();
            let regenerated = std::fs::read_to_string(hooks_dir.join("pre-commit")).unwrap();
            assert!(
                !regenerated.contains(MARKER_START),
                "sanity: regeneration must genuinely wipe any rsry markers, \
                 or this test would prove nothing"
            );

            let after_regeneration =
                std::fs::read_to_string(root.join(".pre-commit-config.yaml")).unwrap();
            assert_eq!(
                after_install, after_regeneration,
                "the YAML fix must be untouched by raw-hook-file regeneration"
            );
        }

        // --- hooks run (rosary-00f2b5) --------------------------------------

        #[test]
        fn hooks_run_unknown_hook_errors() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            let err = run(root, "not-a-real-hook").unwrap_err();
            assert!(format!("{err:#}").contains("unknown hook"), "{err:#}");
        }

        /// Proves `hooks run` genuinely renders and executes the REAL
        /// embedded template (not a stub) — exercised via the pre-commit
        /// hook's own opt-in-by-tracking guard, which short-circuits to a
        /// no-op without needing the `rsry` binary resolvable on PATH (this
        /// test process's `cargo test` sandbox does not guarantee that).
        #[test]
        fn hooks_run_precommit_is_a_noop_when_jsonl_not_tracked() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            seed_commit(root);
            // No .beads/beads.jsonl tracked — the embedded script's own
            // opt-in guard must make this a clean no-op.
            run(root, "pre-commit").unwrap();
        }

        /// Exit-code propagation, proven without depending on the `rsry`
        /// binary: `commit-msg`'s embedded script reads `$1` for the commit
        /// message file. `hooks run` passes no positional argument, so `$1`
        /// is empty — the script deterministically falls through to its own
        /// `exit 1` (no commit-message pattern can match an empty subject).
        #[test]
        fn hooks_run_propagates_a_nonzero_exit_as_an_error() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            init_repo(root);
            let err = run(root, "commit-msg").unwrap_err();
            assert!(format!("{err:#}").contains("exited"), "{err:#}");
        }

        // --- embedded-template invariants ---------------------------------

        #[test]
        fn templates_embedded_and_nonempty() {
            // include_str! is a compile-time op; reading them here proves the
            // build had access to docs/git-hooks/* AND the content survived
            // into the binary. Anything that depends on a real filesystem
            // lookup at runtime (the old find_template_dir path) would fail
            // here when run from a different cwd.
            for (name, content) in HOOKS {
                assert!(!content.trim().is_empty(), "template {name} is empty");
                // The bead-sync hooks drive dolt; the commit-msg hook is the
                // commit-contract gate and has nothing to do with dolt.
                if matches!(*name, "post-merge" | "post-push") {
                    assert!(
                        content.contains("dolt"),
                        "sync template {name} should reference dolt commands"
                    );
                }
            }
        }

        // --- rsry-binary baking (rosary-cb9321) ---------------------------

        fn post_merge_template() -> &'static str {
            HOOKS
                .iter()
                .find(|(n, _)| *n == "post-merge")
                .map(|(_, b)| *b)
                .expect("post-merge template")
        }

        #[test]
        fn post_merge_block_is_portable_and_discloses_installer_version() {
            // The installing binary may live in an ephemeral cargo target or
            // worktree. Generated hooks must not pin that path: they resolve a
            // durable install at runtime and carry the installer version so a
            // mismatch can be diagnosed.
            let ephemeral = "/private/tmp/rosary-build/target/debug/rsry";
            let rendered = render_block(post_merge_template());
            assert!(
                !rendered.contains(ephemeral),
                "ephemeral installer path must not be baked into the hook"
            );
            assert!(
                !rendered.contains("__RSRY_VERSION__"),
                "the install-time version placeholder must be fully substituted"
            );
            assert!(
                rendered.contains("command -v rsry"),
                "runtime PATH lookup must remain"
            );
            assert!(
                rendered.contains("$HOME/.local/bin/rsry"),
                "PATH-restricted hooks need a stable per-user fallback"
            );
            assert!(
                rendered.contains(&format!(
                    "RSRY_HOOK_VERSION=\"{}\"",
                    env!("CARGO_PKG_VERSION")
                )),
                "hook must disclose the installer version to its runtime"
            );
        }

        // --- resolve_hooks_dir --------------------------------------------

        #[test]
        fn resolve_hooks_dir_regular_repo() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            let resolved = resolve_hooks_dir(dir.path()).unwrap();
            // Canonicalize to avoid symlink-prefix mismatches on macOS
            // (/var vs /private/var).
            assert_eq!(
                resolved.canonicalize().unwrap(),
                dir.path()
                    .canonicalize()
                    .unwrap()
                    .join(".git")
                    .join("hooks")
            );
        }

        #[test]
        fn resolve_hooks_dir_worktree() {
            // Main repo with a seed commit, then `git worktree add` a sibling.
            // Inside the worktree, `.git` is a FILE pointing at the gitdir —
            // the old `repo_root.join(".git").join("hooks")` shortcut would
            // produce a non-existent path here.
            let main = tempfile::tempdir().unwrap();
            init_repo(main.path());
            seed_commit(main.path());

            let wt_parent = tempfile::tempdir().unwrap();
            let wt_path = wt_parent.path().join("wt");
            assert!(
                git(
                    main.path(),
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "wt-branch",
                        wt_path.to_str().unwrap(),
                    ],
                )
                .status
                .success(),
                "git worktree add must succeed",
            );

            // Sanity: in a worktree, .git is a file (not a directory).
            assert!(wt_path.join(".git").is_file());

            let resolved = resolve_hooks_dir(&wt_path).unwrap();
            // The resolved path must NOT be wt_path/.git/hooks (which doesn't
            // exist as a directory in a worktree). It should point under the
            // main gitdir or a worktree-specific hooks dir, but in any case
            // the canonicalized prefix should be the main repo's gitdir.
            let main_gitdir = main.path().canonicalize().unwrap().join(".git");
            assert!(
                resolved
                    .canonicalize()
                    .or_else(|_| resolved.parent().unwrap().canonicalize())
                    .unwrap()
                    .starts_with(&main_gitdir),
                "worktree hooks dir {} should be under main gitdir {}",
                resolved.display(),
                main_gitdir.display(),
            );
        }

        #[test]
        fn resolve_hooks_dir_non_git_errors() {
            let dir = tempfile::tempdir().unwrap();
            // Empty tempdir, no git init — must fail loudly.
            let err = resolve_hooks_dir(dir.path()).unwrap_err();
            assert!(
                err.to_string().contains("not a git repo"),
                "expected `not a git repo` in error, got: {err}",
            );
        }

        // --- merge_hook (pure-function behavior) --------------------------

        #[test]
        fn merge_hook_replaces_existing_marker_block() {
            let existing = format!(
                "#!/bin/sh\n# user header\necho hi\n\n{}\nold block contents\n{}\necho after\n",
                MARKER_START, MARKER_END
            );
            let merged = merge_hook(&existing, "new block contents\n");
            assert!(merged.contains("user header"));
            assert!(merged.contains("echo hi"));
            assert!(merged.contains("echo after"));
            assert!(merged.contains("new block contents"));
            assert!(
                !merged.contains("old block contents"),
                "old marked content should be replaced"
            );
        }

        #[test]
        fn merge_hook_appends_when_no_existing_markers() {
            let existing = "#!/bin/sh\necho user logic\n";
            let merged = merge_hook(existing, "rsry block\n");
            assert!(merged.contains("echo user logic"));
            assert!(merged.contains(MARKER_START));
            assert!(merged.contains("rsry block"));
            assert!(merged.contains(MARKER_END));
            // User content must precede the marker block when appending.
            assert!(
                merged.find("echo user logic").unwrap() < merged.find(MARKER_START).unwrap(),
                "user content should come before appended rsry block",
            );
        }

        #[test]
        fn merge_hook_idempotent_when_block_unchanged() {
            // Two calls with the same block produce the same final content.
            let starting = format!(
                "#!/bin/sh\necho user\n\n{}\nsame block\n{}\n",
                MARKER_START, MARKER_END
            );
            let first = merge_hook(&starting, "same block\n");
            let second = merge_hook(&first, "same block\n");
            assert_eq!(first, second, "merge should be idempotent");
            assert_eq!(
                second.matches(MARKER_START).count(),
                1,
                "no duplicated marker block"
            );
        }

        #[test]
        fn merge_hook_recovers_when_end_marker_missing() {
            // Defensive: if the file has START but no END (manual edit damage),
            // we replace from START to EOF rather than leaving cruft.
            let existing = format!("#!/bin/sh\n{}\nstale\n(no end marker)\n", MARKER_START);
            let merged = merge_hook(&existing, "fresh\n");
            assert!(!merged.contains("stale"));
            assert!(!merged.contains("(no end marker)"));
            assert!(merged.contains("fresh"));
            assert!(merged.contains(MARKER_END));
        }

        #[test]
        fn merge_pre_commit_hook_relocates_before_exec_and_is_idempotent() {
            let existing = "#!/bin/sh\nif command -v pre-commit >/dev/null; then\n  exec pre-commit \"$@\"\nfi\n";
            let first = merge_pre_commit_hook(existing, "rsry block\n");
            let second = merge_pre_commit_hook(&first, "rsry block\n");

            assert_eq!(first, second);
            assert_eq!(first.matches(MARKER_START).count(), 1);
            assert!(first.find(MARKER_END).unwrap() < first.find("exec pre-commit").unwrap());
        }

        #[test]
        fn merge_pre_commit_hook_repairs_shebang_without_newline() {
            let merged = merge_pre_commit_hook("#!/bin/sh", "rsry block\n");

            assert!(merged.starts_with("#!/bin/sh\n# >>> rsry-managed"));
            assert_eq!(merged.matches(MARKER_START).count(), 1);
        }

        // --- install (full filesystem + git interaction) ------------------

        #[test]
        fn install_into_fresh_repo() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            install(dir.path()).unwrap();
            for (name, block) in HOOKS {
                let path = dir.path().join(".git").join("hooks").join(name);
                assert!(path.exists(), "{name} should be installed");
                let content = std::fs::read_to_string(&path).unwrap();
                assert!(content.starts_with("#!/bin/sh"));
                assert!(content.contains(MARKER_START));
                assert!(content.contains(MARKER_END));
                let rendered = render_managed_block(name, block);
                assert!(
                    content.contains(rendered.trim()),
                    "{name} should contain rendered template content",
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                    assert_eq!(mode, 0o755, "{name} should be executable (0o755)");
                }
            }
        }

        /// rosary-f9516f: `hooks install` also configures the `beads-jsonl`
        /// merge driver, because gitattributes(5) requires the DEFINITION to
        /// live in git config — the committed `.gitattributes` can only
        /// reference it by name. Idempotent: a second install converges.
        #[test]
        fn install_configures_merge_driver_idempotently() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            install(dir.path()).unwrap();

            let driver =
                git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.driver")).unwrap();
            assert!(driver.contains("bead merge-jsonl"), "{driver}");
            for ph in ["%O", "%A", "%B"] {
                assert!(driver.contains(ph), "driver must pass {ph}: {driver}");
            }
            assert!(git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.name")).is_some());

            install(dir.path()).unwrap();
            let again =
                git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.driver")).unwrap();
            assert_eq!(driver, again, "re-install must converge");
            // `git config --get` errors on a multi-valued key; a successful
            // read after two installs proves we overwrote rather than appended.
        }

        /// The driver resolves rsry when Git invokes it, rather than pinning
        /// whichever ephemeral binary happened to run `hooks install`.
        #[test]
        fn merge_driver_command_is_portable() {
            let ephemeral = "/private/tmp/target/debug/rsry";
            let cmd = merge_driver_command();
            assert!(
                !cmd.contains(ephemeral),
                "driver must not pin installer path: {cmd}"
            );
            assert!(cmd.contains("command -v rsry"), "{cmd}");
            assert!(cmd.contains("$HOME/.local/bin/rsry"), "{cmd}");
            assert!(cmd.contains("RSRY_BIN"), "{cmd}");
            for ph in ["%O", "%A", "%B"] {
                assert!(cmd.contains(ph), "driver must pass {ph}: {cmd}");
            }
        }

        #[test]
        fn install_merges_into_existing_user_hook() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            let hooks_dir = dir.path().join(".git").join("hooks");
            std::fs::create_dir_all(&hooks_dir).unwrap();
            let post_push = hooks_dir.join("post-push");
            // Simulate a pre-existing user hook with custom logic.
            std::fs::write(
                &post_push,
                "#!/bin/sh\n# team-managed hook\necho 'user custom logic'\n",
            )
            .unwrap();

            install(dir.path()).unwrap();

            let content = std::fs::read_to_string(&post_push).unwrap();
            assert!(
                content.contains("user custom logic"),
                "user logic must be preserved across install: {content}",
            );
            assert!(content.contains(MARKER_START));
            assert!(content.contains(MARKER_END));
            assert!(content.contains("team-managed hook"));
        }

        #[test]
        fn install_idempotent_on_reinstall() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            install(dir.path()).unwrap();
            let path = dir.path().join(".git").join("hooks").join("post-push");
            let first = std::fs::read_to_string(&path).unwrap();
            // Reinstall — should produce identical bytes (no duplicated block).
            install(dir.path()).unwrap();
            let second = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, second, "reinstall should be idempotent");
            assert_eq!(
                second.matches(MARKER_START).count(),
                1,
                "no duplicated rsry block on reinstall",
            );
        }

        #[test]
        fn install_preserves_user_content_after_marker_section() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            // First install lays down the rsry block.
            install(dir.path()).unwrap();
            let path = dir.path().join(".git").join("hooks").join("post-push");
            // User appends their own logic AFTER the rsry marker block.
            let original = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{}\necho 'user trailer'\n", original)).unwrap();

            // Reinstall — must preserve the user trailer.
            install(dir.path()).unwrap();
            let after = std::fs::read_to_string(&path).unwrap();
            assert!(
                after.contains("user trailer"),
                "user trailer dropped: {after}"
            );
            assert!(after.contains(MARKER_START));
        }

        #[test]
        fn install_works_in_git_worktree() {
            // The bug Copilot caught: in a worktree, `.git` is a file pointer
            // and `.git/hooks/` doesn't exist. Old code bailed with "is this a
            // git repo?" — new code routes through `git rev-parse` and writes
            // to the right place.
            let main = tempfile::tempdir().unwrap();
            init_repo(main.path());
            seed_commit(main.path());
            let wt_parent = tempfile::tempdir().unwrap();
            let wt_path = wt_parent.path().join("wt");
            assert!(
                git(
                    main.path(),
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "wt-install",
                        wt_path.to_str().unwrap(),
                    ],
                )
                .status
                .success()
            );

            install(&wt_path).expect("install must succeed in a worktree");

            // The hook landed somewhere reachable via resolve_hooks_dir.
            let hooks_dir = resolve_hooks_dir(&wt_path).unwrap();
            for (name, _) in HOOKS {
                assert!(
                    hooks_dir.join(name).exists(),
                    "{name} should be installed at {}",
                    hooks_dir.display(),
                );
            }
        }

        #[test]
        fn install_errors_outside_git_repo() {
            let dir = tempfile::tempdir().unwrap();
            // No git init — install must refuse rather than write into a
            // random directory.
            let err = install(dir.path()).unwrap_err();
            assert!(
                err.to_string().contains("not a git repo"),
                "expected error mentioning `not a git repo`, got: {err}",
            );
        }

        // --- classify_dolt_remote (pure-function decision logic) ----------

        /// Forge a `std::process::Output` with the given exit status and
        /// stdout/stderr. Used to drive `classify_dolt_remote` deterministically
        /// without an actual `dolt` binary.
        fn forge_output(success: bool, stdout: &str, stderr: &str) -> std::process::Output {
            use std::os::unix::process::ExitStatusExt;
            let status = std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 });
            std::process::Output {
                status,
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }

        #[test]
        fn classify_dolt_remote_configured() {
            let out = forge_output(true, "origin\thttps://example.com (fetch)\n", "");
            match classify_dolt_remote(Ok(out)) {
                DoltRemoteStatus::Configured(s) => assert!(s.contains("origin")),
                other => panic!(
                    "expected Configured, got {:?}",
                    std::mem::discriminant(&other)
                ),
            }
        }

        #[test]
        fn classify_dolt_remote_not_configured() {
            let out = forge_output(true, "", "");
            assert!(matches!(
                classify_dolt_remote(Ok(out)),
                DoltRemoteStatus::NotConfigured
            ));
        }

        #[test]
        fn classify_dolt_remote_errored_surfaces_stderr() {
            // The bug Copilot caught: exit-non-zero with empty stdout was
            // misreported as "no remote configured". Now it must surface
            // the failure with stderr preserved.
            let out = forge_output(false, "", "fatal: not a dolt repository\n");
            match classify_dolt_remote(Ok(out)) {
                DoltRemoteStatus::Errored { exit, stderr } => {
                    assert_eq!(exit, 1);
                    assert!(stderr.contains("not a dolt repository"));
                }
                _ => panic!("expected Errored variant for exit-1"),
            }
        }

        #[test]
        fn classify_dolt_remote_spawn_failure() {
            let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
            match classify_dolt_remote(Err(err)) {
                DoltRemoteStatus::NotInvokable(msg) => assert!(msg.contains("no such file")),
                _ => panic!("expected NotInvokable for spawn failure"),
            }
        }

        // --- audit: classify_gitignore_check (examples) --------------------

        #[test]
        fn classify_gitignore_check_shadowed_on_quiet_success() {
            let quiet = forge_output(true, "", "");
            let detail = Some(".gitignore:12:.beads/\t.beads/beads.jsonl".to_string());
            match classify_gitignore_check(Ok(quiet), detail) {
                GitignoreCheck::Shadowed(d) => assert!(d.contains(".gitignore:12")),
                other => panic!("expected Shadowed, got {other:?}"),
            }
        }

        #[test]
        fn classify_gitignore_check_reachable_on_quiet_failure_exit() {
            // `-q` exits 1 when the path is NOT ignored — gitignore(5).
            let quiet = forge_output(false, "", "");
            assert_eq!(
                classify_gitignore_check(Ok(quiet), None),
                GitignoreCheck::Reachable
            );
        }

        /// REGRESSION (found live against notme.bot's default-deny allowlist,
        /// 2026-07-29): `-v` exits 0 whenever ANY rule decided the path,
        /// including a `!negation` that explicitly un-ignores it — so a
        /// `-v`-exit-code-only classifier reported an explicitly TRACKABLE
        /// path as shadowed. `-q`'s exit code is the one with real
        /// ignored/not-ignored semantics; this pins that distinction so it
        /// can't silently regress back to a `-v`-only decision.
        #[test]
        fn classify_gitignore_check_reachable_when_quiet_disagrees_with_verbose_detail() {
            // -q correctly reports "not ignored" (exit 1)...
            let quiet = forge_output(false, "", "");
            // ...even though a verbose detail naming a NEGATION rule is
            // available (what -v would have reported as its deciding line).
            let detail = Some(".gitignore:41:!.beads/beads.jsonl\t.beads/beads.jsonl".to_string());
            assert_eq!(
                classify_gitignore_check(Ok(quiet), detail),
                GitignoreCheck::Reachable,
                "a negation-decided path must classify Reachable regardless of verbose detail"
            );
        }

        #[test]
        fn classify_gitignore_check_unknown_on_spawn_failure() {
            let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
            match classify_gitignore_check(Err(err), None) {
                GitignoreCheck::Unknown(msg) => assert!(msg.contains("no such file")),
                other => panic!("expected Unknown, got {other:?}"),
            }
        }

        // --- audit: pure predicates (property tests) ------------------------
        //
        // These prove the LAWS, not just chosen examples — property tests
        // over `backend_ambiguous` and `store_export_drifted` per the
        // session's mutants-rung discipline (rosary-b2ae79): a law stated
        // once and checked over the whole input space catches boundary bugs
        // an example can't. `proptest_support` isn't used here (no shrink
        // config needed for these small/fast domains); the plain
        // `proptest!` macro's defaults are enough.

        proptest! {
            /// Law 1: an empty store never drifts — there is nothing to lose,
            /// regardless of what the export looks like.
            #[test]
            fn store_export_drift_empty_store_never_flags(jsonl_lines in 0usize..10_000) {
                prop_assert!(!store_export_drifted(0, jsonl_lines));
            }

            /// Law 2: a nonempty store with zero exported lines always
            /// flags — the exact incident this check exists for (366 beads,
            /// 9 repos, zero durable copy, 2026-07-29).
            #[test]
            fn store_export_drift_zero_export_always_flags(db_count in 1i64..10_000) {
                prop_assert!(store_export_drifted(db_count, 0));
            }

            /// Law 3: an export meeting or exceeding the store count never
            /// drifts — a superset export (e.g. after a cross-repo merge) is
            /// never mistaken for data loss.
            #[test]
            fn store_export_drift_full_export_never_flags(
                db_count in 0i64..10_000,
                extra in 0usize..1_000,
            ) {
                let jsonl_lines = db_count as usize + extra;
                prop_assert!(!store_export_drifted(db_count, jsonl_lines));
            }

            /// Law 4 (the load-bearing one): drift is MONOTONIC in
            /// `jsonl_lines` — exporting more can only cure a flagged state,
            /// never cause one. Any threshold-shaped implementation must
            /// hold this regardless of the specific ratio chosen, so this
            /// property survives a future retune of the threshold.
            #[test]
            fn store_export_drift_is_monotonic_in_jsonl_lines(
                db_count in 0i64..10_000,
                jsonl_a in 0usize..10_000,
                jsonl_b in 0usize..10_000,
            ) {
                let (lo, hi) = if jsonl_a <= jsonl_b { (jsonl_a, jsonl_b) } else { (jsonl_b, jsonl_a) };
                // flagged(db, hi) => flagged(db, lo) is the monotonic direction;
                // equivalently !flagged(db, lo) => !flagged(db, hi).
                if store_export_drifted(db_count, hi) {
                    prop_assert!(store_export_drifted(db_count, lo));
                }
            }
        }

        // --- audit: end-to-end fixtures --------------------------------------

        #[test]
        fn audit_flags_gitignore_shadowed_export() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            std::fs::write(dir.path().join(".gitignore"), ".beads/\n").unwrap();
            std::fs::create_dir_all(dir.path().join(".beads")).unwrap();
            std::fs::write(dir.path().join(".beads").join("beads.jsonl"), "").unwrap();

            let err = audit(dir.path()).unwrap_err();
            assert!(format!("{err:#}").contains("gitignore-shadowed"), "{err:#}");
        }

        #[test]
        fn audit_flags_dolt_and_sqlite_coexisting() {
            // The real bug (rosary-9a5926, cloister): a live Dolt server
            // store plus a stray beads.db. This is the shape
            // connect_bead_store's runtime guard has always refused to
            // guess through — the audit check must agree with it now.
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            std::fs::create_dir_all(dir.path().join(".beads").join("dolt")).unwrap();
            std::fs::write(dir.path().join(".beads").join("beads.db"), b"").unwrap();

            let err = audit(dir.path()).unwrap_err();
            assert!(format!("{err:#}").contains("ambiguous backend"), "{err:#}");
        }

        #[test]
        fn audit_passes_when_embeddeddolt_coexists_with_a_live_store() {
            // Corrected behavior (was a false positive before
            // bead_backend::detect_backend): an unused bd-era embeddeddolt/
            // sitting next to a real beads.db is NOT ambiguous —
            // connect_bead_store has always read beads.db and ignored
            // embeddeddolt unconditionally in this shape.
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            std::fs::create_dir_all(dir.path().join(".beads").join("embeddeddolt")).unwrap();
            std::fs::write(dir.path().join(".beads").join("beads.db"), b"").unwrap();

            audit(dir.path()).unwrap();
        }

        #[test]
        fn audit_flags_store_export_drift() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            let db_path = dir.path().join(".beads").join("beads.db");
            std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute("CREATE TABLE issues (id TEXT PRIMARY KEY)", [])
                .unwrap();
            for i in 0..5 {
                conn.execute("INSERT INTO issues (id) VALUES (?1)", [format!("t-{i}")])
                    .unwrap();
            }
            drop(conn);
            // beads.jsonl deliberately absent — the exact incident shape.

            let err = audit(dir.path()).unwrap_err();
            assert!(
                format!("{err:#}").contains("in beads.db, only 0"),
                "{err:#}"
            );
        }

        #[test]
        fn audit_passes_clean_repo_with_no_beads_dir() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            audit(dir.path()).unwrap();
        }

        #[test]
        fn audit_passes_when_store_and_export_agree() {
            let dir = tempfile::tempdir().unwrap();
            init_repo(dir.path());
            let beads_dir = dir.path().join(".beads");
            std::fs::create_dir_all(&beads_dir).unwrap();
            let db_path = beads_dir.join("beads.db");
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute("CREATE TABLE issues (id TEXT PRIMARY KEY)", [])
                .unwrap();
            for i in 0..5 {
                conn.execute("INSERT INTO issues (id) VALUES (?1)", [format!("t-{i}")])
                    .unwrap();
            }
            drop(conn);
            std::fs::write(
                beads_dir.join("beads.jsonl"),
                "{\"id\":\"t-0\"}\n{\"id\":\"t-1\"}\n{\"id\":\"t-2\"}\n{\"id\":\"t-3\"}\n{\"id\":\"t-4\"}\n",
            )
            .unwrap();

            audit(dir.path()).unwrap();
        }

        // --- documentation / marker consistency ---------------------------

        #[test]
        fn marker_constants_have_expected_shape() {
            // Both markers must start with `# >>>` / `# <<<` so they're
            // greppable AND obviously comments in shell. Anyone who edits
            // the markers later should see this test fail before drift
            // leaks into installed hooks.
            assert!(
                MARKER_START.starts_with("# >>> rsry-managed"),
                "MARKER_START shape changed: {MARKER_START}"
            );
            assert!(
                MARKER_END.starts_with("# <<< rsry-managed"),
                "MARKER_END shape changed: {MARKER_END}"
            );
        }

        #[test]
        fn readme_documents_actual_marker_lines() {
            // Drift-detector: if the marker constants change, the docs/git-hooks
            // README MUST reference the new strings. The README explains where
            // users should look in hook files to find the rsry-managed block.
            let readme = include_str!("../docs/git-hooks/README.md");
            assert!(
                readme.contains(MARKER_START),
                "README must contain MARKER_START literal so users can grep for it"
            );
            assert!(
                readme.contains(MARKER_END),
                "README must contain MARKER_END literal so users can grep for it"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// `rsry close-merged` — catch-up sweep for stalled merged-PR beads
// ---------------------------------------------------------------------------

/// Result of a close-merged sweep across one or all registered repos.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CloseMergedSummary {
    /// Open beads inspected
    pub checked: usize,
    /// Beads with no `pr_url` event recorded — skipped
    pub no_pr_url: usize,
    /// Beads whose PR is still open or closed-without-merge — left alone
    pub not_merged: usize,
    /// `gh pr view` failed (auth, network, deleted PR, etc.) — left alone
    pub gh_errors: usize,
    /// Beads we closed (or would close, in dry-run)
    pub merged_closed: usize,
    /// Beads whose PR merged but were held open by the containment gate — a
    /// parent/epic with open children, or a planning bead (rosary-649660).
    /// The PR is still associated; only the close is deferred.
    pub held_open: usize,
    /// IDs of beads closed (in order processed)
    pub bead_ids_closed: Vec<String>,
}

/// Walks every open bead in the given repo (or all registered repos),
/// looks up the PR URL from the `pr_url` event log, runs `gh pr view` to
/// check merge state, and closes the bead when the PR is MERGED.
///
/// Idempotent. `dry_run = true` reports counts but doesn't write.
pub async fn run_close_merged(
    repo_filter: Option<&str>,
    dry_run: bool,
) -> Result<CloseMergedSummary> {
    let cfg = config::load_merged(&config::resolve_config_path())?;
    run_close_merged_with_config(&cfg, repo_filter, dry_run).await
}

/// Inner form taking an explicit Config — exists so unit tests can
/// pass a hand-built empty `Config` and exercise the no-repos /
/// no-match paths without inheriting whatever's in `~/.rsry/config.toml`
/// (which `load_merged` ALWAYS pulls in, defeating any env-based
/// override).
pub async fn run_close_merged_with_config(
    cfg: &config::Config,
    repo_filter: Option<&str>,
    dry_run: bool,
) -> Result<CloseMergedSummary> {
    let mut summary = CloseMergedSummary::default();

    let repos: Vec<&config::RepoConfig> = cfg
        .repo
        .iter()
        .filter(|r| repo_filter.is_none_or(|name| r.name == name))
        .collect();

    if repos.is_empty() {
        if let Some(name) = repo_filter {
            eprintln!("close-merged: no repo named '{name}' is registered");
        } else {
            eprintln!("close-merged: no repos registered");
        }
        return Ok(summary);
    }

    for repo in repos {
        let resolved = scanner::resolve_repo_path(&repo.path);
        // Use the canonical resolver — handles git/jj worktrees where
        // .beads/ lives in the main worktree, not the worktree root.
        // The naive `resolved.join(".beads")` would silently skip those.
        let beads_dir = resolve_beads_dir(&resolved);
        if !beads_dir.exists() {
            continue;
        }
        // Connect to this repo's bead store via the canonical helper.
        let store = match bead_sqlite::connect_bead_store(&beads_dir).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("close-merged: skipping {}: {e}", repo.name);
                continue;
            }
        };

        let beads = match store.list_beads(&repo.name).await {
            Ok(b) => b,
            Err(e) => {
                // Surface store errors instead of silently treating them as
                // "no beads" — masking these produced misleading all-zero
                // summaries before.
                eprintln!("close-merged: list_beads({}) failed: {e}", repo.name);
                summary.gh_errors += 1;
                continue;
            }
        };
        // Sweep ALL non-terminal beads — anything that could legitimately
        // have a PR URL set. The original filter on `Open` only missed
        // the exact stuck-needs-merge cases this command was built for
        // (Dispatched, Verifying, PrOpen).
        let candidate_beads: Vec<&bead::Bead> = beads
            .iter()
            .filter(|b| {
                // Done is the only "fully done" terminal in the enum
                // ("closed" status string maps to Done via From<&str>).
                // Rejected is also terminal — skip those too.
                !matches!(b.state(), bead::BeadState::Done | bead::BeadState::Rejected)
            })
            .collect();

        for b in candidate_beads {
            summary.checked += 1;
            // PR URL can be in two places. Prefer the bead's own pr_url
            // column (set at PR creation in workspace_ops), then fall back
            // to the events log. Either is fine — first one wins.
            let pr_url = b.pr_url.clone().filter(|s| !s.trim().is_empty()).or(
                match store.get_latest_event(&b.id, "pr_url").await {
                    Ok(opt) => opt.filter(|s| !s.trim().is_empty()),
                    Err(_) => None,
                },
            );
            let Some(pr_url) = pr_url else {
                summary.no_pr_url += 1;
                continue;
            };
            let pr_url = pr_url.trim().to_string();

            // Ask gh for the merge state. Single call, JSON-shaped.
            let output = tokio::process::Command::new("gh")
                .args(["pr", "view", &pr_url, "--json", "state,mergeCommit"])
                .output()
                .await;
            let Ok(out) = output else {
                summary.gh_errors += 1;
                continue;
            };
            if !out.status.success() {
                summary.gh_errors += 1;
                continue;
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(_) => {
                    summary.gh_errors += 1;
                    continue;
                }
            };
            let state = parsed["state"].as_str().unwrap_or("");
            if state != "MERGED" {
                summary.not_merged += 1;
                continue;
            }
            let merge_sha = parsed["mergeCommit"]["oid"].as_str().unwrap_or("");

            summary.merged_closed += 1;
            summary.bead_ids_closed.push(b.id.clone());
            if dry_run {
                continue;
            }
            // Record the merge SHA + close the bead. Best-effort logging.
            if !merge_sha.is_empty() {
                store.log_event(&b.id, "merge_sha", merge_sha).await;
            }
            // Format the audit comment so an empty merge_sha doesn't render
            // as `PR merged ()` — that's confusing to read in scrollback.
            let audit_msg = if merge_sha.is_empty() {
                "Auto-closed by rsry close-merged: PR merged".to_string()
            } else {
                format!("Auto-closed by rsry close-merged: PR merged ({merge_sha})")
            };
            store.add_comment(&b.id, &audit_msg, "rosary").await.ok();
            store.close_bead(&b.id).await.ok();
        }
    }

    Ok(summary)
}

/// rsry-native local variant of [`run_close_merged`]. Instead of asking `gh`
/// per bead (an external API + shell transport), it reads the trunk's recent
/// commits with [`vcs::scan_merged_closures`] and closes any still-open bead
/// whose squash-merge commit (`[bead-id] … (#N)`) has landed locally. No `gh` /
/// webhook / tunnel — the git `post-merge` hook (docs/git-hooks/post-merge)
/// drives it after `git pull`. This is the local twin of `serve::github_webhook`:
/// same "merged → close" outcome (satisfying the bead's default "PR merges"
/// close condition), reached by a local pull instead of an inbound POST.
/// Idempotent — re-running only ever closes beads that are still open.
pub async fn run_close_merged_local(
    repo_filter: Option<&str>,
    dry_run: bool,
) -> Result<CloseMergedSummary> {
    let cfg = config::load_merged(&config::resolve_config_path())?;
    run_close_merged_local_with_config(&cfg, repo_filter, dry_run, Publication::Publish).await
}

/// Inner form taking an explicit Config (mirrors [`run_close_merged_with_config`]
/// so tests can pass a hand-built Config).
/// Whether a merge sweep may write its closures back to the git-tracked
/// projection.
///
/// A bare `bool` here would read as `(&cfg, name, false, false)` at the call
/// site — two unrelated switches, indistinguishable. This one is load-bearing
/// enough to name: getting it wrong makes every fresh clone rewrite the shared
/// `beads.jsonl` on first `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// Normal operation, including the post-merge hook: a closure derived from
    /// a merge that just landed SHOULD reach the tracked file.
    Publish,
    /// Bootstrap replay. The closures are re-derived from history the
    /// projection already reflects, so publishing them would echo local
    /// inference back into a shared file.
    Suppress,
}

pub async fn run_close_merged_local_with_config(
    cfg: &config::Config,
    repo_filter: Option<&str>,
    dry_run: bool,
    publication: Publication,
) -> Result<CloseMergedSummary> {
    let mut summary = CloseMergedSummary::default();
    let repos: Vec<&config::RepoConfig> = cfg
        .repo
        .iter()
        .filter(|r| repo_filter.is_none_or(|name| r.name == name))
        .collect();
    if repos.is_empty() {
        eprintln!("close-merged --local: no matching repos registered");
        return Ok(summary);
    }

    for repo in repos {
        let resolved = scanner::resolve_repo_path(&repo.path);
        let beads_dir = resolve_beads_dir(&resolved);
        if !beads_dir.exists() {
            continue;
        }
        // Recent merged-PR closures from local git (trunk, first-parent). Dedup
        // by bead id so a bead referenced by two recent commits is closed once.
        // Bounded window of the last 100 first-parent commits — wide enough that
        // a busy multi-PR session on an active trunk doesn't push a just-merged
        // release commit out of range before the sweep sees it (rosary-cb9321).
        let mut seen = std::collections::HashSet::new();
        let closures: Vec<vcs::MergedClosure> = vcs::scan_merged_closures(&resolved, 100)
            .into_iter()
            .filter(|c| seen.insert(c.bead_id.clone()))
            .collect();
        if closures.is_empty() {
            continue;
        }

        let opened = match publication {
            Publication::Publish => bead_sqlite::connect_bead_store(&beads_dir).await,
            Publication::Suppress => bead_sqlite::connect_bead_store_unpublished(&beads_dir).await,
        };
        let store = match opened {
            Ok(s) => s,
            Err(e) => {
                eprintln!("close-merged --local: skipping {}: {e}", repo.name);
                continue;
            }
        };
        let beads = match store.list_beads(&repo.name).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "close-merged --local: list_beads({}) failed: {e}",
                    repo.name
                );
                continue;
            }
        };

        for closure in &closures {
            summary.checked += 1;
            // Match the webhook's rule: the full id ends with the ref. Only
            // non-terminal beads are eligible (idempotent on re-run).
            let matched = beads.iter().find(|b| {
                !matches!(b.state(), bead::BeadState::Done | bead::BeadState::Rejected)
                    && (b.id == closure.bead_id || b.id.ends_with(&closure.bead_id))
            });
            let Some(b) = matched else {
                continue;
            };

            // Containment gate (rosary-649660): a parent/epic must not
            // auto-close while its children are still open — a merged PR on one
            // child shouldn't sweep the umbrella shut. Children are the open
            // beads linked to b by a parent-child / discovered-from edge.
            let child_ids = store.get_children(&b.id).await.unwrap_or_default();
            let open_children: Vec<String> = child_ids
                .into_iter()
                .filter(|cid| {
                    beads.iter().any(|c| {
                        &c.id == cid
                            && !matches!(
                                c.state(),
                                bead::BeadState::Done | bead::BeadState::Rejected
                            )
                    })
                })
                .collect();
            let is_planning = matches!(b.issue_type.as_str(), "epic" | "design" | "research");
            let hold_open = is_planning || !open_children.is_empty();

            // Record the PR association either way — structured `pr_url` event so
            // a parent + its children's PRs surface as a chain (parity with the
            // gh/webhook path), plus the human-readable github_merge event.
            let pr_ref = vcs::origin_pr_url(&resolved, closure.pr_number)
                .unwrap_or_else(|| format!("#{}", closure.pr_number));
            if !dry_run {
                store.log_event(&b.id, "pr_url", &pr_ref).await;
                store
                    .log_event(
                        &b.id,
                        "github_merge",
                        &format!("PR #{} merged (local git scan)", closure.pr_number),
                    )
                    .await;
            }

            if hold_open {
                summary.held_open += 1;
                let reason = if !open_children.is_empty() {
                    format!(
                        "has {} open child bead(s): {}",
                        open_children.len(),
                        open_children.join(", ")
                    )
                } else {
                    format!("is a {} (planning) bead", b.issue_type)
                };
                eprintln!(
                    "close-merged --local: PR #{} associated with {} but NOT closed — {reason}",
                    closure.pr_number, b.id
                );
                if !dry_run {
                    store
                        .add_comment(
                            &b.id,
                            &format!(
                                "PR #{} merged and linked to this bead, but it was NOT \
                                 auto-closed because it {reason}. Close it once the \
                                 remaining work lands.",
                                closure.pr_number
                            ),
                            "rosary",
                        )
                        .await
                        .ok();
                }
                continue;
            }

            summary.merged_closed += 1;
            summary.bead_ids_closed.push(b.id.clone());
            if dry_run {
                continue;
            }
            store
                .add_comment(
                    &b.id,
                    &format!(
                        "Auto-closed by rsry close-merged --local: PR #{} merged",
                        closure.pr_number
                    ),
                    "rosary",
                )
                .await
                .ok();
            store.close_bead(&b.id).await.ok();
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_merged_summary_default_is_zero() {
        // Sanity: the summary type's Default is all-zero so reporting a
        // no-op sweep is meaningful (no false positives in the count).
        let s = CloseMergedSummary::default();
        assert_eq!(s.checked, 0);
        assert_eq!(s.merged_closed, 0);
        assert!(s.bead_ids_closed.is_empty());
    }

    #[tokio::test]
    async fn close_merged_no_repos_returns_empty_summary() {
        // Regression guard: when the config has no repos (or none match the
        // filter), run_close_merged_with_config returns Ok with a zero
        // summary — not an error. This keeps the command safe to schedule
        // periodically. Uses an explicit empty Config so the test doesn't
        // pull in the user's real ~/.rsry/config.toml via load_merged.
        let cfg = config::Config::default();
        let summary_no_filter = run_close_merged_with_config(&cfg, None, true)
            .await
            .unwrap();
        let summary_filtered = run_close_merged_with_config(&cfg, Some("nonexistent"), true)
            .await
            .unwrap();

        // Both paths return the all-zero default — no work, no errors.
        assert_eq!(summary_no_filter, CloseMergedSummary::default());
        assert_eq!(summary_filtered, CloseMergedSummary::default());
    }

    /// Run an isolated git command in `dir` (no user/global/system config).
    fn tgit(dir: &Path, args: &[&str]) -> std::process::Output {
        let home = tempfile::tempdir().expect("HOME tempdir");
        std::process::Command::new("git")
            .current_dir(dir)
            .env_clear()
            .env("HOME", home.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .args(args)
            .output()
            .expect("spawn git")
    }

    #[tokio::test]
    async fn close_merged_local_holds_open_parent_with_open_child() {
        // rosary-649660: a parent with an open child must be linked to its
        // merged PR but NOT auto-closed. This is exactly the bug that closed
        // rosary-aaffb0 out from under its remaining scope.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tgit(root, &["init", "-q", "-b", "main"]);
        tgit(root, &["config", "user.email", "t@t.invalid"]);
        tgit(root, &["config", "user.name", "t"]);
        tgit(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("f"), "x").unwrap();
        tgit(root, &["add", "f"]);
        // Squash-style merge commit referencing the parent bead.
        tgit(
            root,
            &[
                "commit",
                "-q",
                "-m",
                "[testrepo-parent] feat: umbrella (#1)",
            ],
        );

        // Bead store: open parent + open child, linked by a parent-child edge.
        let beads_dir = root.join(".beads");
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead("testrepo-parent", "Parent", "", 1, "task")
            .await
            .unwrap();
        store
            .create_bead("testrepo-child", "Child", "", 1, "task")
            .await
            .unwrap();
        store
            .add_dependency_typed("testrepo-child", "testrepo-parent", "parent-child")
            .await
            .unwrap();
        drop(store);

        let cfg = config::Config {
            repo: vec![config::RepoConfig {
                name: "testrepo".to_string(),
                path: root.to_path_buf(),
                lang: None,
                self_managed: false,
                approval: config::DispatchApproval::Approved,
            }],
            ..Default::default()
        };

        let summary = run_close_merged_local_with_config(&cfg, None, false, Publication::Publish)
            .await
            .unwrap();

        // Held open, not closed.
        assert_eq!(summary.merged_closed, 0, "parent must not auto-close");
        assert_eq!(summary.held_open, 1, "parent should be held open");

        // But the PR association WAS recorded (structured pr_url event) so the
        // chain surfaces, and the parent is still open.
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        assert_eq!(
            store
                .get_status("testrepo-parent")
                .await
                .unwrap()
                .as_deref(),
            Some("open")
        );
        let pr_evt = store
            .get_latest_event("testrepo-parent", "pr_url")
            .await
            .unwrap();
        assert!(
            pr_evt.is_some(),
            "pr_url event should be recorded on parent"
        );

        // Now close the child and re-run: the parent is eligible and closes.
        store.close_bead("testrepo-child").await.unwrap();
        drop(store);
        let summary2 = run_close_merged_local_with_config(&cfg, None, false, Publication::Publish)
            .await
            .unwrap();
        assert_eq!(
            summary2.merged_closed, 1,
            "parent closes once child is done"
        );
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        // Terminal after close — the exact canonical form ("done"/"closed") is
        // normalized on connect, so assert terminal-ness, not a literal string.
        let status = store.get_status("testrepo-parent").await.unwrap();
        let terminal = status
            .as_deref()
            .map(|s| {
                matches!(
                    bead::BeadState::from(s),
                    bead::BeadState::Done | bead::BeadState::Rejected
                )
            })
            .unwrap_or(false);
        assert!(terminal, "parent should be terminal, got {status:?}");
    }

    #[tokio::test]
    async fn close_merged_local_closes_on_commit_evidence_without_pr_url() {
        // rosary-cb9321: a bead created via MCP and merged carries NO pr_url
        // event — the squash commit's `[bead-id] … (#N)` IS the merge evidence.
        // The multi-segment repo prefix (`ley-line-open`) is the exact shape the
        // first-dash parser rejected, leaving the bead open with checked=0.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tgit(root, &["init", "-q", "-b", "main"]);
        tgit(root, &["config", "user.email", "t@t.invalid"]);
        tgit(root, &["config", "user.name", "t"]);
        tgit(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("f"), "x").unwrap();
        tgit(root, &["add", "f"]);
        tgit(
            root,
            &[
                "commit",
                "-q",
                "-m",
                "[ley-line-open-e5addb] chore(release): 0.7.1 (#229)",
            ],
        );

        // Open bead, NO pr_url event ever recorded.
        let beads_dir = root.join(".beads");
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead("ley-line-open-e5addb", "Release", "", 1, "task")
            .await
            .unwrap();
        drop(store);

        let cfg = config::Config {
            repo: vec![config::RepoConfig {
                name: "ley-line-open".to_string(),
                path: root.to_path_buf(),
                lang: None,
                self_managed: false,
                approval: config::DispatchApproval::Approved,
            }],
            ..Default::default()
        };

        let summary = run_close_merged_local_with_config(&cfg, None, false, Publication::Publish)
            .await
            .unwrap();

        assert_eq!(summary.checked, 1, "the release commit must be scanned");
        assert_eq!(
            summary.merged_closed, 1,
            "commit-message evidence alone must close the bead"
        );
        assert_eq!(summary.bead_ids_closed, vec!["ley-line-open-e5addb"]);

        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        let status = store.get_status("ley-line-open-e5addb").await.unwrap();
        let terminal = status
            .as_deref()
            .map(|s| {
                matches!(
                    bead::BeadState::from(s),
                    bead::BeadState::Done | bead::BeadState::Rejected
                )
            })
            .unwrap_or(false);
        assert!(terminal, "bead should be terminal, got {status:?}");
    }

    #[test]
    fn bead_action_variants_construct() {
        // Verify each BeadAction variant can be constructed with expected fields
        let create = BeadAction::Create {
            title: "Fix the widget".to_string(),
            description: "It is broken".to_string(),
            priority: 1,
            issue_type: "bug".to_string(),
            files: vec!["src/widget.rs".to_string()],
            test_files: vec![],
            acceptance: String::new(),
            force: false,
            role: "canonical".to_string(),
        };
        assert!(matches!(create, BeadAction::Create { priority: 1, .. }));

        let close = BeadAction::Close {
            id: "rsry-abc".to_string(),
            force: false,
        };
        assert!(matches!(close, BeadAction::Close { .. }));

        let list = BeadAction::List {
            status: vec!["open".to_string()],
            priority: vec![1],
            issue_type: vec!["bug".to_string()],
            ready: false,
            dispatchable: false,
            blocked: false,
            limit: 25,
            json: false,
        };
        assert!(matches!(list, BeadAction::List { .. }));

        let comment = BeadAction::Comment {
            action: BeadCommentAction::Add {
                id: "rsry-abc".to_string(),
                body: "looking into this".to_string(),
            },
        };
        assert!(matches!(
            comment,
            BeadAction::Comment {
                action: BeadCommentAction::Add { .. }
            }
        ));
    }

    #[test]
    fn generate_bead_id_uses_repo_prefix() {
        let id = generate_bead_id("mache");
        assert!(
            id.starts_with("mache-"),
            "id should start with 'mache-': {id}"
        );
        // Suffix must be exactly 6 hex characters
        let suffix = &id["mache-".len()..];
        assert_eq!(suffix.len(), 6, "suffix should be 6 chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex: {suffix}"
        );
    }

    #[test]
    fn generate_bead_id_different_repos() {
        let id1 = generate_bead_id("rosary");
        let id2 = generate_bead_id("mache");
        assert!(id1.starts_with("rosary-"));
        assert!(id2.starts_with("mache-"));
    }

    #[test]
    fn generate_bead_id_no_collision_in_tight_loop() {
        // rosary-b62d5f: same-millisecond creates used to collide (millis & mask).
        // A per-process monotonic counter makes consecutive ids distinct.
        use std::collections::HashSet;
        let n = 10_000;
        let ids: HashSet<String> = (0..n).map(|_| generate_bead_id("rosary")).collect();
        assert_eq!(ids.len(), n, "all {n} ids must be distinct");
        // format contract preserved: prefix-6hex
        let sample = generate_bead_id("rosary");
        let suffix = &sample["rosary-".len()..];
        assert_eq!(suffix.len(), 6);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // --- rosary-3f8515: prefix sanitization (no more malformed `.-` IDs) ---

    #[test]
    fn sanitize_prefix_falls_back_when_unusable() {
        // The exact bug: empty / "." / whitespace prefixes produced `.-xxxxxx`
        // and `-xxxxxx` IDs. They must normalize to a safe fallback instead.
        assert_eq!(sanitize_prefix(""), "bead");
        assert_eq!(sanitize_prefix("."), "bead");
        assert_eq!(sanitize_prefix("   "), "bead");
        assert_eq!(sanitize_prefix("///"), "bead");
    }

    #[test]
    fn sanitize_prefix_normalizes_case_and_junk() {
        assert_eq!(sanitize_prefix("Rosary"), "rosary"); // lowercase
        assert_eq!(sanitize_prefix("ley-line"), "ley-line"); // hyphens kept (valid prefix)
        assert_eq!(sanitize_prefix("My Repo!"), "my-repo"); // junk → single hyphen, trimmed
        assert_eq!(sanitize_prefix("foo.bar"), "foo-bar"); // dots → hyphen
        assert_eq!(sanitize_prefix("-x_"), "x"); // leading/trailing separators trimmed
    }

    #[test]
    fn sanitize_prefix_takes_path_basename() {
        // Callers sometimes pass a path-like repo identifier; use the last
        // non-empty segment, not the whole path (which would inject hyphens).
        assert_eq!(sanitize_prefix("/Users/me/the-firm"), "the-firm");
        assert_eq!(sanitize_prefix("/Users/me/the-firm/"), "the-firm");
    }

    #[test]
    fn resolve_bead_prefix_precedence_and_fallthrough() {
        // explicit config prefix wins
        assert_eq!(
            resolve_bead_prefix(Some("explicit"), "name", Some("remote"), "base"),
            "explicit"
        );
        // explicit junk → fall to repo name
        assert_eq!(
            resolve_bead_prefix(Some("."), "name", Some("remote"), "base"),
            "name"
        );
        // no explicit → repo name
        assert_eq!(
            resolve_bead_prefix(None, "name", Some("remote"), "base"),
            "name"
        );
        // empty/junk name → git remote (the "default = remote name" path)
        assert_eq!(
            resolve_bead_prefix(None, "", Some("remote"), "base"),
            "remote"
        );
        // name + remote both junk → dir basename
        assert_eq!(
            resolve_bead_prefix(Some(""), ".", Some("  "), "base"),
            "base"
        );
        // nothing usable anywhere → safe fallback
        assert_eq!(resolve_bead_prefix(None, "", None, ""), "bead");
        // chosen source is sanitized
        assert_eq!(
            resolve_bead_prefix(Some("My Repo!"), "x", None, "y"),
            "my-repo"
        );
    }

    #[test]
    fn generate_bead_id_never_malformed_for_bad_prefix() {
        // End-to-end: even a garbage prefix yields a well-formed ID.
        for bad in ["", ".", "  ", "/Users/x/"] {
            let id = generate_bead_id(bad);
            assert!(
                !id.starts_with('-') && !id.starts_with('.'),
                "malformed id from prefix {bad:?}: {id}"
            );
            let (pfx, suffix) = id.rsplit_once('-').expect("id has a separator");
            assert!(!pfx.is_empty(), "empty prefix in {id}");
            assert_eq!(suffix.len(), 6, "suffix should be 6 hex: {id}");
        }
    }

    #[test]
    fn pr_title_uses_hyphenated_bead_prefix_from_head() {
        let title = pr_title_with_head_bead(
            "[canonical-hours-4f71c9] feat(observer): publish portable observer core",
            "feat(observer): publish portable observer core",
        );
        assert_eq!(
            title.as_deref(),
            Some("[canonical-hours-4f71c9] feat(observer): publish portable observer core")
        );
    }
}
