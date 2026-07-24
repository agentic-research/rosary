# Agent Work Continuity Design

**Status:** Proposed

**Date:** 2026-07-24

**Tracking decade:** `agent-work-continuity`

**Entry gate:** `rosary-04fa73`

## Purpose

Rosary should preserve one coherent unit of agent work across local editing,
provider sessions, verification, publication, interruption, and resume.
Currently those concerns cross three incomplete boundaries:

1. Git, jj, and colocated repositories are manipulated through a mixture of
   Git subprocesses, jj subprocesses, and `jj-lib` through `leyline-vcs`.
2. Rosary records provider-native session addresses and outbound messages, but
   does not deliver an addressed message to a live session.
3. Execution context is split among worktrees, dispatch records, handoffs,
   event logs, and provider transcripts. ADR-0015 defines a durable execution
   capsule, but its relationship to jj working history, published Git commits,
   local models, and external checkpoint systems is not yet closed.

This design joins those boundaries without turning Rosary into a Git hosting
network, model runtime, or transcript platform.

## Decisions

### 1. Rosary owns continuity; lower layers own mechanics

Responsibilities are divided as follows:

| Layer | Responsibility |
| --- | --- |
| Git and jj libraries | Object storage, commits, refs, working-copy commits, merge and rebase mechanics |
| `leyline-vcs` | Repository classification and transactional Git, jj, and colocated operations |
| Rosary | Beads, dispatches, session routing, capsules, verification, and lifecycle policy |
| Provider adapters | Start, send, resume, interrupt, observe, and terminate provider-native sessions |
| External projections | Export or import Rosary context through systems such as Entire |
| Hosting | Authentication, replication, access control, remotes, and regional storage |

Rosary must not implement a Git object database, remote protocol, merge
algorithm, decentralized hosting network, or model inference engine.

### 2. Production VCS operations do not use CLI subprocesses

Production implementations in this decade must not execute `git` or `jj`
commands. Git operations use a Rust library behind `leyline-vcs`; jj operations
use `jj-lib` behind the same boundary.

The abstraction requirement is stronger than replacing `Command::new` call by
call. One `VcsRepository` transaction owns:

1. repository classification;
2. the pre-operation snapshot;
3. the requested mutation;
4. explicit Git/jj synchronization for a colocated repository;
5. post-operation invariant checks; and
6. a typed result describing every changed identity.

The three supported repository kinds are distinct:

- **Git:** Git refs and worktrees are authoritative.
- **jj:** jj changes, commits, bookmarks, and workspaces are authoritative.
- **Colocated:** Git and jj share objects, but neither may be mutated through an
  uncoordinated side channel. Import and export are transaction steps, never an
  accidental consequence of the next apparently read-only command.

An operation fails without destructive cleanup when its postconditions cannot
be proven. Recoverable state remains present for diagnosis and retry.

### 3. A capsule closes over code state and agent context

ADR-0015 remains authoritative for durable execution-lineage capsules. This
design adds the VCS and session bindings needed to make a capsule a complete
continuity unit.

A capsule records:

- bead, thread, dispatch, phase-run, and provider-session identities;
- its parent capsule and generation;
- the workspace lease and base code state;
- the current jj change and commit identities while work is evolving;
- the stable Git commit identity after publication;
- ordered prompts, messages, tool/file events, handoffs, and usage evidence;
- verification results and referenced content-addressed artifacts; and
- continuation, interruption, cancellation, and fold state.

The capsule has two code-state phases:

| Phase | Code identity | Semantics |
| --- | --- | --- |
| Evolving | jj change/commit plus workspace identity | Rewritable local working history; may receive more turns |
| Published | stable Git commit plus optional jj change lineage | Immutable interchange point used by PRs and external projections |

Publishing creates or advances a projection of the capsule; it does not erase
the evolving lineage. Rebase or squash may replace the publication commit, so
the capsule records an explicit supersession edge instead of silently changing
an identifier.

