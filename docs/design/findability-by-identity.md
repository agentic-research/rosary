# Findability by Identity

**Status:** Draft design (principal-architect analysis)
**Date:** 2026-07-20
**Scope:** ART ecosystem — rosary, cloister, ley-line, signet
**Relates to:** rosary ADR-0010 (observation lattice), ADR-0012 (personal bead
substrate), ADR-0014 (own the store), ADR-0016 (dispatch via cloister),
cloister ADR-0003 (content-addressed bead store), ley-line-3278b4 (Merkle DAG
substrate), `src/scope.rs` (ScopeId, rosary-b5da2f)
**Evidence beads:** rosary-05fbe0, rosary-617010, rosary-6e5fc1,
rosary-560953, rosary-75af4d

---

## 1. Problem framing: we address information by where it sits, not what it is

A bead — the unit of work and coordination state across ART — is today *rows
in a per-repo store*. Its address is a chain of mutable location facts: which
filesystem path the repo was reached through, which `.beads/` directory that
path resolves to, which backend (SQLite file vs Dolt server) happens to live
there, which tenant/config registered it. Every link in that chain is mutable,
aliasable, or mechanically fragile, so the chain breaks in every way a chain
can break. All five failures below occurred in real sessions:

| Failure | Bead | Mechanism | Failure class |
| --- | --- | --- | --- |
| Binary store reverted by ordinary git ops | rosary-05fbe0 | committed SQLite `beads.db` has no 3-way merge; checkout/stash-pop silently restored an old binary. Hit **twice** in ley-line-open — lost a close + 4 creates | **corrupted** |
| Live Dolt store committed to git | (cloister) | 13 files incl. noms binary chunks (`*.darc`, `journal.idx`, `manifest`), `.dolt/LOCK`, `sql-server.info` (live runtime state), `privileges.db` in git — same class, worse: a *running* database's binary + runtime files under a VCS that can't merge them | **corrupted** (latent) |
| Symlink alias missed the pool | rosary-617010 | repo lookup compared paths as exact strings; `~/github/art/X` vs `~/remotes/art/X` (a real symlink) are the *same repo* but missed the registry → silent empty search | **mis-routed** |
| Aliasing and tenancy are one bug | rosary-6e5fc1 | many-addresses→one-repo (symlink) and one-address→many-repos (tenant) are the same defect: surface address ≠ identity; both need `resolve(address, context) → Identity` before keying | **mis-routed** (general form) |
| Phantom global store | rosary-560953 | beads created outside a repo cwd (or via a mis-resolved path) landed in `~/.beads/beads.db`; 18 orphaned beads (15 real cloister work items + 3 notme) stranded, unfindable from their repos | **lost** |
| Backend chosen with no governing intent | rosary-75af4d | `rsry enable` hardcoded Dolt-per-repo (spawning a dolt-server each); a single-user blog got a database server. Fixed to SQLite-default — but the fix is another location-mechanic default, not a principle | **trapped** |

These are not five bugs. They are one absence surfacing five ways.

### 1.1 The root cause (from the 5-why)

**Storage mechanics are doing the job an identity/role/sharing model should
do.** Because a bead *is* its rows, the store's mechanics — SQLite file
semantics, Dolt server semantics, git's binary-blob semantics, filesystem
path comparison — leak into every decision: where a bead is findable, whether
it survives a `git checkout`, whether two agents can touch it concurrently,
whether a clone sees it. The bead has no first-class:

1. **Identity** — a stable name independent of path, alias, tenant, or backend;
2. **Role** — canonical work-record vs local coordination record;
3. **Sharing semantics** — derived from role, not from whichever store the
   bead happened to land in.

Fix the absence and each failure class becomes structurally impossible rather
than procedurally patched. That is the design bar: **findability and integrity
must be properties of the data model, not disciplines of the operator.**

### 1.2 Invariants any solution must satisfy

- **(a)** canonical identity independent of path / alias / tenant;
- **(b)** role (canonical vs coordination) is explicit;
- **(c)** sharing derives from role, not repo/backend;
- **(d)** no storage mechanic is a git-revertible, unmergeable footgun;
- **(e)** concurrent agents cannot corrupt shared state;
- **(f)** addressing is uniform across local / symlink / cloud / tenant;
- **(g)** integrity is provable — a retrieved bead is verifiably the right,
  uncorrupted one.

