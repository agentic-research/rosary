# Migration Export Fidelity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve original bead timestamps during Dolt-to-SQLite migration and make contract JSONL dependency ordering deterministic.

**Architecture:** `migrate_store` will restore typed source timestamps immediately after recreating each bead, and `verify_migration` will compare timestamps before allowing the backend swap. Contract export will sort each dependency slice before serializing it, so both record order and nested dependency order are stable regardless of backend query order.

**Tech Stack:** Rust, Tokio, SQLite, existing `BeadStore` and `SqliteBeadStore` test helpers.

## Global Constraints

- Work only on `rosary-3f38c5`; leave `rosary-9b597a` and `rosary-ee49bf` for dedicated follow-up branches.
- Use test-first red/green cycles for every behavioral change.
- Do not synthesize timestamps during migration; malformed source timestamps remain an explicit read error.

---

### Task 1: Preserve and verify source timestamps

**Files:**
- Modify: `src/bead_migrate.rs`
- Test: `src/bead_migrate.rs`

**Interfaces:**
- Consumes: `SqliteBeadStore::restore_timestamps(&self, id, created_at, updated_at)`.
- Produces: a migrated bead whose `created_at` and `updated_at` equal its source values; `verify_migration` rejects timestamp drift.

- [ ] **Step 1: Write the failing migration timestamp test**

```rust
#[tokio::test]
async fn migrate_preserves_created_and_updated_timestamps() {
    let source = store();
    source.create_bead("bead-1", "title", "", 2, "task").await.unwrap();
    source.restore_timestamps(
        "bead-1",
        "2024-01-02T03:04:05Z".parse().unwrap(),
        "2025-06-07T08:09:10Z".parse().unwrap(),
    ).await.unwrap();
    let target = store();
    migrate_store(&source, &target, "repo").await.unwrap();
    let migrated = target.get_bead("bead-1", "repo").await.unwrap().unwrap();
    assert_eq!(migrated.created_at.to_rfc3339(), "2024-01-02T03:04:05+00:00");
    assert_eq!(migrated.updated_at.to_rfc3339(), "2025-06-07T08:09:10+00:00");
}
```

- [ ] **Step 2: Run the test and confirm it fails because migration stamps current time**

Run: `cargo test bead_migrate::tests::migrate_preserves_created_and_updated_timestamps`

Expected: FAIL on timestamp equality.

- [ ] **Step 3: Restore source timestamps during pass one**

```rust
target
    .restore_timestamps(&b.id, b.created_at, b.updated_at)
    .await
    .with_context(|| format!("restoring timestamps for {}", b.id))?;
```

Place this after status/external-reference restoration for each migrated bead.

- [ ] **Step 4: Compare timestamps in the existing field-level migration verifier**

```rust
same!(created_at);
same!(updated_at);
```

Place these assertions alongside the existing scalar field checks.

- [ ] **Step 5: Run the focused tests and commit**

Run: `cargo test bead_migrate::tests`

Expected: PASS.

Commit: `git commit -m "[rosary-3f38c5] fix(beads): preserve migration timestamps"`

### Task 2: Canonicalize contract dependency order

**Files:**
- Modify: `src/import.rs`
- Test: `src/import.rs`

**Interfaces:**
- Consumes: `BeadStore::get_dependencies(&self, issue_id)`.
- Produces: `export_beads_contract_jsonl` whose bytes are identical when equivalent dependency query results arrive in different orders.

- [ ] **Step 1: Write the failing deterministic-export test**

```rust
#[tokio::test]
async fn contract_jsonl_sorts_dependency_arrays() {
    let store = SqliteBeadStore::connect(Path::new(":memory:")).unwrap();
    // Create a bead with dependency ids inserted in reverse lexical order.
    // Export must serialize ["dep-a", "dep-z"] regardless of insertion order.
}
```

Assert the contract record's `dependencies` array is lexically sorted, and export twice to assert byte equality.

- [ ] **Step 2: Run the test and confirm it fails because the dependency query order is used unchanged**

Run: `cargo test import::tests::contract_jsonl_sorts_dependency_arrays`

Expected: FAIL on the dependency array order.

- [ ] **Step 3: Sort dependencies before contract serialization**

```rust
let mut deps = store.get_dependencies(&b.id).await?;
deps.sort();
```

Pass `&deps` to `bead_to_contract_value`; do not change comment order because it is already explicitly chronological.

- [ ] **Step 4: Run focused tests and commit**

Run: `cargo test import::tests`

Expected: PASS.

Commit: `git commit -m "[rosary-3f38c5] fix(beads): canonicalize export dependencies"`

### Task 3: Validate the repair branch

**Files:**
- Verify only.

- [ ] **Step 1: Run formatting and the canonical repository gate**

Run: `cargo fmt --check && task check`

Expected: both commands exit 0.

- [ ] **Step 2: Update the migration bead with concrete verification evidence**

Add a comment on `rosary-3f38c5` naming the tests and gate results. Do not close until the fix is committed, pushed, and merged according to the bead's close condition.

- [ ] **Step 3: Push the repair branch**

Run: `git push -u origin fix/rosary-3f38c5`

Expected: remote branch is created and `git status --short --branch` shows it tracking `origin/fix/rosary-3f38c5`.