Large transcripts and artifacts remain content-addressed references. Provider
transcripts are inputs to the capsule, not the source of capsule identity.

### 4. Dispatch becomes durable start-or-send routing

`rsry_dispatch` becomes a thin control-plane operation over a durable session
router. The router accepts a `SessionCommand` envelope containing:

- stable command and correlation IDs;
- target bead, capsule, and optional provider-native session address;
- operation: `start`, `send`, `resume`, `interrupt`, `cancel`, or `inspect`;
- message or structured run specification;
- expected capabilities and authority grant;
- delivery deadline and retry policy; and
- reply/event address.

Routing behavior is deterministic:

```text
known live address + send-capable provider -> send
known resumable address                   -> resume, then send
no address + start allowed                -> start, persist address, then send
no usable route                           -> durable undeliverable result
```

Every command moves through `recorded -> claimed -> delivered -> acknowledged`
or a terminal failure state. Stable IDs make retries idempotent. A tool response
that only says `recorded` must not imply delivery.

The current `rsry_agent_session_message_record` is retained as a compatibility
surface during migration, but its event feeds the same router. It no longer
terminates the delivery path.

### 5. Provider sessions expose commands, not only process lifecycle

The provider-neutral session contract grows beyond `wait`, `kill`, and
`session_ref`. A provider declares capabilities and implements the supported
subset of:

- start a session;
- send a turn;
- resume a session;
- stream normalized events;
- interrupt or cancel;
- inspect health;
- retrieve or reference transcript slices; and
- report usage and modified files.

Unsupported capabilities fail before dispatch. Provider adapters translate
native events into Rosary events; they do not choose pipeline policy or capsule
identity.

Codex uses its native app-server protocol. ACP uses its protocol transport.
Claude must move to an agent-native protocol before it satisfies the
no-subprocess production contract.

### 6. Local models are providers, not a new orchestration system

A local model connects behind the same provider contract and durable router.
The first implementation must choose one existing inference surface:

- an in-process Rust inference library;
- a stable local HTTP or Unix-domain-socket service; or
- narrow, versioned FFI.

Spawning a model CLI is excluded. Implementing inference, scheduling, model
formats, or GPU memory management in Rosary is also excluded.

The provider must advertise tool calling, structured output, streaming,
context-window, cancellation, and session-continuity capabilities truthfully.
A model without persistent native sessions can still participate through a
Rosary-owned capsule context projection, but it must not pretend to support
native resume.

### 7. Entire is an optional projection through `entire-agent-rosary`

Entire's external-agent protocol discovers an `entire-agent-*` executable and
invokes stateless subcommands using JSON or raw transcript bytes over
stdin/stdout. `entire-agent-rosary` is therefore an external compatibility
adapter, not part of Rosary's internal dispatch path.

The process direction is deliberate:

```text
Entire CLI -> entire-agent-rosary -> Rosary HTTP/library API
```

Rosary never spawns Entire. The adapter never mutates Git or jj directly. It
maps Entire protocol operations to Rosary's durable service:

- lifecycle hooks map to normalized session and capsule events;
- session discovery maps to `AgentSessionRef` and capsule lookup;
- transcript reads and chunking map to capsule artifacts;
- modified-file and token extraction map to recorded normalized events;
- subagent extraction maps to child phase-runs or child capsules;
- resume formatting maps to a Rosary address or command; and
- optional text generation maps only to a provider that declares that
  capability.

Capabilities must be truthful. Missing Rosary APIs cause a capability to be
omitted rather than emulated through filesystem scraping.

Entire's ephemeral and persistent checkpoint stores are reference designs, not
Rosary authorities. Importing or exporting an Entire Checkpoint creates a
projection edge to a Rosary capsule. It never changes capsule identity.

The adapter defaults to local-only behavior. Transcript publication requires
explicit configuration after redaction and privacy checks.

### 8. RefFolder is an optional single-authority directory projection