---

## 2. The identity model

### 2.1 What already exists (build on, don't reinvent)

Three pieces of this design are already accepted or built elsewhere in the
ecosystem; the design's job is to *connect* them, not invent them:

- **cloister ADR-0003 (Accepted, 2026-04-29)** specifies beads as an
  **immutable content-addressed DAG** (blobs keyed by digest; union over
  reachable digests is associative/commutative/idempotent) plus **mutable
  single-writer refs** updated by compare-and-swap, over a five-primitive
  substrate (`BlobStore.put/get/has` + `RefStore.cas/list`) that maps onto
  workerd DO SQLite, native SQLite, ley-line's content store, or any KV with
  CAS. Its 2026-05-11 amendment made the same `BlobStore` power the OCI
  registry — image layers and bead blobs share one content-addressed monoid.
  "An OCI registry is just a filepath" is literally implemented.
- **rosary ADR-0010 (Accepted; built, shadow-folding)** defines the
  observation lattice: append-only G-set of authenticated observations +
  per-field deterministic fold (chain-max, LWW-register, OR-set,
  flat-lattice). Same-set-of-observations → same derived view, any order.
- **`ScopeId` (`src/scope.rs`)** is the front half of an address resolver:
  `Repo(name)` / `External(uri)` / `Global`, with canonical string forms and
  a `WorkRef` bridge. What it lacks is a *back half* — it normalizes the
  *shape* of an address but resolves `Repo` by basename string, which is
  exactly the aliasing bug (617010/6e5fc1) one level up.

Key observation (mine, but load-bearing): **cloister ADR-0003's per-field
merge lattices and rosary ADR-0010's field algebras are the same mathematics
discovered twice** — set-union tags / max priority / LWW description /
MV-register-vs-flat-lattice state / append-dedup comments. Two accepted ADRs
in two repos independently derived "structured record whose fields merge by
per-field join." That convergence is strong evidence the shape is right, and
a mandate to unify rather than maintain both.

### 2.2 A bead's identity is three layers

```
┌───────────────────────────────────────────────────────────────────┐
│  Layer 3: ALIASES        rosary-7f83c3, Linear ROS-142, a path…   │  many, mutable, human
│           resolve(alias, context) → BeadId                        │
├───────────────────────────────────────────────────────────────────┤
│  Layer 2: TIP            digest of current head commit blob       │  moves via ref CAS
│           refs/beads/<BeadId> → tip digest                        │
├───────────────────────────────────────────────────────────────────┤
│  Layer 1: BeadId         digest of the GENESIS blob               │  one, immutable, global
│           bead:sha256:<hex>                                       │
└───────────────────────────────────────────────────────────────────┘
```

