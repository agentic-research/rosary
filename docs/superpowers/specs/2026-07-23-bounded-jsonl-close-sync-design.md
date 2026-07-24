# Bounded JSONL Close Sync

## Problem

Closing a bead changes only the local store. If close is the final action after
an implementation commit, no later commit runs the pre-commit exporter, so the
tracked JSONL remains stale. The existing pre-commit exporter also serializes
the entire local store, which can republish records intentionally omitted from
the public projection.

## Design

One bounded projection function owns the behavior for every surface. It reads
the IDs already present in the tracked JSONL, replaces those records with their
current live-store representation, preserves published records unavailable in
the local store, sorts by ID, and never adds a live-store-only ID.

CLI close and MCP close invoke this function immediately after the store close.
The pre-commit hook invokes the same projection through a bounded export flag,
so all three paths have identical publication semantics. Writes use a temporary
file followed by an atomic rename.

## Error handling

Malformed published JSONL and serialization failures fail loudly. A successful
local close followed by a projection failure returns an error explaining that
the store is closed but the public projection was not refreshed. Repositories
without tracked JSONL and Dolt-backed repositories remain no-ops.

## Falsifiable verification

An end-to-end fixture creates one published bead and one local-only bead, tracks
only the published record, and closes it. The test fails unless JSONL changes
immediately to terminal status and still contains exactly one record. A direct
MCP-handler test applies the same setup and assertion, preventing CLI/MCP drift.