Some durable context is naturally navigated as a directory—design documents,
research, generated evidence, or a capsule export—but should not be copied
through every feature branch. A future `RefFolder` may map one configured
working-tree path to one dedicated custom ref:

```text
design/ <-> refs/rosary/folders/<folder-id>
```

The normal code commit carries only a stable folder manifest or pointer. The
directory contents have one authority behind the custom ref. A VCS transaction
materializes that tree into a workspace and captures edits back with
compare-and-swap semantics.

This is not Git submodules, sparse checkout, an ignored shared directory, or
dual tracking. The same file must never be authoritative both on the normal
branch and in the RefFolder. Writable materializations require a lease; a
concurrent ref advance produces an explicit merge/conflict result rather than
last-writer-wins.

The storage representation—Git tree or CAS manifest referenced by a Git
ref—remains a separate design decision. The design must test whether its custom
ref namespace remains isolated from jj bookmark import in colocated repos. It
must also define transport, garbage collection, recovery, and behavior when the
ref is unavailable.

RefFolder is tracked by `rosary-131957`. It is optional and cannot block the
VCS, capsule, routing, local-provider, or Entire-adapter delivery paths.

## Data Flow

### New work

1. An MCP or HTTP caller submits a start-or-send command.
2. Rosary validates the bead, authority grant, desired provider capabilities,
   and workspace policy.
3. `leyline-vcs` opens a typed VCS transaction and creates or reuses the
   workspace lease.
4. Rosary creates or resumes the capsule.
5. The router starts the provider session or sends to its existing address.
6. Provider events append to the capsule and update materialized projections.
7. Verification appends evidence and determines whether publication is
   permitted.
8. `leyline-vcs` publishes the code state and verifies Git/jj invariants.
9. Rosary records the publication identity and fold result.
10. Optional adapters project the capsule to Entire or another external system.

### Message to an active conversation

1. The caller supplies a bead/capsule or provider-native session address.
2. Rosary resolves it to one live or resumable session.
3. The command is durably recorded with an idempotency key.
4. A provider delivery worker claims and sends it through the native protocol.
5. Acknowledgement and subsequent agent events use the same correlation ID.
6. Failure leaves the message inspectable and retryable; it is never reported
   as delivered merely because persistence succeeded.

### Recovery

After restart, Rosary reconstructs pending commands and capsules from durable
state. It verifies the workspace lease through `leyline-vcs`, reconnects or
resumes provider-native sessions where supported, and retries only commands
whose idempotency state permits it. Missing worktrees are rehydrated from the
capsule's base and latest valid code-state checkpoint.

## Failure Semantics

- A VCS transaction that cannot prove its postconditions fails closed and
  preserves recoverable workspace and ref state.
- Automatic jj import/export outside a declared transaction is a contract
  violation.
- A recorded message without provider acknowledgement remains pending.
- Duplicate commands return the existing command result.
- Ambiguous session resolution fails instead of selecting the newest session.
- Provider loss produces a resumable or terminal event according to declared
  capabilities.
- Capsule artifact loss is reported explicitly; metadata must not claim a
  transcript or proof is available when its content address cannot be resolved.
- External adapter failure never blocks Rosary's local capsule or code
  publication.
- Privacy or redaction failure blocks external transcript publication.

## Verification

All VCS behavior is tested in disposable repositories. Tests must never use a
developer's live colocated checkout.

### VCS matrix

Every supported operation is exercised against plain Git, pure jj, and
colocated Git+jj fixtures:

- inspect/status;
- create, reuse, and remove workspace;
- branch/bookmark creation;
- switch and fork;
- checkpoint and resume;
- fetch and publication;
- rebase, merge, and squash;
- cleanup after success, failure, and interruption; and
- concurrent workspaces targeting the same base.

Each fixture captures and compares:

- Git refs, HEAD, index, worktree registrations, and reachability;
- jj operation head, working-copy commit, changes, bookmarks, workspace
  registrations, and reachability;
