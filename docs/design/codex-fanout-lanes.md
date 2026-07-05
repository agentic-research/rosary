# Codex fan-out lanes — safe parallel dispatch of the rosary backlog

**Bead:** rosary-5f45d2 · **Date:** 2026-07-01 · **Status:** superseded snapshot — the hand-computed "dispatchable" count is now code-enforced (`Bead::is_dispatchable`, rosary-d4bb09) and the god-file driver `src/serve/handlers.rs` was split (#294); treat the figures as a historical lane map

## Goal

Turn the rosary backlog into a structure where **many Codex workers fan out
safely** while one orchestrating thread coordinates. "Safely" = no two
concurrently-dispatched beads write the same files (rosary's own
`epic::has_file_overlap` gate is the safety net; this doc pre-computes it).

## Method

Union-find over the **file-overlap graph** of the 264 dispatchable
(bug/task/feature/chore) open rosary beads. Two beads share an edge iff their
file scopes intersect. Connected components = beads that MUST serialize;
singletons = beads that collide with nothing → the parallel-safe queue. Scopes
were validated against the working tree, so **stale scopes are excluded** from
the graph (see below).

## The two blockers to fan-out (findings)

### 1. 86 beads carry STALE file scopes

Overlap detection is only as good as the scopes. 86 dispatchable beads point at
files that no longer exist — overwhelmingly the **monolithic paths that were
since split**:

| Stale scope        | Real location today                             |
| ------------------ | ----------------------------------------------- |
| `src/dispatch.rs`  | `src/dispatch/{mod,providers,session,sweep}.rs` |
| `src/serve.rs`     | `src/serve/{mod,handlers,tools}.rs`             |
| `src/reconcile.rs` | `src/reconcile/{mod,verify,persistence,…}.rs`   |
| `src/session.rs`   | (still exists) / split usages                   |

Others point at **never-created planned files** (`src/reflog.rs`,
`src/issue_type/*.rs`, `src/store_d1.rs`, `src/policy/*.rs`) — design beads whose
scopes are aspirational. A stale scope silently **removes** a bead from overlap
detection, so two beads that really touch `src/dispatch/mod.rs` but are scoped to
the old `src/dispatch.rs` look independent and get dispatched together → collision.

**Remediation:** refresh the 86 stale scopes to real split paths before trusting
fan-out. This is a **mechanical metadata edit** — currently MCP-gated (blocked on
rosary-080934, the MCP-hydration bug). Until then the graph below excludes stale
scopes, which is safe (understates parallelism) but the 86 need fixing to widen
the lanes.

### 2. One 130-bead "god-file" collision cluster

Half the dispatchable backlog fuses into a **single serial component** because
everything shares a few hot files:

```
cluster[130]  hot = src/serve/handlers.rs · src/main.rs · src/dispatch/mod.rs
```

These files are shared by ~everything, so the overlap graph says "130 beads must
serialize." That is the structural reason the backlog doesn't fan out. The fix is
**decomposition, not scheduling** — split the god-files so work stops routing
through them:

- `src/serve/handlers.rs` → the L0 op-core extraction (`src/bead_ops.rs`, PR
  #256/#257) is exactly this: each op moves out of the shared handler into its
  own unit. Continuing that shrinks the cluster structurally.
- `src/main.rs` (3k lines) → extract CLI subcommand handlers.
- `src/dispatch/mod.rs` → the agent-native harness work (rosary-5a9a88) already
  moves logic into `providers.rs`/`session.rs`.

## The parallel-safe queue (18 rosary-src singletons)

Each collides with **nothing** — dispatch any subset concurrently, today:

| Bead          | P   | files                                                                 |
| ------------- | --- | --------------------------------------------------------------------- |
| rosary-3eef9c | 1   | `src/serve/github_webhook.rs`                                         |
| rosary-9c4c7a | 1   | `crates/bdr/src/{channels,harmony}.rs`, `src/reconcile/completion.rs` |
| rosary-e13ecf | 1   | `crates/bdr/src/accrete.rs`                                           |
| rosary-fdb65b | 1   | `src/decompose.rs`, `src/import.rs`                                   |
| rsry-c1a52f   | 1   | `crates/bdr/src/parse.rs`                                             |
| rsry-eb0b83   | 1   | `scripts/e2e-sandbox.sh`                                              |
| rosary-42eccd | 2   | `src/observation/resolve.rs`                                          |
| rosary-9780c8 | 2   | `src/observation/algebra_chain.rs`                                    |
| rosary-979603 | 2   | `src/observation/algebra_lww.rs`                                      |
| rosary-97e386 | 2   | `src/observation/{log,quarantine}.rs`                                 |
| rosary-983757 | 2   | `src/observation/tree_fold.rs`                                        |
| rosary-b6367a | 2   | `src/reconcile/verify.rs`                                             |
| rosary-e1e4bc | 2   | `scripts/statusline.sh`                                               |
| rosary-a7f18e | 3   | `src/dolt/{mod,tests}.rs`                                             |
| …             |     | (+ crates/rosary-beads, rsry-mcp-bundle bundle beads)                 |

## Starter fan-out batch (dispatch these first, in parallel)

Eight beads, pairwise non-colliding, priority-ordered — a concrete first wave:

1. **rosary-3eef9c** (P1) — `github_webhook.rs`: write pr_url event on webhook match
1. **rosary-9c4c7a** (P1) — bdr channels/harmony + reconcile/completion: event bus + bead correlation
1. **rosary-e13ecf** (P1) — `accrete.rs`: wire accrete() into reconciler on close
1. **rosary-fdb65b** (P1) — decompose/import: auto-decompose external issues
1. **rsry-c1a52f** (P1) — `parse.rs`: mache schema-driven doc classification
1. **rsry-eb0b83** (P1) — `e2e-sandbox.sh`: automated no-HITL dispatch-verify-close loop
1. **rosary-42eccd** (P2) — `observation/resolve.rs`: verify undercut proof
1. **rosary-b6367a** (P2) — `reconcile/verify.rs`: mechanical merge gate

The whole `src/observation/algebra_*` family (rosary-9780c8/979603/97e386/983757)
is also mutually non-colliding — a natural second wave for one worker or several.

## Collision clusters (must serialize within)

| cluster          | size | hot files                                 | note                                                      |
| ---------------- | ---- | ----------------------------------------- | --------------------------------------------------------- |
| god-file         | 130  | handlers.rs · main.rs · dispatch/mod.rs   | inflated by stale scopes + god-files; decompose to shrink |
| dispatch-legacy  | 27   | `src/dispatch.rs` (stale!) · reconcile.rs | mostly evaporates once scopes refresh to split paths      |
| capnp-substrate  | 7    | README · CLAUDE.md · lib.rs               | the `[capnp-substrate]` thread — intentional sequence     |
| signet-docs      | 3    | signet/docs/\*                            | cross-repo (target signet, not rosary src)                |
| observation-fold | 3    | observation/fold.rs · algebra_orset.rs    | ADR-0010 follow-ups                                       |

## Recommendation

1. **Fan out the 8-batch now** (or the 18 singletons) — genuinely parallel-safe.
1. **Fix the 86 stale scopes** (mechanical, MCP-gated on rosary-080934) — this
   widens the lanes by pulling beads out of the phantom `src/dispatch.rs` cluster.
1. **Keep decomposing the god-files** (handlers.rs/main.rs) — the only way to
   shrink the 130-cluster structurally. Parallelism is a *decomposition* outcome,
   not a *scheduling* one.
1. Cross-repo beads (signet/ll/conductor) living in the rosary store are a
   separate relocation concern (`rsry bead move`), out of scope here.

> Thread/decade assignment + dependency edges (the "documented via bead metadata"
> half of the acceptance) are MCP-gated (rosary-080934). This doc is the durable
> lane map; the metadata application follows once the MCP write path is restored.
