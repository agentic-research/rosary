# Bounded JSONL Close Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make CLI close, MCP close, and pre-commit refresh already-published JSONL records without publishing local-only beads.

**Architecture:** Add one bounded projection renderer and one tracked-file refresh helper in `src/jsonl_sync.rs`. CLI and MCP close call the helper; `bead export --published-from` and the pre-commit hook reuse the renderer.

**Tech Stack:** Rust, Tokio, Clap, shell git hooks, SQLite fixture stores.

## Global Constraints

- Never invoke or depend on the legacy bead CLI.
- Never add a live-store ID that is absent from the current public JSONL.
- Preserve published records that are absent from the local store.
- Keep Dolt and non-opted-in repositories unchanged.

---

### Task 1: Pin CLI and MCP behavior

**Files:**
- Create: `tests/close_jsonl_sync.rs`
- Modify: `src/serve/handlers/tests.rs`

**Interfaces:**
- Consumes: existing `rsry bead close` CLI and `tool_bead_close`.
- Produces: two failing behavioral tests requiring immediate bounded refresh.

- [x] Write the CLI fixture with one public and one local-only bead.
- [x] Run `cargo test --test close_jsonl_sync -- --nocapture`.
- [x] Confirm failure is `open` versus expected `done`.
- [x] Add and run the equivalent direct MCP handler test; confirm the same failure.

### Task 2: Add the bounded projection primitive

**Files:**
- Create: `src/jsonl_sync.rs`

**Interfaces:**
- Produces: `export_published_beads_contract_jsonl`, which renders only existing public IDs, and `refresh_tracked_beads_jsonl`, which atomically refreshes opted-in JSONL.

- [x] Add unit coverage for live replacement, missing-local preservation, stable sorting, and local-only exclusion.
- [x] Implement the minimal renderer and atomic tracked-file refresh.
- [x] Run the focused unit tests and both close tests.

### Task 3: Wire every adapter to the primitive

**Files:**
- Modify: `src/main.rs`
- Modify: `src/serve/handlers/mod.rs`
- Modify: `docs/git-hooks/pre-commit`

**Interfaces:**
- Consumes: bounded renderer and tracked-file refresh from Task 2.
- Produces: identical CLI, MCP, and pre-commit semantics.

- [x] Add `--published-from` to JSONL export and use it in pre-commit.
- [x] Invoke tracked refresh after CLI close.
- [x] Resolve the MCP repo root and invoke the same tracked refresh after MCP close.
- [x] Run focused tests, `cargo fmt --check`, and `task check`.
