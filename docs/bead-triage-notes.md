# Bead Triage Notes — 2026-03-19

## Inventory Summary

| Repo      | Open    | Closed/Done | Total Scanned |
| --------- | ------- | ----------- | ------------- |
| rosary    | ~45     | ~30         | ~75           |
| mache     | ~30     | ~20         | ~50           |
| ley-line  | 14      | 2           | 16            |
| crumb     | 2       | 3           | 5             |
| signet    | 4       | 6           | 10            |
| **Total** | **~95** | **~61**     | **~156**      |

Note: rosary bead_search caps at 50 results per query. Multiple queries with different terms were used to maximize coverage. Some beads may have been missed if they don't match any search term used.

______________________________________________________________________

## Phase 2: Duplicate Analysis

### Cluster 1: BDR Decompose + Enrichment Pipeline

These beads form a clear dependency chain but were created separately. The enrichment Phase beads (rosary-ca47f7, rosary-ca882d, rosary-cb1a41, rosary-cb5756, rosary-cb9a99) are properly ordered sub-beads of rosary-b2cb1a. No duplication — they are a well-decomposed pipeline.

- **rosary-b2cb1a** — "BDR decompose: schema-driven with success criteria" (P0, epic-level)
- **rosary-ca47f7** — Phase 1: type-state framework (P1, depends on b2cb1a)
- **rosary-ca882d** — Phase 2: symbol resolution via mache (P1, depends on ca47f7)
- **rosary-cb1a41** — Phase 3: embedding-based dedup (P1, depends on ca882d)
- **rosary-cb5756** — Phase 4: haiku LLM validation (P2, depends on cb1a41)
- **rosary-cb9a99** — Phase 5: pre-flight + materialization (P1, depends on 3 others)
- **mache-cbf644** — mache_embedding_similarity MCP tool (P1, mache-side of cb1a41)

**Verdict**: No dupes. Well-structured pipeline. Thread candidate.

### Cluster 2: Dedup Beads (overlapping scope)

Multiple beads about dedup, created at different times with overlapping goals:

- **rosary-a26a78** — "Active dedup: agent verifies uniqueness before starting work" (P1, open)
- **rosary-984b20** — "Epic: ADR-003 blast-radius dedup — three-layer dedup" (P1, epic, open)
- **rsry-b3bfbe** — "SOTA dedup: beyond Jaccard — tropo + embeddings" (P2, open)
- **rosary-98ec56** — "Phase 4: Validation — re-run experiment" (P2, open)
- **rosary-e61486** — "Symbol-scoped beads — AST-level dispatch isolation" (P1, design, open)
- **loom-84n** — "Bead dedup: content-similarity check" (P1, CLOSED)
- **loom-vrc** — "Bead reconciler: semantic grouping" (P1, CLOSED)
- **rsry-55c311** — "Dedup false positives" (P2, CLOSED)

**Analysis**:

- loom-84n and loom-vrc are CLOSED — the initial Jaccard dedup is implemented in epic.rs.
- rsry-b3bfbe ("SOTA dedup: beyond Jaccard") is SUBSUMED by rosary-984b20 (ADR-003 blast-radius dedup) and the BDR enrichment pipeline (cluster 1). The three-layer model in 984b20 IS the "beyond Jaccard" work, and cb1a41 implements the embedding part.
- rosary-a26a78 ("active dedup") is a different concern — agent-side verification vs system-side filtering. Distinct from 984b20.
- rosary-98ec56 is a sub-bead of 984b20 (validation phase).
- rosary-e61486 is related but distinct — it's about scoping dispatch isolation by AST symbols, not about dedup.

**Action**: CLOSE rsry-b3bfbe as dupe of rosary-984b20 + rosary-cb1a41.

### Cluster 3: E2E Dispatch Loop

- **rsry-d93546** — "E2E dispatch loop: agent gets code, works, commits, closes bead" (P0, open)
- **rosary-a3b2cf** — "E2E pipeline integration test — containerized full-loop validation" (P0, DONE)

