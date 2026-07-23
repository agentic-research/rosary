# Distributed Work-State / Sync — SOTA Prior Art

Fit-for-purpose adaptation of `/prior-art-cartographer` (its shipped 7 IDL/schema
axes don't fit sync systems — the 7 **sync-oriented** axes below replace them).
Decision lens = **borrow** (extract patterns; not wholesale migration).
Every factual claim traces to a primary source fetched 2026-07-20.

## 1. TL;DR

- **The cross-cutting pattern:** every credible local-first work tracker makes
  work-state an **append-only operation log that rides the git remote you
  already have** (git-bug's `refs/<ns>/<id>` op-packs; Radicle COBs; beads via
  `refs/dolt/data`). None commits a mutable binary DB — that's precisely
  rosary's data-loss bug (rosary-05fbe0).
- **The simple thing rosary is missing:** an **op-log CRDT persisted in git refs
  as the live source of truth.** rosary already *designed* exactly this (ADR-0010
  observation lattice = append-only G-set + per-field fold) but it's stuck at R4b
  and `persist_status` still writes status imperatively. Promote the lattice +
  serialize it to git = git-bug's model, which rosary half-built already.
- **Convergence note:** Steve Yegge's `beads` independently landed on rosary's
  exact backend (embedded Dolt, JSONL as export-only). Confirms the substrate
  choice; the open gap for both is *auto-sync* and *derived* (not imperative)
  status.

## 2. Baseline recap — rosary + cloister (us)

- **Storage:** beads = per-repo work items in SQLite `.beads/beads.db` (default,
  now treated as LOCAL) or Dolt (`.beads/dolt/`, server mode). Orchestrator state
  = `~/.rsry/backend.db` SQLite, per-machine.
- **Sync:** MANUAL (`git push` / `dolt push`) — user must remember. No auto-sync.
- **Root pain (rosary-05fbe0):** a committed *binary* bead DB has no git 3-way
  merge → ordinary git ops (checkout/reset/stash-pop) silently overwrite live
  bead state. Fix shipped: untrack the binary, export to a git-tracked JSONL.
- **Merge:** no semantic merge — git text-merge on JSONL, or Dolt cell-merge (but
  Dolt needs its OWN remote: DoltHub/S3+DynamoDB/OCI — can't ride a plain git
  remote). The observation lattice (ADR-0010: append-only G-set + per-field fold,
  a CRDT-ish substrate) is DESIGNED + unit-tested but NOT the live source of
  truth (stuck at R4b); `persist_status` still is → status is imperative.
- **Reconciler, not queue:** rosary is k8s-style scan→triage→dispatch→verify.
  Status *should* derive from observed reality (merge/release events) but that
  derivation isn't fully live.
- **Access:** coarse (repo ACL). Finer-grained intended via ley-line (ChaCha
  content-addressing) + signet (identity) + git-based "wasteland" federation —
  pub/private via repo + key possession.

## 3. Cross-cutting matrix

| Axis | git-bug | cr-sqlite (Vlcn) | Radicle (Heartwood) | beads (Yegge) | Linear Sync Engine | **rosary (us)** |
|---|---|---|---|---|---|---|
| 1 Storage | Git objects under `refs/<ns>/<id>` | SQLite ext: triggers + metadata tables | Git repos, namespace per node key | Embedded Dolt; JSONL = export only | Server Postgres SSOT + client IndexedDB | SQLite `beads.db` or Dolt; binary committed (the bug) |
| 2 Transport | Rides your git remote (push/pull) | Transport-agnostic (you build it) | Gossip + git-v2 fetch via seeds | Dolt remote over `refs/dolt/data` on git remote | GraphQL mutations + WebSocket deltas | Manual `git push` / `dolt push` |
| 3 Merge | Op-based CRDT, DAG + Lamport order | CRDT: causal-length/LWW/counter/fract-index | COB = op-based CRDT (append+merge) | Hash IDs + git/Dolt cell-merge | Server total order via `lastSyncId`, LWW | JSONL text-merge / Dolt cell-merge; lattice not live |
| 4 Offline | Full offline-first | Write offline, merge later | Local-first; net only when needed | Offline-native (all cmds) | Optimistic local, queue + resend | Local works; sync manual |
| 5 Identity | Signed ops; author entity | Not addressed (app's job) | Self-certifying keys sign refs | Inherits git ACL (SSH/HTTPS role) | Account/workspace tenant + sync-groups | Coarse repo ACL; ley-line/signet planned |
| 6 Maturity | Mature, active, bridges to GH/GL | Alpha 0.16.x, dormant (~2yr no release) | 1.0 (2024), production p2p | Active, cross-platform, prod-ish | Mature commercial SaaS | Pre-1.0, dogfooding |
| 7 Fit | **Borrow the op-log-in-refs model** | Borrow CRDT column-types menu | Borrow COB + key-signed refs | Validates our Dolt choice | Borrow monotonic-order idea, skip server | — |

## 4. Per-system sections

### 4.1 git-bug — the reference implementation of what rosary needs

TL;DR: a distributed, offline-first tracker that stores each entity as an
operation-based CRDT inside git objects and syncs over the git remote you
already have. This *is* the pattern rosary's lattice was designed to be.

1. **Storage:** each entity (bug/config) is a series of `Operation`s bundled into
   an `OperationPack` stored as a git `Blob` (JSON array), referenced by a `Tree`
   under `/ops`, wrapped in a `Commit`, chained, and exposed as a git `Reference`
   under `refs/<namespace>/<id>`. Media attach as blobs under `/media`.
2. **Transport:** "use git remotes as a medium for synchronisation and
   collaboration" — pushing the ref pushes all needed objects. No server.
3. **Merge:** operation-based CRDT. Concurrent pulls create a merge commit → a
   DAG with one root. Deterministic replay order = (a) Lamport logical clock when
   not concurrent, else (b) lexicographic order of the OperationPack id. Lamport
   clocks are serialized into `Tree` entry *names* (`create-clock-14`) pointing
   at the empty blob (zero network transfer). DAG structure enforces clock
   monotonicity; violating commits are refused.
4. **Offline:** "distributed, offline-first" — create/comment/close offline, sync
   later.
5. **Identity:** ops carry a signed author entity; signed commits "limit how this
   data model can be abused." Op id = `hash(json(op))` + random nonce for entropy.
6. **Maturity:** mature and active; third-party bridges synchronize with GitHub
   and GitLab (`doc/feature-matrix.md`).
7. **Fit:** the highest-value borrow in this doc.

Verdict:
- **Borrow — op-pack-in-git-refs as the bead wire format.** Store beads as
  operation packs under `refs/beads/<id>` instead of a mutable binary DB. This
  structurally eliminates rosary-05fbe0 (git can't clobber append-only refs) and
  gives free sync over the existing remote.
- **Borrow — Lamport-clock-in-tree-entry-name trick** for zero-transfer logical
  ordering; maps directly onto the lattice's per-field fold order.
- **Skip — its CLI/TUI/bridge surface;** rosary only needs the data model.

Sources:
- git-bug data model (op-CRDT, refs, Lamport, merge algorithm): https://github.com/git-bug/git-bug/blob/master/doc/design/data-model.md (accessed 2026-07-20)
- README (offline-first, git objects not files, push/pull to remotes, bridges): https://github.com/git-bug/git-bug/blob/master/README.md (accessed 2026-07-20)

### 4.2 cr-sqlite (Vlcn) — CRDT semantics bolted onto SQLite

TL;DR: turn any SQLite table into a CRDT via triggers + metadata; converge peers
by exchanging changesets. Great CRDT menu, but you supply the transport, and it's
alpha/dormant.

1. **Storage:** runtime-loadable SQLite/libSQL extension; adds metadata tables +
   triggers around your existing schema (no schema rewrite needed).
2. **Transport:** **transport-agnostic — ships no sync server.** You query local
   changes and ship them yourself.
3. **Merge:** tables = causal-length sets; rows = maps of per-column CRDTs;
   columns choose LWW (default), counter (summation), fractional-index, or
   multi-value register. Changes flow through the `crsql_changes` virtual table
   (read to extract, insert to apply). Peers converge with no conflict.
4. **Offline:** explicit — "write to your SQLite database while offline… both come
   online and merge."
5. **Identity/access:** not addressed — the embedding app's responsibility.
6. **Maturity:** alpha (npm `@vlcn.io/crsqlite` 0.16.x, last release ~2 years
   ago); counter + rich-text CRDTs still incomplete. `[inferred dormant from
   release cadence]`.
7. **Fit:** borrow the *taxonomy*, not the code.

Verdict:
- **Borrow — the column-CRDT menu** (LWW / counter / fractional-index / MV
  register) as a vocabulary for the lattice's `FieldAlgebra` per-field choices
  (`src/observation/algebra_*.rs` already has chain-max / LWW / OR-set /
  flat-lattice — this validates and extends that set).
- **Skip — adopting cr-sqlite itself:** alpha, dormant, no identity, no transport;
  rosary would still have to build sync. rosary's lattice already covers the same
  ground in-tree.

Sources:
- cr-sqlite intro (extension, CRDT + causal event log, offline merge): https://vlcn.io/docs/cr-sqlite/intro (accessed 2026-07-20)
- README (CRDT types per table/row/column, `crsql_changes`, transport-agnostic): https://github.com/vlcn-io/cr-sqlite/blob/main/README.md (accessed 2026-07-20)

### 4.3 Radicle (Heartwood) — sovereign p2p git with CRDT collaborative objects

TL;DR: fully decentralized git collaboration; issues/patches are op-based CRDTs
("COBs"), identity is cryptographic keys, and repos replicate via gossip + git
fetch from seed nodes. The purest "pub/private falls out of key possession" model.

1. **Storage:** standard git repos; each node stores copies of others' repos via
   the git namespace feature keyed by node identifier.
2. **Transport:** two protocols — a **gossip** layer (node/inventory/reference
   announcements for discovery + routing) and the **git v2 smart transfer
   protocol** for actual content, fetched from **seed nodes** (no NAT punching
   yet). Not a plain single git remote — a p2p mesh.
3. **Merge:** issues and patches are **Collaborative Objects (COBs)** — "an
   implementation of a conflict-free replicated data type … any node can append
   data to a COB, and changes from different nodes can be merged without
   conflict." Same op-based-CRDT family as git-bug.
4. **Offline:** "local-first software: network access is only required for
   operations that inherently require communicating with other computers."
5. **Identity:** repos are self-signing — associated with a small set of
   cryptographic signing keys, so identity + contents authenticate regardless of
   where stored. Nodes = public keys.
6. **Maturity:** first 1.0.0-rc announced 2024-03-26; production p2p network.
7. **Fit:** the federation endgame rosary already gestures at (wasteland +
   ley-line + signet).

Verdict:
- **Borrow — self-certifying key-signed refs** as the concrete design for
  ley-line/signet federation: identity = keypair, authenticity travels with the
  object, pub/private = who holds the repo + keys. This is the wasteland model,
  already implemented.
- **Borrow — COB confirms the op-CRDT-in-git choice** independently of git-bug.
- **Skip — the gossip/seed-node p2p transport for now:** heavyweight; rosary's
  hub-and-spoke git remotes are sufficient until true serverless federation is
  the goal.

Sources:
- Radicle protocol / Heartwood (namespaces, gossip + git-v2, COB CRDT, self-signing keys, local-first, seeds): https://lwn.net/Articles/966869/ (accessed 2026-07-20)
- Protocol overview corroboration (gossip message types, key identity): https://radicle.dev/guides/protocol (search-surfaced; page returned 403 on direct fetch 2026-07-20) `[unverified — could not fetch directly]`

### 4.4 beads (Steve Yegge) — independent convergence on rosary's stack

TL;DR: agent-memory issue tracker. Notably, current beads uses **embedded Dolt as
source of truth with JSONL as export only** — the same architecture rosary
landed on — and syncs Dolt over a git remote. Strong external validation; its gap
(auto-sync/derived status) is rosary's gap too.

1. **Storage:** source of truth = embedded Dolt at `.beads/embeddeddolt/`
   (single-writer) or an external `dolt sql-server` (server mode). "`.beads/
   issues.jsonl` is an export for viewers and interchange, not the source of
   truth or a backup." (Note: earlier public write-ups described JSONL *as* the
   source of truth with SQLite as a read cache — the architecture evolved toward
   Dolt. `[architecture changed over time]`.)
2. **Transport:** rides existing git infra — Dolt remotes via `bd dolt push/pull`
   against `refs/dolt/data` on your git remote. **This contradicts our baseline's
   assumption that Dolt always needs its own non-git remote** — Dolt-over-git-refs
   exists and is worth evaluating for rosary.
3. **Merge:** hash-based IDs (`bd-a1b2`) to "prevent merge collisions in
   multi-agent/multi-branch workflows"; Dolt cell-level merge + native branching
   underneath.
4. **Offline:** offline-native — all core commands work without git connectivity
   when `BEADS_DIR` is set.
5. **Identity/access:** inherits git repo ACL; auto-detects maintainer role via
   SSH URL or HTTPS credentials.
6. **Maturity:** actively maintained, cross-platform (macOS/Linux/Windows/
   FreeBSD), production-oriented.
7. **Fit:** confirms the substrate; borrow the git-native Dolt sync path.

Verdict:
- **Borrow — `refs/dolt/data`-over-git-remote sync.** If rosary keeps Dolt for
  server-mode repos, this removes the "Dolt needs DoltHub/S3+DynamoDB" tax and
  lets Dolt beads ride the plain git remote — directly relevant to the sync gap.
- **Borrow — hash-based bead IDs** to kill multi-agent merge collisions (rosary
  already uses `generate_bead_id`; confirm it's collision-resistant across
  branches).
- **Skip nothing structurally — it's a peer.** The shared lesson: both still lack
  auto-sync + derived status.

Sources:
- beads README (Dolt source of truth, JSONL export-only, `refs/dolt/data` sync, hash IDs, offline, git ACL, maturity): https://github.com/gastownhall/beads/blob/main/README.md (accessed 2026-07-20; repo `steveyegge/beads` redirects to `gastownhall/beads`)

### 4.5 Linear Sync Engine — the centralized counter-example

TL;DR: the gold standard for *felt* local-first UX, achieved WITHOUT CRDTs via a
server that assigns a global total order (`lastSyncId`). Instructive as the
opposite pole: rosary is decentralized-git, Linear is server-authoritative.

1. **Storage:** server Postgres = single source of truth; client IndexedDB holds
   a subset. Client can't finalize local state until server delta arrives.
2. **Transport:** GraphQL mutations up, WebSocket delta packets down.
3. **Merge:** **no CRDTs** — a centralized server establishes total order of all
   transactions; `lastSyncId` is a monotonically incrementing integer = global DB
   version; conflicts resolve last-write-wins in that order.
4. **Offline:** optimistic in-memory writes; transactions persist to an IndexedDB
   `__transactions` queue and resend on reconnect; full/partial/local bootstrap.
5. **Identity/access:** multi-tenant workspace; `subscribedSyncGroups` (user/team/
   role arrays) gate model visibility.
6. **Maturity:** mature commercial SaaS; the reverse-engineering was endorsed by
   Linear's CTO as "correct and more complete than what Linear publishes."
7. **Fit:** mostly a contrast; one idea transfers.

Verdict:
- **Borrow — the monotonic global-order idea, applied locally:** rosary's Lamport
  clocks / lattice fold already give a total-ish order without a server. Linear
  proves total order is what makes sync feel instant; rosary should make its
  derived order the live truth (R4b) rather than leaning on imperative
  `persist_status`.
- **Skip — the central Postgres SSOT + WebSocket server.** Antithetical to
  rosary's git-native, offline-first, federated model. rosary already integrates
  Linear as UI (see `src/linear.rs`); it should not adopt Linear's *engine*.

Sources:
- Reverse-engineered Linear Sync Engine (IndexedDB + Postgres SSOT, `lastSyncId` total order, no CRDT/LWW, transaction queue, sync groups, CTO endorsement): https://github.com/wzhudev/reverse-linear-sync-engine (accessed 2026-07-20)

### 4.6 Entire CLI (Thomas Dohmke, ex-GitHub CEO) — agent-session capture on a git branch

Fetched 2026-07-22 from https://github.com/entireio/cli and https://entire.io.
MIT-licensed, Go (98.9%), $60M seed at ~$300M valuation announced Feb 2026.

- **What it is:** a Git observability layer for *AI agent sessions*, not work
  items. `entire enable` installs per-agent hooks (Claude Code via
  `.claude/settings.json`, plus Codex, Copilot CLI, Cursor, Factory Droid,
  Gemini CLI, OpenCode, Pi) that capture transcripts, prompts, tool calls and
  files-touched as you work.
- **Storage (the load-bearing detail):** "Checkpoints" — 12-char hex ids — are
  stored on a **separate git branch, `entire/checkpoints/v1`**, explicitly *not*
  as refs or notes, and *not* in the working tree. Session metadata is
  structured records rather than content-addressed blobs.
- **Transport:** rides the repo's existing remote by default; a distinct
  `--checkpoint-remote` can send metadata to a *different* remote than the code.
- **Caution it documents:** "If your repository is public, this data is visible
  to anyone." Agent transcripts on a pushed branch are a disclosure surface —
  directly relevant to rosary's write-time secret scrubber (`src/secrets.rs`),
  which would need to cover any transcript rosary ever ships.

**Why it matters here — fifth independent confirmation.** Entire had no reason
to converge with git-bug/Radicle/beads, and did anyway: *work/agent state rides
the git remote you already have, and lives OUTSIDE the working tree.* Four of
five systems use refs (`refs/<ns>/<id>`, COBs, `refs/dolt/data`); Entire uses a
branch. None puts a mutable file in the working tree. That is now the strongest
signal in this document.

**Where it extends the analysis.** This doc scoped "work-state". Entire is the
first surveyed system covering *execution lineage* — the ADR-0015 capsule
concept, shipped. It also proves the integration point rosary already uses
(hooking `.claude/settings.json`) is enough to capture sessions passively,
which bears on `rsry capture --from-session` (today: manual, one transcript at
a time) and on rosary-2268aa (notifying an active session).

**Honest tension with what rosary just built (rosary-4ebf52, PR #399).** The
JSONL sync shipped there puts `.beads/beads.jsonl` in the **working tree** and
stages it from `pre-commit` — deliberately, because a working-tree file is
*reviewable in a PR diff* ("this commit added exactly one bead"), which
refs/branches are not. That benefit is real and none of the five systems
provide it. But it is the outlier design, and it buys the reviewability with
the costs the others avoid: commit-coupling (bead churn lands in code commits),
merge conflicts against code changes, and a hook that must stage a file. The
migration path is already recommendation #1 below — op-log in `refs/beads/<id>`
— so PR #399 should be read as the *pragmatic step that makes state text and
mergeable*, not as the endpoint. Deciding between "reviewable in-tree" and
"clean out-of-tree" is a real fork, and Entire shows a third option: keep both,
by putting state on a branch and rendering it into review separately.

## 5. What rosary should borrow — prioritized

1. **Promote the observation lattice to the live source of truth and serialize it
   as an op-log in git refs** (git-bug §4.1 + Radicle COB §4.3). This is the
   headline move: it simultaneously (a) fixes rosary-05fbe0 for good — append-only
   refs can't be clobbered by checkout/reset/stash-pop the way a binary DB is;
   (b) unblocks R4b (rosary-a66b3a) — status *derives* from the fold instead of
   imperative `persist_status`; (c) gives free auto-sync over the existing git
   remote. Where: `src/observation/*` (lattice already = append-only G-set +
   per-field fold), a new serializer writing packs under `refs/beads/<id>`, and
   retire imperative writes in `persist_status`. rosary already *designed* exactly
   git-bug's data model — finish it.

2. **Adopt Dolt-over-`refs/dolt/data` for server-mode repos** (beads §4.4). Kills
   the baseline assumption that Dolt needs DoltHub/S3+DynamoDB and lets Dolt beads
   ride the plain git remote you already push. Where: `src/bead_dolt.rs` /
   `src/dolt/mod.rs` sync paths; evaluate `bd`'s `refs/dolt/data` push/pull as
   prior art. Lower-lift than #1 and directly closes the manual-sync gap for the
   Dolt tier.

3. **Formalize the per-field CRDT menu and key-signed identity** (cr-sqlite §4.2 +
   Radicle §4.3). Use cr-sqlite's column-CRDT taxonomy (LWW / counter /
   fractional-index / MV-register) to review/extend `src/observation/algebra_*.rs`
   coverage, and adopt Radicle's self-certifying key-signed refs as the concrete
   design for ley-line/signet federation (identity = keypair, pub/private = key +
   repo possession — the wasteland model). Where: `src/observation/registry.rs`
   (algebra dispatch) and the ley-line/signet integration seam.

4. **Separate the metadata remote from the code remote** (Entire §4.6). Entire's
   `--checkpoint-remote` lets work/session state push somewhere other than the
   source repo. For rosary that is the cheap answer to two open problems at once:
   private bead state on a public repo, and the disclosure surface a pushed
   transcript creates. Pairs with `src/secrets.rs` scrubbing whatever ships.

Cross-cutting caution: do **not** adopt a central server (Linear §4.5) or a
gossip/seed p2p mesh (Radicle transport) yet — both are heavier than rosary's
hub-and-spoke git remotes need to be. The winning move is the *cheap* one every
git-native tool already proved: op-log in refs on the remote you already have.
