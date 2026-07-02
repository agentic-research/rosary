# ADR-0012: Personal/root rosary substrate — storage, sync, and tamper-resistance

- **Status:** Accepted (decision); implementation sequenced (see §Sequencing)
- **Bead:** rosary-e4d471 · **Epic:** rosary-792ed6
- **Unblocks:** rosary-e5066e, rosary-e52b24, rosary-e55ec9; cloister-9d19e3 → notme-9da488

## Context

Per-repo `.beads/` has **zero access control**: anyone with repo-clone access reads every bead, and the on-disk Dolt files are mutable — a misbehaving local agent can corrupt the store without crossing any signing boundary (`rosary-792ed6`). Several classes of work need a **personal/root** home that lives *outside any project repo*: customer-escalation notes (PII), personal cross-project TODOs, triage drafts, and the incoming-triage queue.

The substrate must be: (1) **outside any project repo**, (2) **encrypted at rest**, (3) **syncable across rigs** (laptop, Linux rigs), and (4) **tamper-evident** — an agent must not be able to mutate it without the keyholder's consent.

Three axes had to be decided. The guiding constraint: **reuse infrastructure that already exists** in the ecosystem rather than invent a new substrate.

## Decision

### Axis 1 — Storage: local SQLite working copy + age-encrypted content-addressed blobs

`~/.rsry/personal.db` (SQLite) is the **live working store** — same backend pattern as the orchestrator's `~/.rsry/backend.db`, zero new infra, ergonomic to query. The **durable, synced artifact** is a set of **per-bead `age`-encrypted, content-addressed blobs**. ley-line already implements exactly this CAS layer (`docs/decades/2026-merkle-cas-substrate.md`, `docs/design/013-edge-native-arena-sync.md`).

**Rejected:**

- *cloister-hosted only* — makes the personal store's very existence a networked dependency with a load-bearing auth model from day one. We want it usable offline on a single laptop first.
- *ley-line peer-replicated blob only* — sync for free, but harder to introspect and couples the store to the LLO daemon being up everywhere.
- *pure-FS / no durable layer* — per-machine, no sync.

### Axis 2 — Sync: `SyncBackend` trait; **GitRepo backend first**, R2 backend as the shared tier

Sync is an interface, not a single mechanism. The artifact (Axis 1's `.age` blobs) is pushed/pulled through a `SyncBackend`:

- **`GitRepoBackend` (first, default)** — a **private git repo** of the per-bead `.age` blobs. This reuses plumbing rosary already has: bd's `backup: git-push` (in `.beads/config.yaml`) already does JSONL→git today; layering `age` on top is the established **`passage`/`cottage`/SOPS** pattern (git + age, readable per-file diffs, per-bead merge). A *public* repo variant is a documented future option (the blobs are encrypted, so a public repo leaks only metadata cardinality).
- **`R2Backend` (shared tier)** — Cloudflare R2, the *same-flavor* content-addressed store reached over private networking (the cloister Worker → R2 binding, `cloister-9d19e3`). This is the multi-machine/multi-agent shared tier; it implements the same trait, so it composes with — not competes with — the git backend.

**Rejected:**

- *cloister server-side merge only* — single point of failure + the auth story must be solved before anything works.
- *ley-line CRDT peer sync only* — requires the LLO daemon running everywhere.
- *git-crypt (whole-file encryption)* — binary diffs, unmergeable. Per-file `age` (passage-style) keeps diffs readable.

### Axis 3 — Tamper: `age` for encryption, signet + go-platform-signers for consent

Encryption and *consent* are different jobs:

- **Encryption: `age`.** age has replaced GPG for new projects (no keyserver, multi-recipient, seconds to set up). Recipients = the keyholder's age key(s).
- **Consent / tamper-evidence: a signet-signed append-only attestation log**, where every mutation appends an entry **signed by a signet identity backed by `go-platform-signers`** — a `crypto.Signer` that is either **PKCS#11 (YubiKey)** *(portable default — works on Linux rigs and macOS)* or **Touch ID / Secure Enclave** *(macOS convenience)*, selected by build tag. "An agent cannot mutate without keyholder consent" reduces to: **a write requires a fresh signature from the hardware-backed key; an agent without the YubiKey present / Touch ID prompt satisfied cannot produce one.** The gate **fails closed**.

This is the *same signet append-only primitive* ADR-0011's `resolve.rs` ships — one pattern, two uses (decision authority there, mutation consent here).

**Rejected:**

- *local FS perms (700) only* — no defense against a compromised local agent, which is exactly the threat the epic names.
- *TEE / Keychain-only* — strongest locally but macOS-only; won't port to Linux rigs.
- *gpg* — supplanted by `age` (encrypt) + a hardware signer (sign).

## Coherence (why this is a clean ADR, not a moonshot)

Almost nothing is new. It reuses: bd's git-backup, ley-line's CAS + R2 edge-sync, `go-platform-signers` (PKCS#11 + Touch ID), `age`, and the ADR-0011 signet append-only primitive. The ADR is mostly *composition*.

## Consequences — new surface

- **`ScopeId::Personal`** variant (alongside `Repo`/`External`/`Global` in `src/scope.rs`), reserved scope string `personal`. Triage admits it like `Global` (reuse the `rosary-fa8a39` pattern).
- **`src/bead_personal.rs`** — the personal store: SQLite working copy + `age` blob export/import (content-addressed).
- **`SyncBackend` trait** + `GitRepoBackend` (first) + `R2Backend` (shared tier, impl home: `cloister-9d19e3`).
- **Attestation gate** — each write appends a signet-signed entry (`go-platform-signers`-backed); reads verify the chain. An `attestations` append-only table (or sidecar log) in `personal.db`.
- **Schema migration** — a separate `~/.rsry/personal.db` with the existing bead schema + the append-only `attestations` table. No change to per-repo bead schemas.

## Sequencing

1. `ScopeId::Personal` + reserved scope + triage admission → **rosary-e5066e**
1. `src/bead_personal.rs` storage module (SQLite + age blob export) → **rosary-e5066e**
1. `SyncBackend` trait + `GitRepoBackend` (age blobs → private git repo) → **rosary-e52b24**
1. signet attestation gate via `go-platform-signers` (fail-closed mutation consent) → **rosary-e55ec9**
1. `R2Backend` (shared tier, Cloudflare Worker → R2) → **cloister-9d19e3** → **notme-9da488** (credential mint)

## Acceptance criteria

- This ADR lands, status Accepted, one option chosen per axis with rejected-alternative paragraphs (done).
- `ScopeId::Personal` round-trips and is admitted by triage (mirror `triage_admits_global_scope_bead_when_gate_active`).
- **The consent test:** an append to the personal store with no valid signet signature is **rejected** (fail-closed) — provable with a stub signer that refuses, asserting the gate denies the mutation. This is the load-bearing "agent cannot mutate without keyholder consent" guarantee.
- `GitRepoBackend` round-trips `age`-encrypted per-bead blobs to a test git repo (push then pull reconstructs the store).

## Cross-references

- Epic: `rosary-792ed6`; sub-beads: `rosary-e5066e`, `rosary-e52b24`, `rosary-e55ec9`
- Shared tier impl: `cloister-9d19e3`; credential mint: `notme-9da488`
- signet primitive: ADR-0011 (`src/observation/resolve.rs`); `go-platform-signers` (PKCS#11 + Touch ID)
- ley-line: `2026-merkle-cas-substrate`, `013-edge-native-arena-sync`
- Prior art: `pass`/`passage`/`cottage`/SOPS (git + age); `age` as the GPG replacement