**Analysis**: rsry-d93546 describes the same E2E flow as rosary-a3b2cf which is already DONE. The sub-tasks in d93546 (verify worktree creation, session registry, agent execution, bead closure, cleanup) are all covered by the work in a3b2cf.

**Action**: CLOSE rsry-d93546 as completed by rosary-a3b2cf.

### Cluster 4: Silent Agent Failures

- **rosary-39d1bc** — "Dispatched agents can crash silently — no worktree, no error" (P0, open)
- **rosary-184dd9** — "rsry_active reports dead PIDs as healthy" (P0, open)

**Analysis**: These are closely related but distinct. 39d1bc is about agents that exit without producing artifacts. 184dd9 is about the health check being wrong. Both need fixing but are different failure modes. 184dd9 blocks 39d1bc conceptually (can't detect crash if you can't detect death).

**Verdict**: Related, not duplicate. Link them.

### Cluster 5: Dolt Init on Enable

- **rosary-e504f8** — "rsry_bead_create silently succeeds without per-repo Dolt DB" (P0, open)
- **rosary-e4f182** — "rsry enable should init .beads/ Dolt DB if missing" (P0, open)

**Analysis**: These are two views of the same problem. e4f182 is the fix (enable should init Dolt), e504f8 is the symptom (create succeeds but beads are in limbo). The fix for e4f182 resolves e504f8.

**Action**: CLOSE rosary-e504f8 as dupe of rosary-e4f182 (fixing enable to init Dolt resolves the silent create issue).

### Cluster 6: Control Room iOS App

- **control-room-a26ca8** — "Create shared JSONDecoder" (P0, open)
- **control-room-9910a6** — "MCPClient.swift — Observable API client" (P0, open)
- **control-room-9889f9** — "Validate Rork models against actual rsry MCP JSON shapes" (P0, open)

**Analysis**: These are the iOS control room app beads. They belong to a separate project (control-room-108) but are stored in the rosary repo. They form a dependency chain (a26ca8 → 9910a6 depends on it, 9889f9 validates models). Not duplicates.

**Verdict**: Distinct, sequential. Leave open. SKIP for thread assignment — they belong to a separate product area.

### Cluster 7: Workspace Dispatch Isolation

- **rosary-ea6c7f** — "rsry_dispatch must refuse dispatch without workspace isolation" (P0, open)
- **rosary-cd9cbb** — "jj workspace isolation for all dispatch" (P0, CLOSED)
- **rosary-3e5cf7** — "Workspace lifecycle coordination" (P0, CLOSED)
- **rosary-a36e29** — "Worktree isolation leak" (P0, CLOSED)
- **rosary-3cfe41** — "Agent worktrees and branches deleted before merge" (P0, CLOSED)

**Analysis**: rosary-ea6c7f is the only open one. The other workspace isolation bugs are closed. ea6c7f specifically requests that dispatch REFUSE to run without isolation. Given that cd9cbb (jj workspace isolation) is closed, the mechanism exists — ea6c7f is about the default behavior. Still valid.

**Verdict**: Keep ea6c7f open. The others are properly closed.

### Cluster 8: Linear Sync + Webhooks

- **rsry-eaf9e5** — "Linear auto-setup: project-per-repo, milestones-per-phase" (P1, open)
- **rosary-c3b232** — "ADR-003: Linear hierarchy mapping + upstream config" (P0, design, open)
- **rsry-cc97bb** — "Agent roles as Linear assignees" (P1, design, open)
- **rsry-04390a** — "Linear custom field bead_id" (P1, open)
- **rsry-795299** — "Linear webhook auto-registration" (P1, open)
- **rosary-a111a3** — "rsry sync does not reconcile Linear→bead" (P1, bug, open)
- **rsry-78dcd6** — "Auto-provisioned tunnel + webhook" (P1, epic, open)
- **rsry-79873a** — "Integration: rsry serve orchestrates HTTP + tunnel + webhook" (P2, open)
- **rosary-18aa35** — "Linear sync: push bead comments as comments" (P2, open)
- **rsry-45fb87** — "HITL recovery: poll Linear comments for /resume" (P1, open)