**Layer 1 — BeadId = the digest of the genesis blob.** The genesis blob is a
deterministic canonical serialization (sorted-key JSON, UTF-8, LF — cloister
ADR-0003's canonical form) of the bead's *creation event*:

```jsonc
{
  "schema":   "bead-genesis/v1",
  "role":     "canonical",            // or "coordination" | "personal" — §3
  "home":     "repo:rosary",          // ScopeId canonical form; a claim, not an address
  "title":    "…",
  "created":  "2026-07-20T…",
  "creator":  "<signet cert fingerprint>",
  "entropy":  "<128-bit nonce>"
}
```

Honesty about the naming: this is **event-addressing, not pure state
content-addressing**. Two beads with identical titles created in the same
instant must be distinct work items, so genesis carries a nonce and the
creator's signet identity. The BeadId is the content-address *of the creation
event*, and it is stable for the bead's whole life precisely because the
genesis blob never changes. (Pure state-addressing — id = hash of current
fields — is unusable as a name: the id would change on every edit. Git made
the same choice: a commit id addresses an immutable object; a *branch* names a
line of work.) This is my design inference; the given facts don't dictate it,
but every workable CAS system with mutable logical objects (git, OCI
tags→manifests, IPNS→IPFS) lands on this exact two-layer split, and cloister
ADR-0003 already has both layers.

**Layer 2 — the tip.** The bead's current state is the head of its commit DAG
(cloister ADR-0003 Layer 1). `refs/beads/<BeadId>` maps the stable name to
the current tip digest via CAS. Retrieval is provably intact: fetch blob,
recompute digest, compare — invariant (g) is structural. History is
first-class (the DAG), and "the snapshot agent X saw when it branched" is an
LCA query, not archaeology.

**Layer 3 — aliases.** `rosary-7f83c3`-style ids, Linear issue keys, file
paths, tenant-scoped names are all *petnames*: entries in an alias namespace
(`refs/aliases/<name> → BeadId`), created freely, resolvable everywhere, never
used as storage keys. The existing `{repo}-{6hex}` ids survive unchanged as
the default human alias — nothing user-facing has to change.

### 2.3 The resolver: `resolve(address, context) → BeadId`

One function, one seam, all addressing flows through it. This is
rosary-6e5fc1's prescription made total:

```
resolve(surface_address, context):
  1. classify   — bead:sha256:… (already Layer 1: done)
                | alias (rosary-7f83c3, ROS-142, "that auth bug")
                | scope+alias (repo:rosary / external:… / global — ScopeId)
                | filesystem path
  2. canonicalize — paths: realpath() (symlink resolution; the 617010 fix,
                    now mandatory at the only entry point instead of
                    best-effort at N call sites)
  3. scope       — path → RepoId (see below); tenant context → namespace
  4. lookup      — refs/aliases/<namespace>/<name> → BeadId
  5. verify      — optional: fetch tip, check digest + signet cert chain
```

**RepoId must itself be identity, not path.** A repo's stable identity is the
digest of its git root commit (`git rev-list --max-parents=0 HEAD`) — the one
address every clone, symlink, worktree, and tenant mount of the same repo
agrees on, with the git remote URL set as a fallback alias. (Inference: the
given facts establish only that path-keying fails; root-commit identity is my
proposal. Caveat in §8: history rewrites change it.) With RepoId in place,
symlink aliasing and multi-tenancy collapse into the alias layer where 6e5fc1
says they belong: many surface addresses, one identity; one surface address
resolved per-tenant-context, many identities.

**Phantom stores become impossible to lose.** Under this model
`~/.beads/beads.db` is just *another materialization* (§4) of blobs that carry
their own identity and `home` claim. The 18 stranded beads of rosary-560953
were unfindable because the *file* was the identity; when the blob is the
identity, a scan of any materialization finds `home: repo:cloister` beads and
re-homes them mechanically. Misfiled ≠ lost.

---

## 3. Role, and everything that derives from it

### 3.1 Role is explicit, immutable, and set at genesis

`role` lives in the genesis blob, so it is part of the BeadId. Three values:

| Role | Meaning | Grounding |
| --- | --- | --- |
| `canonical` | The project's shared work record. Wants to reach every clone and collaborator. | the default today |
| `coordination` | A branch's / dispatched agent's transient working record. Scoped to that context; **intentionally not shared**. | rosary already has this tier: `.beads/ephemeral.sqlite3` ("wisps/molecules, intentionally not versioned") |
| `personal` | Owner-scoped, possibly encrypted. | ADR-0012 personal/root substrate |

**Roles do not mutate; promotion is derivation.** When coordination work turns
out to matter, you don't flip a bit — you create a *new canonical bead* whose
genesis carries `derived_from: <coordination BeadId>` (BDR provenance already
has the variant vocabulary: `Doc`/`Session`/`Code`/…). This keeps BeadId
stable (role is inside the hash), keeps the audit trail honest, and mirrors
how a scratch branch becomes a PR: you don't rename the scratch branch, you
merge its content into a named line. (Design choice, mine: the alternative —
mutable role field — reintroduces exactly the "sharing changed under you"
class of surprise this doc exists to kill.)

### 3.2 Sharing derives from role (invariant c)

| Role | Replication | Materialization committed to git | Local cache |
| --- | --- | --- | --- |
| `canonical` | yes — via git-native **text** (append-only JSONL event log, union-mergeable) and/or CAS sync ("have digest? no? send it") and/or Dolt **with its own remote** | the JSONL log only — never a binary | `.beads/beads.db` (gitignored, rebuildable) |
| `coordination` | no — pinned to its context (worktree, dispatch, branch) | nothing | `.beads/ephemeral.sqlite3` (gitignored, GC-able) |
| `personal` | owner's devices only | nothing | encrypted blobs (ley-line ChaCha; keys via signet) — selective disclosure = key possession |

This makes the rosary-05fbe0 / cloister-Dolt footgun (invariant d)
**structural**: nothing binary is ever tracked, because binaries are always
*caches* derived from the log/CAS, and a cache that git reverts is rebuilt,
not lost. The storage-reality constraint is honored: git and Dolt are two
distinct VCSs that don't losslessly compose; Dolt cannot ride a plain git
remote. So Dolt is demoted from "a place beads live" to "an optional queryable
materialization with its own remote (DoltHub/S3/self-host) for teams that
want SQL history" — and rosary-75af4d's "wrong backend, no governing intent"
can't recur, because backend choice no longer carries semantic weight. The
governing intent is the role; the backend is a cache policy.

### 3.3 Multi-agent coordination derives too (invariant e)

Today's dispatch already isolates *code* in a git worktree; state isolation
follows the same shape:

1. **Dispatch**: agent gets a worktree + a coordination namespace. Its run
   events, comments, and status observations append to its local
   coordination log (role 2 — never shared, never in git).
2. **During the run**: observations about the *canonical* bead (progress,
   feedback run-events per the feedback contract) are content-addressed
   observation blobs signed with the agent's signet ephemeral cert
   (ADR-0010's `cert` field), staged under `refs/agents/<dispatch_id>` —
   cloister ADR-0003's branch-per-agent, one CAS-able ref row per agent,
   "no coordination beyond their own ref."
3. **Propagation**: on completion/verify, the reconciler *folds* the agent's
   observations into the canonical bead's derived view (ADR-0010 fold) and
   advances `refs/beads/<BeadId>` by CAS. Write-all-blobs-then-CAS-the-ref is
   the single linearization point; a lost CAS race retries against the new
   tip with a per-field lattice merge — no spurious conflicts on tag/comment
   fields, explicit MV-register surfacing on state. Concurrent agents cannot
   corrupt shared state because they never share a mutable cell; they share
   an idempotent blob monoid and race only on a compare-and-swap.

Note what this does to ADR-0016's world: the harness runs host-side, but
every state effect of a dispatch flows through content-addressed, signed
observations — which composes directly with cloister's receipt chain (per-
dispatch attestation) and ADR-0015's capsule lineage. The dispatch record and
the bead state stop being separate bookkeeping.

---

## 4. cloister + ley-line CAS as the base; stores as materializations

**cloister is the base, not a sidecar.** The stack, bottom-up:

```
signet        identity & certs: who created/observed (genesis.creator, observation.cert)
ley-line      crypto & content store: ChaCha selective disclosure; a native BlobStore impl
cloister      BlobStore + RefStore (ADR-0003) — the substrate; also OCI registry tenant,
              receipt chain, harness control plane (ADR-0040/rosary ADR-0016)
─────────────────────────────────────────────────────────────────────────────
rosary        bead semantics ON the substrate: genesis/commit/observation blob
              schemas, per-field fold (ADR-0010 ∪ cloister ADR-0003 lattices),
              reconciler, dispatch, verify, Linear/GitHub as observer peers
─────────────────────────────────────────────────────────────────────────────
materializations (all rebuildable caches, all gitignored except text):
  .beads/beads.db          SQLite query index of canonical beads (fast WHERE state='open')
  .beads/events.jsonl      git-committed text log — the git-native share surface
  .beads/ephemeral.sqlite3 coordination-role cache
  Dolt (own remote)        optional SQL-history materialization for teams
  Linear / GitHub          UI materializations, already peers under ADR-0010
```

Rosary keeps ADR-0014's decision — it owns bead *semantics* and speaks the
bead format — but the *storage* answer to "where does a bead live" becomes:
**in the content-addressed monoid, reachable from a ref; everything else is a
cache.** "Find by what it is, not where it sits" then works uniformly
(invariant f): locally the blob is in a SQLite blob table or ley-line store;
in-cluster it's in cloister's BlobStore (same digests — substrate-equivalence
is a test, per ADR-0003); across machines, sync is containerd-style digest
exchange. A bead created on a laptop, referenced from a phone via cloister,
and materialized into Linear is one identity with three caches.

`connect_bead_store()` (ADR-0014's single entry point) survives as the
materialization manager: it stops *being* the truth and starts *serving* it.

---

## 5. The observation lattice: promote it, and re-home it

ADR-0010 is currently "built + shadow-folding, not yet source of truth" —
`persist_status` (a mutable cell) still wins, with the R4b ratchet
(`check-persist-status-ratchet.sh`, 21 call sites and falling) driving the
flip. This design's position:

1. **Finish R4b — the flip is a prerequisite, not a nice-to-have.** A mutable
   status cell is a location-addressed fact with all the same failure modes
   as a location-addressed bead: it can be reverted, raced, and forked. A
   folded status *cannot be corrupted by losing a write* — losing an
   observation degrades the view monotonically and re-syncing the blob
   restores it. Append-only logs are the one representation that is
   simultaneously git-mergeable (union) and semantically correct (fold).
2. **Events-under-refs: re-home the G-set into the CAS.** Today observations
   persist in orchestrator SQLite (`log_sqlite.rs`) — one more per-machine
   location. Make each `Observation` a content-addressed blob (it already
   carries `payload_hash` and an optional signet cert — it is *almost* a CAS
   object now) and make the persisted log a ref-anchored set. Then:
   G-set union = blob-set union (the monoid's native operation); dedup =
   digest equality; tamper-evidence = recompute; attribution = cert. The
   lattice stops being a rosary-internal table and becomes the ecosystem's
   shared event substrate — which is what "every external system is a peer
   source of observations" (ADR-0010's own framing) was always pointing at.
3. **Unify the two algebra stacks.** ADR-0010's per-field algebras and
   cloister ADR-0003's per-field merge lattices must become one specification
   with two implementations (Rust + TS), pinned by a cross-substrate
   equivalence test (same observations → same digests → same fold on both).
   Divergence here would fork the meaning of "status."

The commit-DAG (cloister ADR-0003) and the observation G-set (ADR-0010) are
complementary, not redundant: the DAG records *authored state transitions*
(edits with parentage, LCA, veto-able merges); the G-set records *witnessed
facts* (webhooks, poll results, agent reports) that fold into derived fields.
A bead's tip commit references the fold-input observation set it was derived
from — making every derived status reproducible from named, hashed inputs.
(This paragraph is design inference; ADR-0010 explicitly disclaims replica
semantics, and ADR-0003 explicitly wants history-as-object — the split above
is what lets both keep their claims.)

---

## 6. Trade-offs, and alternatives rejected

### 6.1 Honest costs of this design

- **Query performance.** `WHERE state='open'` against SQLite rows is trivially
  fast; a CAS needs write-time indexes (ADR-0003 flags this). Mitigation: the
  SQLite materialization *is* the index — we keep today's query path, demoted
  to cache. Cost: index-consistency becomes a test obligation.
- **Garbage collection** becomes real (unreachable blobs, dead coordination
  namespaces). Mark-and-sweep from refs; can be lazy; coordination-role GC
  policy is genuinely new work.
- **Two-phase writes** (blobs, then ref CAS) make every mutation path longer
  than an UPDATE. ADR-0003's own estimate: implementations ~2× longer, "worth
  it for the structural payoff." I concur — the MCP/CLI surface is unchanged.
- **Digest discipline.** Deterministic canonical serialization must be
  bit-identical across Rust and TS forever. This is a hard, testable contract
  — and the substrate-equivalence test makes drift loud, not silent.
- **A nonce in genesis** means BeadId is not derivable from bead *content*
  alone — you cannot ask "does a bead titled X exist?" by hashing. Dedup
  stays a search problem (epic::is_dominated_by), which it already is.

### 6.2 Rejected: UUID + central registry ("just give beads a UUID and index them")

The obvious lighter fix: assign every bead a UUIDv7 at creation, keep stores
as they are, and maintain a global index (`~/.rsry/backend.db` or a hosted
service) mapping UUID → (repo, store, row). Rejected on four grounds:

1. **It solves findability only while the registry is reachable, current, and
   itself uncorrupted** — it *adds* a mutable location instead of removing
   one. The registry is `~/.beads/beads.db` (560953) with better PR.
2. **No integrity (invariant g).** A UUID names a row; it proves nothing
   about the bytes you got back. A digest is simultaneously name *and*
   checksum — retrieval and verification are one operation.
3. **It leaves every store-mechanic footgun in place**: the binary-in-git
   revert (05fbe0) still destroys state, the UUID just lets you name what
   you lost. Merge across clones is still row-level conflict, not per-field
   join — invariants (d) and (e) unmet.
4. **It forfeits the convergence dividend.** cloister ADR-0003 is accepted
   and partially built; the OCI registry already shares the BlobStore. A
   UUID registry builds a parallel, weaker addressing scheme *alongside* a
   content-addressed one the ecosystem already committed to.

### 6.3 Rejected: Dolt everywhere, DoltHub as the share plane

Standardize on Dolt (cell-level merge! branches! history!) with a real Dolt
remote per repo. Rejected: it re-couples bead semantics to one engine's
mechanics (the exact ADR-0014 lesson — bd churned storage 3× in 6 months and
stranded 227 beads); it forces server infrastructure per repo (75af4d's blog
getting a dolt-server); it cannot ride git remotes, so every repo needs a
second remote + credential surface; and cell-level merge is a *weaker* form of
the per-field lattices we get engine-free. Dolt remains valuable exactly where
§3.2 puts it: an opt-in SQL-history materialization with its own remote.

### 6.4 Rejected (partially): pure CRDT everything

Making the whole bead one CRDT document (automerge-style) gets convergence
but loses history-as-object, LCA ("what did the agent see when it branched"),
and the ability to veto a concurrent state change — cloister ADR-0003 already
litigated this ("you cannot get this hybrid from a pure CRDT"), and ADR-0010
deliberately walked back its own "CRDT lattice" framing after math review.
We take CRDT *semantics per field* where they help, explicit DAG history
where the workflow needs it.

---

## 7. Migration path

Phased so every phase pays for itself and nothing waits on cloister timelines
it doesn't need. P0 is largely shipped; P1–P2 are pure-rosary; P3+ engage the
substrate.

**P0 — stop the bleeding (shipped / in flight).**
Untrack binary stores; git-track a JSONL export; gitignore the binary
(05fbe0 fix — apply the same surgery to cloister's committed Dolt store:
untrack `*.darc`, `journal.idx`, `manifest`, `LOCK`, `sql-server.info`,
`privileges.db`). Canonicalize paths at repo lookup (617010). SQLite-default
backend (75af4d). These are necessary but only procedural — they patch
mechanics without adding identity.

**P1 — identity layer (rosary-only, no substrate dependency).**
Define `bead-genesis/v1` canonical serialization + digest. Backfill: compute
a genesis blob for every existing bead from its earliest known state
(created_at, title, creator where known; fresh entropy — flagged
`backfilled: true` since original creation entropy is unrecoverable).
Existing `{repo}-{6hex}` ids become aliases. Extend `resolve()`: unify
ScopeId parsing + `realpath` canonicalization + RepoId (root-commit digest)
into the single resolver seam; delete every other path-keyed lookup.
**Exit test:** the 18 phantom beads (560953) are findable and re-homed by
identity from a scan of the global store.

**P2 — log as truth, stores as caches (rosary-only).**
Per-repo `.beads/events.jsonl`: append-only, one canonical-JSON event per
line, `merge=union` in `.gitattributes`, committed. `beads.db` and
`ephemeral.sqlite3` become rebuildable caches (`rsry rebuild` from log; the
post-merge hook re-materializes). Finish R4b: flip the read path to the fold,
drive `persist_status` call-site count to one fold-driven writer, delete the
ratchet. **Exit test:** `git checkout`/`stash pop`/revert against a repo with
concurrent bead writes loses zero state (rebuild + union-merge round-trip).

**P3 — CAS substrate.**
Implement rosary-side `BlobStore`/`RefStore` (ADR-0003's five primitives):
local SQLite blob table and/or ley-line content store; cloister DO in-cluster.
Blobs for genesis/commits/observations; write-blobs-then-CAS-ref discipline;
substrate-equivalence test (same bead → same digest, Rust vs workerd).
Digest-exchange sync replaces "which file is newer."

**P4 — roles + coordination tier formalized.**
`role` in genesis; dispatch writes to `refs/agents/<dispatch_id>`
coordination namespaces; promotion-as-derivation (`derived_from`); GC policy
for expired coordination namespaces. Ephemeral tier keyed by identity, not
path.

**P5 — derived surfaces + tenancy.**
Linear/GitHub complete their ADR-0010 promotion to observer peers writing
observation blobs. Tenancy = ref namespaces (`refs/tenants/<t>/aliases/…`) —
one address, many identities, resolved by context, per 6e5fc1. Dolt
materialization (own remote) offered as opt-in.

---

## 8. Open questions

1. **Digest algorithm.** cloister BlobStore/OCI is SHA-256; rosary uses
   BLAKE3 internally (`payload_hash`, skills). One algorithm must own the
   addressing layer. Lean SHA-256 for OCI/registry compatibility, BLAKE3
   confined to non-addressing interior hashes — but this deserves an ADR.
2. **RepoId under history rewrite.** Root-commit digest changes on rebase of
   the root / filter-repo. Is (root-commit ∪ remote-URL aliases) enough, or
   does RepoId need its own genesis blob (a `.beads/identity` file)?
3. **Ref-store consensus off-cluster.** On a single machine, SQLite txn; in
   cloister, the DO. Two laptops offline-editing the same canonical bead race
   the ref on sync — per-field lattice merge covers folds, but authored-DAG
   forks need the MV-register surfacing UX ADR-0003 left out of scope.
4. **Coordination-tier GC policy.** When is a dispatched agent's namespace
   collectable — on verify-pass? deadletter? TTL? Interacts with ADR-0015
   capsules (which want lineage kept).
5. **Backfill fidelity.** Are backfilled genesis blobs (fresh entropy)
   acceptable as permanent identities, or do we need a
   `bead-genesis/v1-backfill` schema so provenance-sensitive consumers can
   tell?
6. **Linear id round-tripping.** Linear issue keys as aliases are easy;
   Linear *webhook* observations asserting state need cert/attribution
   mapping (ADR-0010's `cert: None` inbound path) before they can be
   first-class fold inputs.
7. **The `home` claim vs reality.** Genesis declares `home: repo:X`; a bead
   can be *materialized* elsewhere. Does `rsry bead move` rewrite the claim
   via a commit (mutable field under LWW), or is home immutable with a
   `residence` field folded from observations? (Current `bead move` tombstones
   the source — the identity model makes the tombstone unnecessary, since the
   BeadId doesn't change when residence does.)
8. **Query-index consistency budget.** How stale may `beads.db` be relative
   to the log/CAS before `rsry_list_beads` must force a rebuild? Needs a
   freshness marker (log HEAD digest stamped into the cache).

---

## 9. Summary of the argument

Every observed failure — corrupted (05fbe0, cloister-Dolt-in-git), mis-routed
(617010, 6e5fc1), lost (560953), trapped/mis-provisioned (75af4d) — is a
consequence of using a mutable location as a name. The ecosystem already
contains the correct primitives, accepted and partially built: a content-
addressed blob monoid + CAS refs (cloister ADR-0003, shared with the OCI
registry), an order-invariant observation fold (rosary ADR-0010), an address
classifier (ScopeId), identity certs (signet), and an encrypting content
store (ley-line). This design's contribution is the connection: **BeadId =
genesis digest; state = ref-addressed DAG tip; facts = content-addressed
signed observations; role is declared at genesis; sharing, storage backend,
git-visibility, and multi-agent coordination are all derived
materializations.** Findability and integrity stop being operational
disciplines and become what the data structure is.
