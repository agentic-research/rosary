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
mod bead_close_condition;
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
mod cloister_provider;
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
mod hooks;
mod import;
mod init;
mod jsonl;
mod jsonl_sync;
mod linear;
#[allow(dead_code)]
mod linear_tracker;
mod linear_transport;
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
        /// Deprecated: isolation is mandatory (a shared-checkout dispatch
        /// caused a real data-loss incident; see workspace/lifecycle.rs).
        /// Parsed for compatibility, ignored with a warning when false.
        #[arg(
            long,
            default_value_t = true,
            action = clap::ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        isolate: bool,
        /// Show what the targeted pipeline would dispatch without spawning
        #[arg(long)]
        dry_run: bool,
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
    /// Cluster the open backlog for near-duplicates, sequences, and shared
    /// scope (rosary-cb1af4 slice 1: `epic::cluster_beads` computes
    /// `ClusterAction::Merge` today with no executor anywhere in the
    /// codebase — this is that executor). Reports by default; `--execute`
    /// performs any suggested merges: the `close` beads get a comment
    /// linking to `keep`, then close (force, since a merge is a
    /// superseded-not-completed closure, not a normal done).
    Epic {
        #[command(subcommand)]
        action: EpicAction,
        /// Repo path (defaults to the current directory)
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
enum EpicAction {
    /// Run `epic::cluster_beads` over the repo's open backlog and report
    /// every cluster found. Pass `--execute` to perform suggested merges
    /// instead of only reporting them.
    Scan {
        /// Perform suggested Merge actions instead of only reporting them.
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
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
    /// Publish the named beads' current records into the tracked
    /// `.beads/beads.jsonl` — the commit-msg hook's primitive (thread
    /// trusted-kernel/projection-timing, rosary-e5bfd3).
    Publish {
        /// Read the bead ids from this commit-message file (`[<repo>-<hex>]`
        /// brackets in the subject line)
        #[arg(long, value_name = "FILE")]
        from_commit_msg: Option<String>,
        /// Bead ids to publish
        ids: Vec<String>,
    },
    /// pre-push gate: for the beads the pushed commits name, the pushed
    /// ref's `.beads/beads.jsonl` blob must agree with the store. Reads
    /// git's pre-push stdin (rosary-e5c037).
    VerifyPushed,
    /// On the trunk after a merge: re-render every published bead from the
    /// store and commit the projection (rosary-e5c0a0).
    TrunkRefresh {
        /// Push the refresh commit to origin
        #[arg(long)]
        push: bool,
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

    if bead_backup::classify(beads_dir)? != bead_backup::Backend::Dolt {
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
    // Only `--commit`'s target sits inside a Dolt-backed `.beads/` by
    // construction (that's the whole premise of this command) — `connect`'s
    // rosary-554a74 ambiguity guard would refuse to create it there, so
    // `--commit` needs the sanctioned `connect_staging` bypass (rosary-cb7a8d).
    // Dry-run's target is always under `temp_dir()`, never Dolt-backed, so
    // `connect`'s guard would never fire for it anyway — use it unchanged
    // rather than widening the bypass to a path that doesn't need it.
    let target = if commit {
        bead_sqlite::SqliteBeadStore::connect_staging(&built)?
    } else {
        bead_sqlite::SqliteBeadStore::connect(&built)?
    };
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
    // Replay trunk-derived closures into the SAME raw store the restore just
    // filled. Under ADR-0024 amendment A a store write never touches the
    // projection — the hooks publish at commit time — so there is nothing to
    // suppress here any more (tests/init_jsonl_reconciliation.rs).
    let mut closed = CloseMergedSummary::default();
    close_merged_in_repo(
        &store,
        repo_entry,
        repo_root,
        &merged_closures_for(repo_root),
        false,
        &mut closed,
    )
    .await;

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
            dry_run,
        } => {
            if !isolate {
                eprintln!(
                    "[dispatch] --isolate=false is deprecated and ignored — isolation is \
                     mandatory (a shared-checkout dispatch caused a data-loss incident; \
                     see workspace/lifecycle.rs). Dispatching isolated."
                );
            }
            reconcile::run_targeted_dispatch(
                &bead_id,
                std::path::Path::new(&repo),
                &provider,
                dry_run,
            )
            .await?;
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
                // Projection-timing surfaces (thread trusted-kernel/projection-timing)
                // read git and the store themselves; like Diff they run before
                // the store gate so a hook can call them from any checkout.
                BeadAction::Publish {
                    from_commit_msg,
                    ids,
                } => {
                    publish::commit::publish_from_commit(
                        &repo_root,
                        from_commit_msg.as_deref().map(Path::new),
                        ids,
                    )
                    .await?;
                    return Ok(());
                }
                BeadAction::VerifyPushed => {
                    publish::push::verify_pushed(&repo_root, std::io::stdin().lock()).await?;
                    return Ok(());
                }
                BeadAction::TrunkRefresh { push } => {
                    publish::trunk::refresh_trunk(&repo_root, *push)?;
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
                    bead_ops::create_bead(
                        client.as_ref(),
                        &repo_root,
                        &id,
                        &args,
                        created_by.as_deref(),
                    )
                    .await?;
                    cli::bead_created(&id, &args.title);
                }
                BeadAction::Close { id, force } => {
                    bead_ops::close_bead(client.as_ref(), &id, &repo_name, force).await?;
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
                    // rosary-ee49bf: `BeadState::Done` has no valid transitions
                    // BY DESIGN (rosary-e0e19f: that's a claim the workflow has
                    // no next step, not that the record can't be wrong) — so the
                    // guarded update_status path refuses `done -> open` even
                    // when the close was itself wrong. `reopen` on a terminal
                    // bead IS that "the record was wrong" claim; route through
                    // the same correction mechanism `bead correct` uses rather
                    // than the transition gate, which would refuse it.
                    let current = client
                        .get_status(&id)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("no such bead: {id}"))?;
                    if bead::BeadState::from(current.as_str()).is_terminal() {
                        bead_correct::correct_status(
                            client.as_ref(),
                            &id,
                            "open",
                            "Reopened via `rsry bead reopen` — recovering a bead \
                             left in a terminal state.",
                        )
                        .await?;
                    } else {
                        client.update_status(&id, "open").await?;
                    }
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
                BeadAction::Publish { .. }
                | BeadAction::VerifyPushed
                | BeadAction::TrunkRefresh { .. } => {
                    unreachable!("projection-timing surfaces are dispatched pre-store")
                }
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
        Command::Epic { action, repo } => {
            let repo_root = scanner::resolve_repo_path(std::path::Path::new(&repo));
            let beads_dir = resolve_beads_dir(&repo_root);
            let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
            let repo_name = repo_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| repo.clone());

            match action {
                EpicAction::Scan { execute, json } => {
                    run_epic_scan(client.as_ref(), &repo_name, execute, json).await?;
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
                    "close-merged --local: {} {} (checked={}, held_open={}, \
                     refused_unmet_condition={}, close_failed={})",
                    summary.merged_closed,
                    verb,
                    summary.checked,
                    summary.held_open,
                    summary.refused_unmet_condition,
                    summary.close_failed,
                );
                for id in &summary.bead_ids_closed {
                    println!("  {id}");
                }
            } else {
                let summary = run_close_merged(repo.as_deref(), dry_run).await?;
                println!(
                    "close-merged: {} {} (checked={}, no_pr_url={}, not_merged={}, \
                     gh_errors={}, refused_unmet_condition={}, close_failed={})",
                    summary.merged_closed,
                    verb,
                    summary.checked,
                    summary.no_pr_url,
                    summary.not_merged,
                    summary.gh_errors,
                    summary.refused_unmet_condition,
                    summary.close_failed,
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
    /// Beads whose PR merged but held open because their `acceptance_criteria`
    /// explicitly requires more than a merge (rosary-c75925/e0e19f) — a
    /// verification `close-merged` can't itself perform. The refusal is
    /// recorded as a comment; close manually once actually verified.
    pub refused_unmet_condition: usize,
    /// Beads that passed every gate but whose `close_bead` write itself
    /// failed — never folded into `merged_closed`/`bead_ids_closed`, so a
    /// failed write is never counted as a success (rosary-c75925).
    pub close_failed: usize,
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

            // rosary-c75925/e0e19f: a commit REFERENCING this bead merging is
            // not proof that whatever this bead's acceptance_criteria actually
            // demands (if it demands something beyond "a PR merges") ran and
            // passed — this scan has no way to check that. Only auto-close
            // when the bead's own declared condition IS "a PR merges".
            if !crate::bead::merge_alone_satisfies_close(&b.acceptance_criteria) {
                summary.refused_unmet_condition += 1;
                eprintln!(
                    "close-merged: PR {pr_url} merged for {} but NOT closed — \
                     acceptance_criteria requires more than a merge: {}",
                    b.id, b.acceptance_criteria
                );
                if !dry_run
                    && let Err(e) = store
                        .add_comment(
                            &b.id,
                            &format!(
                                "PR merged ({pr_url}), but NOT auto-closed — this bead's \
                                 acceptance_criteria requires verification close-merged can't \
                                 provide: \"{}\". Close manually once actually verified, or \
                                 update the condition if a merge alone is sufficient.",
                                b.acceptance_criteria
                            ),
                            "rosary",
                        )
                        .await
                {
                    eprintln!(
                        "close-merged: failed to record refusal comment for {}: {e:#}",
                        b.id
                    );
                }
                continue;
            }

            if dry_run {
                summary.merged_closed += 1;
                summary.bead_ids_closed.push(b.id.clone());
                continue;
            }
            // Record the merge SHA. Best-effort logging.
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
            if let Err(e) = store.add_comment(&b.id, &audit_msg, "rosary").await {
                eprintln!(
                    "close-merged: failed to record audit comment for {}: {e:#}",
                    b.id
                );
            }
            // rosary-c75925: only count a close as done once the write actually
            // succeeds — a failed write must never be reported as a success.
            // merge_alone_satisfies_close above IS this call site's gate;
            // bead_ops::close_bead's stricter "looks like a runnable command"
            // check is the wrong question here.
            let close_result = store.close_bead(&b.id).await; // nosemgrep: bead-close-bypasses-gate
            match close_result {
                Ok(()) => {
                    summary.merged_closed += 1;
                    summary.bead_ids_closed.push(b.id.clone());
                }
                Err(e) => {
                    summary.close_failed += 1;
                    eprintln!("close-merged: failed to close {}: {e:#}", b.id);
                }
            }
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
    run_close_merged_local_with_config(&cfg, repo_filter, dry_run).await
}

/// Inner form taking an explicit Config (mirrors [`run_close_merged_with_config`]
/// so tests can pass a hand-built Config).
/// Cluster the repo's open backlog and, on `execute`, perform any suggested
/// `ClusterAction::Merge`: close the `close` beads with a comment linking to
/// `keep` (rosary-cb1af4 slice 1 — the executor `ClusterAction::Merge` never
/// had before this).
pub async fn run_epic_scan(
    client: &dyn store::BeadStore,
    repo_name: &str,
    execute: bool,
    json: bool,
) -> Result<()> {
    let beads = client.list_beads(repo_name).await?;
    let clusters = epic::cluster_beads(&beads);

    if json {
        let rendered: Vec<_> = clusters
            .iter()
            .map(|c| {
                serde_json::json!({
                    "relationship": format!("{:?}", c.relationship),
                    "action": format!("{:?}", c.action),
                    "cohesion": c.cohesion,
                    "bead_ids": c.bead_ids,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rendered)?);
    } else if clusters.is_empty() {
        println!("no clusters found in {} open beads", beads.len());
    } else {
        for c in &clusters {
            println!(
                "{:?} cohesion={:.2} action={:?}",
                c.relationship, c.cohesion, c.action
            );
            for id in &c.bead_ids {
                if let Some(b) = beads.iter().find(|b| &b.id == id) {
                    println!("  {} [P{}] {}", b.id, b.priority, b.title);
                }
            }
        }
    }

    let merges: Vec<_> = clusters
        .iter()
        .filter_map(|c| match &c.action {
            epic::ClusterAction::Merge { keep, close } => {
                Some((keep.clone(), close.clone(), c.cohesion))
            }
            _ => None,
        })
        .collect();

    if merges.is_empty() {
        return Ok(());
    }
    if !execute {
        eprintln!(
            "\n{} suggested merge(s) — pass --execute to perform them",
            merges.len()
        );
        return Ok(());
    }
    execute_epic_merges(client, repo_name, merges).await
}

/// The mutation half of `run_epic_scan`, split out on its own (fan_out_skew):
/// scanning/reporting talks to `epic`+`serde_json`, this talks to
/// `bead_ops` — different concerns, different callees.
async fn execute_epic_merges(
    client: &dyn store::BeadStore,
    repo_name: &str,
    merges: Vec<(String, Vec<String>, f64)>,
) -> Result<()> {
    for (keep, close, cohesion) in merges {
        for close_id in &close {
            client
                .add_comment(
                    close_id,
                    &format!(
                        "Merged into {keep} — near-duplicate (cohesion {cohesion:.2}) detected \
                         by `rsry epic scan --execute`."
                    ),
                    "rsry-cli",
                )
                .await?;
            bead_ops::close_bead(client, close_id, repo_name, true).await?;
        }
        println!("merged {close:?} into {keep}");
    }
    Ok(())
}

pub async fn run_close_merged_local_with_config(
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
        eprintln!("close-merged --local: no matching repos registered");
        return Ok(summary);
    }

    for repo in repos {
        let resolved = scanner::resolve_repo_path(&repo.path);
        let beads_dir = resolve_beads_dir(&resolved);
        if !beads_dir.exists() {
            continue;
        }
        let closures = merged_closures_for(&resolved);
        if closures.is_empty() {
            continue;
        }

        let store = match bead_sqlite::connect_bead_store(&beads_dir).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("close-merged --local: skipping {}: {e}", repo.name);
                continue;
            }
        };
        close_merged_in_repo(
            store.as_ref(),
            repo,
            &resolved,
            &closures,
            dry_run,
            &mut summary,
        )
        .await;
    }

    Ok(summary)
}

/// Recent merged-PR closures from local git (trunk, first-parent). Dedup by
/// bead id so a bead referenced by two recent commits is closed once. Bounded
/// window of the last 100 first-parent commits — wide enough that a busy
/// multi-PR session on an active trunk doesn't push a just-merged release
/// commit out of range before the sweep sees it (rosary-cb9321).
fn merged_closures_for(resolved: &Path) -> Vec<vcs::MergedClosure> {
    let mut seen = std::collections::HashSet::new();
    vcs::scan_merged_closures(resolved, 100)
        .into_iter()
        .filter(|c| seen.insert(c.bead_id.clone()))
        .collect()
}

/// One repo's merge sweep against an already-open store.
///
/// Takes the store rather than opening one so `rsry init`'s bootstrap can
/// replay into the raw store it just restored the projection into, and the
/// post-merge sweep can pass whatever `connect_bead_store` hands it. Neither
/// path writes the projection: under ADR-0024 amendment A only the hooks do.
async fn close_merged_in_repo(
    store: &dyn store::BeadStore,
    repo: &config::RepoConfig,
    resolved: &Path,
    closures: &[vcs::MergedClosure],
    dry_run: bool,
    summary: &mut CloseMergedSummary,
) {
    let beads = match store.list_beads(&repo.name).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "close-merged --local: list_beads({}) failed: {e}",
                repo.name
            );
            return;
        }
    };

    for closure in closures {
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
                        && !matches!(c.state(), bead::BeadState::Done | bead::BeadState::Rejected)
                })
            })
            .collect();
        let is_planning = matches!(b.issue_type.as_str(), "epic" | "design" | "research");
        let hold_open = is_planning || !open_children.is_empty();

        // Record the PR association either way — structured `pr_url` event so
        // a parent + its children's PRs surface as a chain (parity with the
        // gh/webhook path), plus the human-readable github_merge event.
        let pr_ref = vcs::origin_pr_url(resolved, closure.pr_number)
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

        // rosary-c75925/e0e19f: a commit REFERENCING this bead merging is
        // not proof that whatever this bead's acceptance_criteria actually
        // demands (if it demands something beyond "a PR merges") ran and
        // passed — a local git-log scan has no way to check that. Only
        // auto-close when the bead's own declared condition IS "a PR
        // merges".
        if !crate::bead::merge_alone_satisfies_close(&b.acceptance_criteria) {
            summary.refused_unmet_condition += 1;
            eprintln!(
                "close-merged --local: PR #{} merged for {} but NOT closed — \
                     acceptance_criteria requires more than a merge: {}",
                closure.pr_number, b.id, b.acceptance_criteria
            );
            if !dry_run
                && let Err(e) = store
                    .add_comment(
                        &b.id,
                        &format!(
                            "PR #{} merged, but NOT auto-closed — this bead's \
                                 acceptance_criteria requires verification close-merged \
                                 can't provide: \"{}\". Close manually once actually \
                                 verified, or update the condition if a merge alone is \
                                 sufficient.",
                            closure.pr_number, b.acceptance_criteria
                        ),
                        "rosary",
                    )
                    .await
            {
                eprintln!(
                    "close-merged --local: failed to record refusal comment for {}: {e:#}",
                    b.id
                );
            }
            continue;
        }

        if dry_run {
            summary.merged_closed += 1;
            summary.bead_ids_closed.push(b.id.clone());
            continue;
        }
        let audit_msg = format!(
            "Auto-closed by rsry close-merged --local: PR #{} merged",
            closure.pr_number
        );
        if let Err(e) = store.add_comment(&b.id, &audit_msg, "rosary").await {
            eprintln!(
                "close-merged --local: failed to record audit comment for {}: {e:#}",
                b.id
            );
        }
        // rosary-c75925: only count a close as done once the write actually
        // succeeds — a failed write must never be reported as a success.
        // merge_alone_satisfies_close above IS this call site's gate;
        // bead_ops::close_bead's stricter "looks like a runnable command"
        // check is the wrong question here.
        let close_result = store.close_bead(&b.id).await; // nosemgrep: bead-close-bypasses-gate
        match close_result {
            Ok(()) => {
                summary.merged_closed += 1;
                summary.bead_ids_closed.push(b.id.clone());
            }
            Err(e) => {
                summary.close_failed += 1;
                eprintln!("close-merged --local: failed to close {}: {e:#}", b.id);
            }
        }
    }
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

    /// rosary-cb1af4 slice 1: `epic::cluster_beads` computes `ClusterAction::Merge`
    /// but nothing executed it anywhere in the codebase before `run_epic_scan`.
    /// Pins that the executor is dry-run by default and actually merges (close +
    /// comment + jsonl refresh) on `--execute`, using the same near-duplicate
    /// fixture `epic.rs`'s own `merge_action_keeps_highest_priority` test uses.
    #[tokio::test]
    async fn epic_scan_dry_run_reports_without_closing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let beads_dir = root.join(".beads");
        std::fs::create_dir_all(&beads_dir).unwrap();
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "t-a".to_string(),
                title: "fix widget bug".to_string(),
                priority: 1,
                issue_type: "bug".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "t-b".to_string(),
                title: "fix widget bug in production".to_string(),
                priority: 2,
                issue_type: "bug".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        run_epic_scan(store.as_ref(), "t", false, false)
            .await
            .unwrap();

        assert_eq!(store.get_status("t-a").await.unwrap().unwrap(), "open");
        assert_eq!(store.get_status("t-b").await.unwrap().unwrap(), "open");
    }

    #[tokio::test]
    async fn epic_scan_execute_merges_near_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tgit(root, &["init", "-q", "-b", "main"]);
        tgit(root, &["config", "user.email", "t@t.invalid"]);
        tgit(root, &["config", "user.name", "t"]);
        tgit(root, &["config", "commit.gpgsign", "false"]);
        let beads_dir = root.join(".beads");
        std::fs::create_dir_all(&beads_dir).unwrap();

        // NearDuplicate requires RAW (unfiltered) title Jaccard > 0.8 across every
        // pair in the cluster (epic::classify_relationship) — not the
        // stopword-filtered signal combined_similarity otherwise uses. 9/11 shared
        // tokens, one word differs, so raw Jaccard = 0.818. (epic.rs's own
        // merge_action_keeps_highest_priority test picked a fixture that scores
        // 0.6 here and silently never exercises Merge at all — its `if let
        // Merge {...}` has no `else`, so a wrong relationship just passes.)
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "t-a".to_string(),
                title: "fix widget bug during checkout flow process on mobile devices".to_string(),
                priority: 1,
                issue_type: "bug".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "t-b".to_string(),
                title: "fix widget bug during checkout flow process on mobile browsers".to_string(),
                priority: 2,
                issue_type: "bug".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        run_epic_scan(store.as_ref(), "t", true, false)
            .await
            .unwrap();

        // Lower-priority bead (t-b, P2) merged away; higher-priority (t-a, P1) kept.
        assert_eq!(store.get_status("t-a").await.unwrap().unwrap(), "open");
        assert_eq!(store.get_status("t-b").await.unwrap().unwrap(), "done");
        let comments = store.list_comments("t-b", false).await.unwrap();
        assert!(
            comments.iter().any(|c| c.text.contains("Merged into t-a")),
            "closed bead must record why it was merged, not just disappear"
        );
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

        let summary = run_close_merged_local_with_config(&cfg, None, false)
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
        store.close_bead("testrepo-child").await.unwrap(); // nosemgrep: bead-close-bypasses-gate — test setup, not a bypass
        drop(store);
        let summary2 = run_close_merged_local_with_config(&cfg, None, false)
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

        let summary = run_close_merged_local_with_config(&cfg, None, false)
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

    /// rosary-c75925/e0e19f: a bead whose `acceptance_criteria` explicitly
    /// demands something beyond "a PR merges" must NOT auto-close on mere
    /// commit-message evidence — close-merged has no way to verify that
    /// declared condition. This is the exact bug rosary-e0e19f described (and
    /// was itself bitten by, twice, before landing this).
    #[tokio::test]
    async fn close_merged_local_refuses_a_bead_with_an_unmet_explicit_condition() {
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
            &["commit", "-q", "-m", "[testrepo-abc123] fix: widget (#1)"],
        );

        let beads_dir = root.join(".beads");
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "testrepo-abc123".to_string(),
                title: "Fix widget".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                acceptance_criteria: "cargo test -p widget must pass".to_string(),
                ..Default::default()
            })
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

        let summary = run_close_merged_local_with_config(&cfg, None, false)
            .await
            .unwrap();

        assert_eq!(
            summary.refused_unmet_condition, 1,
            "an explicit, unmet condition must be refused, not silently closed"
        );
        assert_eq!(summary.merged_closed, 0);
        assert!(summary.bead_ids_closed.is_empty());

        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        let status = store.get_status("testrepo-abc123").await.unwrap();
        assert_eq!(
            status.as_deref(),
            Some("open"),
            "bead must stay open — its declared condition was never verified"
        );

        // rosary-c75925 (Copilot review, PR #473): a counter incrementing is
        // not evidence the refusal comment was actually written — pin the
        // write itself, not just the summary that reports it.
        let comments = store.list_comments("testrepo-abc123", false).await.unwrap();
        assert!(
            comments.iter().any(|c| c.text.contains("NOT auto-closed")
                && c.text.contains("cargo test -p widget must pass")),
            "refusal must be recorded as a comment naming the unmet condition, got: {comments:?}"
        );
    }

    /// The other half: a bead whose condition IS explicitly the PR-merge
    /// default (not just absent) still closes normally — accepting it is
    /// honoring what the bead declared, not a bypass.
    #[tokio::test]
    async fn close_merged_local_closes_a_bead_whose_condition_is_the_merge_default() {
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
            &["commit", "-q", "-m", "[testrepo-def456] fix: gadget (#2)"],
        );

        let beads_dir = root.join(".beads");
        let store = bead_sqlite::connect_bead_store(&beads_dir).await.unwrap();
        store
            .create_bead_full(store::NewBead {
                id: "testrepo-def456".to_string(),
                title: "Fix gadget".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                acceptance_criteria: bead::DEFAULT_PR_MERGE_CLOSE_CONDITION.to_string(),
                ..Default::default()
            })
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

        let summary = run_close_merged_local_with_config(&cfg, None, false)
            .await
            .unwrap();

        assert_eq!(summary.refused_unmet_condition, 0);
        assert_eq!(summary.merged_closed, 1);
        assert_eq!(summary.bead_ids_closed, vec!["testrepo-def456"]);
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