**Analysis**:

- rsry-eaf9e5 and rosary-c3b232 overlap significantly. c3b232 is the ADR/design doc, eaf9e5 is the implementation. They're related but distinct (design vs impl).
- rsry-cc97bb is about agent role mapping to Linear — this is part of the c3b232 design (which explicitly mentions role mapping). However, cc97bb focuses on assignees specifically.
- rsry-04390a (custom field) is a concrete sub-task of the Linear integration.

**Verdict**: No closeable dupes. These should be grouped into a thread.

### Cluster 9: Feature Branch Lifecycle + PR

- **rsry-988505** — "Feature branch lifecycle: source → feature → dev branches" (P0, design, open)
- **rosary-1e40db** — "Full feature pipeline: pre-flight → PR → implement → review → merge" (P1, epic, open)
- **rosary-1e137e** — "PR-as-execution-surface" (P1, feature, open)
- **rsry-8cbbe7** — "PR merge → bead close + review dispatch trigger" (P1, open)
- **rsry-8c9fab** — "PR create → bead verifying transition + URL linkage" (P1, open)
- **rsry-0d0e28** — "Feature rollup: combine closed beads into feature branch PR" (P0, design, open)
- **rosary-87a4fc** — "Conventional commits + GH automation" (P2, design, open)

**Analysis**: rsry-988505 and rosary-1e40db are highly overlapping — both describe the multi-phase pipeline from feature to merge. 988505 is the branch strategy, 1e40db is the conductor pipeline template. They're complementary views of the same system.

**Verdict**: Keep both — 988505 is VCS architecture, 1e40db is orchestration architecture. Thread them together.

### Cluster 10: Compute Providers / Execution Backend

- **rsry-e4e88f** — "Pluggable ExecutionBackend: sprites.dev first, local fallback" (P1, epic, open)
- **rosary-a246fa** — "Docker compute provider" (P2, open)

**Verdict**: Not duplicates. a246fa is a sub-bead of e4e88f. Thread candidate.

### Cluster 11: Session/Observability

- **rsry-9b0d64** — "Pause/play architecture: session persistence for blocked agents" (P0, design, open)
- **rosary-cdf5df** — "Dynamic prompt enhancement: agent .md files as templates" (P1, open)
- **rsry-45dd10** — "AgentRouter: complexity-based provider selection" (P0, open)

**Verdict**: All distinct concepts. No dupes.

### Cluster 12: Service Boundaries / Architecture

- **rsry-bdfec6** — "Architecture: control plane / agent plane separation" (P1, design, open)
- **rsry-c9116a** — "rsry serve HTTP should auto-daemonize" (P1, open)
- **rsry-a036f7** — "Homebrew service: rosary as launchd daemon" (P1, open)
- **rosary-31e9db** — "Fully hosted rosary" (P2, epic, open)

**Analysis**: rsry-c9116a and rsry-a036f7 overlap. c9116a is about daemonization (PID file, logging, start/stop). a036f7 is about Homebrew service (launchd plist, brew services). These are the same concern (daemon lifecycle) from different angles. a036f7 is more specific and actionable.

**Action**: CLOSE rsry-c9116a as subsumed by rsry-a036f7 (homebrew service covers daemonization).

### Cluster 13: ley-line OTP + Instance-to-Instance

- **ley-line-736b44** — "OTP supervision + instance-to-instance communication" (P1, epic, open)
- **ley-line-73c2ba** — "Bidirectional transport: extend sender/receiver" (P1, open)
- **ley-line-73e878** — "Arena reconciliation protocol" (P2, open)
- **ley-line-73db51** — "Distributed event bus" (P2, open)
- **ley-line-73cefc** — "Peer discovery and instance registry" (P2, open)