- filesystem state;
- emitted typed result; and
- absence of forbidden production subprocess calls.

### Router matrix

Tests cover:

- start with no address;
- send to a live address;
- resume then send;
- duplicate and out-of-order commands;
- provider unavailable before and after claim;
- timeout, cancellation, interruption, and late acknowledgement;
- service restart between every state transition; and
- concurrent senders to one session.

The deterministic fake provider remains the primary contract harness. Live
provider tests are opt-in compatibility checks, not the correctness oracle.

### Capsule and adapter matrix

Tests cover evolving and published capsule identity, publication supersession,
multi-session condensation, child runs, artifact loss, privacy blocking,
recovery without a worktree, and stable projection to Entire protocol-v1
golden fixtures. The adapter tests execute the protocol boundary as Entire
would, while Rosary internals remain library/service calls.

## Delivery Plan

The decade remains `proposed` until this design is reviewed. Its threads are
ordered as follows:

1. **Stabilization** (`rosary-04fa73`) — hook repair and v0.10.1 release.
2. **Reference architecture** (`rosary-04fa9e`) — source-level Entire and jj
   seam report.
3. **VCS transactions** (`rosary-e80f2a`) — complete operation matrix and
   `leyline-vcs` ownership design.
4. **Capsules** (`rosary-04fac9`) and **session routing**
   (`rosary-04faf5`) — independent designs after their prerequisites.
5. **Local execution** (`rosary-04fb2b`) and **Entire adapter**
   (`rosary-04fb57`) — provider/projection designs after routing and reference
   seams are stable.
6. **RefFolder** (`rosary-131957`) — optional design after the VCS and capsule
   contracts; it is not on the decade's critical path.
7. **Conformance** (`rosary-04fb84`) — final adversarial matrix across all
   approved contracts.
8. Implementation beads are created only after the relevant design is approved
   and actual source/test file scopes are known.

Research and design may proceed before stabilization. Implementation may not.

## Non-Goals

- Replacing GitHub or adopting EntireDB as Rosary's authority.
- Building decentralized Git, a remote helper, or a checkpoint hosting service.
- Making Entire a mandatory dependency.
- Treating provider transcripts as Rosary's canonical event schema.
- Treating every recorded message as a new agent session.
- Providing native resume for providers that cannot support it.
- Replacing ADR-0015's capsule schema with Entire's checkpoint schema.
- Requiring RefFolder for capsules or storing the same file authoritatively in
  both a normal branch and a folder ref.
- Removing all external process boundaries: Entire intentionally invokes the
  adapter executable. The prohibition is on Rosary implementing durable
  behavior by spawning tool CLIs.

## Acceptance

This design is ready for implementation planning when:

- the responsibility boundaries and no-production-shell-out rule are approved;
- the distinction between a local-model provider and the durable router is
  accepted;
- ADR-0015 remains the capsule authority with the bindings defined here;
- Entire is accepted as an optional projection with the documented process
  direction and privacy default; and
- each implementation phase can be decomposed into beads with verified,
  non-overlapping source and test file scopes.

## References

- [ADR-0015: execution-lineage capsules](../../adr/0015-execution-lineage-capsules.md)
- [ADR-0016: dispatch via cloister](../../adr/0016-dispatch-via-cloister.md)
- [Codex / Claude Code dispatch parity](../../design/codex-claude-dispatch-parity.md)
- [Entire external-agent protocol](https://github.com/entireio/cli/blob/main/docs/architecture/external-agent-protocol.md)
- [Entire sessions and checkpoints](https://github.com/entireio/cli/blob/main/docs/architecture/sessions-and-checkpoints.md)
- [Entire checkpoint interfaces](https://github.com/entireio/cli/blob/main/api/checkpoint/interfaces.go)
- [jj Git compatibility](https://docs.jj-vcs.dev/latest/git-compatibility/)