**Verdict**: These are a well-structured epic (736b44) with sub-beads. No dupes. Thread candidate.

### Cluster 14: ley-line CI/Release

- **ley-line-5df113** — "Fix Rust CI for private repo (no GHA minutes)" (P1, open)
- **ley-line-5dea33** — "Publish linux release artifacts" (P1, open)
- **ley-line-1fx** — "CI: add release workflow" (P3, open)
- **ley-line-kl5** — "CI: add permissions: contents: read" (P4, open)

**Analysis**: 5df113 (CI fails on private repo) and 1fx (add release workflow) are related but distinct — 5df113 is a blocker, 1fx is about the release mechanism itself.

**Verdict**: No dupes. Thread candidate.

### Cluster 15: Handoff / Context Beads

- **ley-line-11r** — "HANDOFF: Fix bead consumption — production outpaces work 10:1" (P1, open)

**Analysis**: This is a stale handoff bead from 2026-03-13. It describes problems (loom-34x, loom-t02, loom-15i) that have been partially addressed. The three "pillars" reference old loom-prefixed IDs. Much of this is now done (bead CRUD is built, dispatch works, MCP is primary interface). However, the core problem (bead production outpaces consumption) may still be valid.

**Verdict**: SKIP for human review. This is a stale meta-bead that may need updating or closing.

### Cluster 16: Mache VFS/Graph Bugs

All distinct bugs in different files with different symptoms:

- mache-5c5366 (release.yml bug)
- mache-5c4c3b (VFS handler gap)
- mache-5c416f (SQL injection)
- mache-5c76b3 (unbounded batch read)
- mache-5c6f1b (orphaned daemon)
- mache-5c6836 (leaked SQLite connections)
- mache-5c5a44 (WritableGraph ListChildren bug)
- mache-5ca676 (aliased pointers under RLock)
- mache-5c9356 (symlink containment check)

**Verdict**: All distinct. No dupes. Many are P1/P2 bugs that should be grouped.

### Cluster 17: Mache Schemas and Features

- mache-8eb3e6 — "BDR schema: project decade/thread/bead hierarchy" (P1)
- mache-85t — "Beads schema: project beads as browsable filesystem" (P2)
- mache-gsi — "Schema for investigation logs" (P2)
- mache-kv0 — "Schema for Claude Code conversation history" (P2)
- mache-8qq — "Extract meta-patterns from CC history" (P2)
- mache-c0n — "Schema-driven projection from external type systems" (P2)
- mache-tkr — "Terraform ACI" (P2)

**Analysis**: mache-8eb3e6 and mache-85t have overlapping scope — both project beads from Dolt. 8eb3e6 adds hierarchy (decade/thread/bead), 85t is the flat bead projection with write-back. 8eb3e6 supersedes 85t (it includes beads as part of the hierarchy).

**Action**: CLOSE mache-85t as subsumed by mache-8eb3e6 (BDR schema includes bead projection within the hierarchy).

### Cluster 18: Mache Write-back

- mache-b1w — "Write-back support for JSON data sources" (P2, epic)
- mache-b1w.1 — "Design write-schema spec" (P2)
- mache-b1w.2 — "Track JSONPath origin metadata" (P2)
- mache-b1w.3 — "Implement JSON splice write-back" (P2)

**Verdict**: Well-structured epic with sub-beads. No dupes. Thread candidate.

### Cluster 19: Mache Code Intelligence

- mache-bsq — "detect_changes — git diff → blast radius" (P2)
- mache-ok2 — "trace/ virtual dir — transitive call path" (P2)
- mache-8zq — "\_diagnostics/doc-drift" (P2)
- mache-ml2 — "Mermaid diagram output" (P2)
- mache-e49cc4 — "find duplicate function signatures" (P2)
- rsry-b003dd — "Expose file-level imports as virtual node" (P2)
- rsry-b003eb — "Transactional/Atomic multi-node writes" (P2)
- rsry-b0040c — "Sticky/Zombie node handles" (P2)
- mache-axk — "Agent-mode MCP: auto-provision" (P2)

**Verdict**: All distinct features. No dupes. Thread candidates for "mache code intelligence" and "mache write path".

### Cluster 20: Misc Rosary

- **rosary-f0af8f** — "Switch to content-hash bead IDs" (P2, open)
- **rsry-6f2185** — "Bead metadata normalization" (P1, open)
- **rsry-68f6a9** — "rsry dispatch by bead ID (auto-resolve repo)" (P0, open)
- **rsry-460b10** — "Add verifying state" (P0, open)
- **rosary-e9e9db** — "rsry_bead_update — PATCH semantics" (P0, open)
- **loom-2al** — "Phase-gated development model" (P0, epic, open)
- **loom-2m3** — "Conversational bead capture: /btw skill" (P1, open)
- **loom-w8c** — "Agent hierarchy: prod / staging / feature / dev / PM" (P2, epic, open)
- **loom-w8c.5** — "Meta-agent: cross-session idea dedup" (P2, open)
- **rosary-57c4fb** — "GitHub App: rosary[bot] identity" (P2, open)
- **rosary-735446** — "Add krust OCI image builds" (P2, open)
- **rsry-b3afc9** — "Public bead hashes on GitHub remote" (P2, open)

**Verdict**: All distinct. No closeable dupes.

______________________________________________________________________

## Phase 2 Observations

1. **The search cap at 50 means some rosary beads may not have been reviewed.** The dedup analysis covers the beads found across multiple targeted searches.

1. **Four actionable duplicates found:**

   - rsry-b3bfbe subsumed by rosary-984b20 + rosary-cb1a41
   - rsry-d93546 completed by rosary-a3b2cf
   - rosary-e504f8 dupe of rosary-e4f182
   - rsry-c9116a subsumed by rsry-a036f7
   - mache-85t subsumed by mache-8eb3e6

1. **One stale handoff bead**: ley-line-11r needs human review.

1. **The BDR enrichment pipeline (cluster 1) is excellently decomposed** — no intervention needed.

1. **The dedup cluster (cluster 2) has organic overlap** from beads created at different times as the design evolved. The closed beads (loom-84n, loom-vrc) represent completed iterations; the open ones (984b20, cb1a41, a26a78) represent the next generation.

______________________________________________________________________

## Phase 3: Thread/Decade Organization

### Proposed Decades

#### Decade A: "BDR & Decomposition Quality"

- Thread A1: BDR Core (schema-driven decompose)
- Thread A2: Enrichment Pipeline (mache resolution, dedup, validation)
- Thread A3: Active Dedup (runtime dedup at triage/dispatch)

#### Decade B: "Agent Hierarchy & Dispatch"

- Thread B1: Agent Roles & Routing (roles, provider selection, prompt enhancement)
- Thread B2: Compute Providers (Sprites, Docker, local)
- Thread B3: Pipeline Lifecycle (feature branches, PR flow, merge)
- Thread B4: Dispatch Quality (isolation, silent failures, observability)

#### Decade C: "Infrastructure & Workflow"

- Thread C1: Linear Integration (sync, webhooks, hierarchy mapping)
- Thread C2: Service Lifecycle (daemon, hosting, tunnel, launchd)
- Thread C3: Build/Release (krust OCI, CI, GitHub App)

#### Decade D: "Cross-Repo Architecture"

- Thread D1: Reactive Store (ADR-005, local firebase)
- Thread D2: ley-line OTP & Networking
- Thread D3: Mache Code Intelligence
- Thread D4: Mache Schemas & Projections

#### Unassigned

- control-room iOS beads (separate product)
- crumb beads (separate repo, low volume)
- signet beads (separate repo)
- Stale handoff beads
- Meta/phase-gate beads (loom-2al, loom-w8c)
