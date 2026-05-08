# Warp OSS — Learnings for Rosary

**Date**: 2026-05-08
**Source**: [github.com/warpdotdev/warp](https://github.com/warpdotdev/warp) — ~698 MB, 63 crates, 159 spec dirs at the snapshot time
**Status**: Snapshot research. Numbers and code refs valid as of May 2026.

## TL;DR

Warp open-sourced its terminal client under AGPL-3.0 + a small MIT carve-out for the UI framework (`warpui_core`, `warpui`). OpenAI is the founding sponsor. The paid product is **Oz** — the agentic management bot that triages issues, writes specs, opens PRs, and reviews PRs in this very repo. The repo *is* the demo: contributors watch Oz work alongside humans on `build.warp.dev`.

Rosary is doing the same shape of thing — agent orchestration over multi-repo work — and is structurally **deeper** in some places (provenance, cross-repo deps, Dolt-versioned beads) but **simpler than it should be** in two specific places that Warp gets right:

1. **Spec PR before code PR** for feature work, with a clean two-doc gate (`PRODUCT.md` + `TECH.md`)
2. **Explicit user-approval gate** on agent runs (`OrchestrationConfigStatus: None | Approved | Disapproved`) before auto-launch

## Architecture map

### Crate layout (the relevant subset)

| Warp crate | Rough rosary analogue | Note |
|---|---|---|
| `ai/agent/orchestration_config.rs` | `src/dispatch/`, `src/config.rs` | Warp models `RunAgentsRequest` with `harness_type` + `execution_mode: Local | Remote{environment_id, worker_host}` per-run, not per-config |
| `ai/skills/` (skill_provider, parser) | `agents/*.md` | Warp parses skill files at runtime; rosary loads agent .md statically |
| `computer_use` | `src/dispatch/session.rs` ACP path | Tool-use abstraction layer |
| `isolation_platform` | `src/workspace/` worktrees | Warp's sandbox; rosary uses git/jj worktree isolation |
| `ipc`, `jsonrpc` | `src/serve/mod.rs` | Wire-protocol layer |
| `persistence` | `src/store_*.rs` | Their state store |
| `command-signatures-v2` | (none — closest is `crates/bdr/`) | Excluded from `cargo nextest --workspace`. A separate codegen surface that doesn't run with normal CI. Lesson: rosary could quarantine generated/derived code the same way |

63 crates total. They split aggressively — `field_mask`, `fuzzy_match`, `string-offset`, `markdown_parser`, `natural_language_detection`, etc. are each their own crate.

### Spec PR flow

```
specs/APP-NNNN/
  PRODUCT.md   — user-facing problem, behavior, non-goals, mocks
  TECH.md      — file paths + line numbers + diff sketch + interactions
```

98 of the 159 spec dirs follow `APP-NNNN` or `GH-NNNN` numbering. The others are personal scratch dirs (`Advait-M/`, `alokedesai/`, `andy/`).

The TECH.md spec I read (`APP-1915`) is **strikingly precise** — concrete file paths, line ranges (`view.rs (14712-14803)`), exact callers to update, code snippets of the proposed change. This is what rosary's `tech_spec` bead field aspires to but rarely contains.

### Readiness labels

| Label | Means |
|---|---|
| `ready-to-spec` | Problem understood, design open. **Reserved for feature requests.** Next step: open a spec PR. |
| `ready-to-implement` | Design settled, OR triaged bug. Next step: open a code PR. |
| `needs-mocks` | Wait for design mocks. |

Three flat labels replace what rosary models as a dual state machine (ADR-0004). Simpler. Possibly too simple — you can't express "blocked on dep" or "deadlettered" — but for a 50k-star repo it's been enough. **Question for rosary**: are the additional states earning their complexity?

### Review pipeline (Oz)

```
File issue → Warp team triages → readiness label → contributor opens PR
   → Oz auto-reviews → Oz approves → SME (human) auto-assigned → CI → merge
```

Oz is the auto-assignee on every PR. Contributors push fixups, comment `/oz-review` (max 3x per PR) to re-trigger. This is **exactly** the pattern PR #173/#176 did this session with Copilot — except Oz is the product, not a generic bot.

### `/feedback` command

Warp users can file an issue from inside the running terminal — the command attaches logs + env automatically. Rosary's `rsry bead create` is text-only; users have to paste context themselves.

## What Warp does that rosary doesn't (and could)

### 1. Spec PR is a hard gate

Rosary has bead descriptions. A long bead description ≠ a structured PRODUCT/TECH split, and the description doesn't get reviewed independently of the code.

Concrete proposal: a new `bead.spec_status` field with values `none | needs-spec | spec-in-review | ready-to-implement`, and a `rsry spec` command that opens a PR adding `specs/<bead-id>/PRODUCT.md + TECH.md` against the target repo. Bead can't dispatch to dev-agent until `spec_status = ready-to-implement`. This formalizes Golden Rule 12 (5-whys) into a reviewable artifact.

Already-existing bead: `rosary-d59626` ("Scoping agent — LLM pre-dispatch validation, 5 whys as pipeline phase -1") covers half of this. Spec PR is the *output* of that scoping phase.

### 2. Explicit approval gate before auto-launch

```rust
pub enum OrchestrationConfigStatus { None, Approved, Disapproved }
```

The user has to approve the (model_id, harness_type, execution_mode) triple. After approval, future `run_agents` calls with matching fields auto-launch; mismatched calls show a confirmation card.

Rosary today: bead enters queue → triage scores → reconciler dispatches if score ≥ threshold. There's no human approval primitive. Adding `bead.dispatch_approval: None | Approved | Rejected` (default None for new beads, auto-Approved for self-managed repos) would let users gate dispatch on a per-(repo, agent) basis without changing the queue.

### 3. Per-run execution_mode

```rust
RunAgentsExecutionMode::Local
RunAgentsExecutionMode::Remote { environment_id, worker_host }
```

Rosary's compute provider is swapped at config time (sprites vs local vs Fly). Warp lets a single conversation pin where each agent run executes. **Lesson**: lift compute_provider into `RunSpec` / `DispatchRecord`, default from config.

### 4. Specs-in-tree numbering

`specs/APP-NNNN/` mirrors GitHub issue numbers. Easy to grep. Easy to land a spec without code. Easy for the bot to reference.

Rosary stores specs in bead descriptions (live in Dolt, not in the tree). Tradeoff: Dolt is versioned + cross-repo queryable, but specs aren't grep-able from `find docs/`. Putting a synced copy at `docs/specs/<bead-id>/` could give both.

### 5. `script/presubmit` over `task lint`

Warp uses a single `./script/presubmit` that runs fmt + clippy + tests as one command. Rosary uses Taskfile with `task lint` (clippy) + `task test` (cargo nextest) + pre-commit hooks. Roughly equivalent — Warp's is simpler to remember; rosary's is more granular.

## What rosary does that Warp doesn't

### 1. Cross-repo work tracking

Warp is a single repo. Rosary's `[[repo]]` config + cross-repo deps (ADR-0009 stratified acyclicity + modal evidence) is genuinely novel — Warp has nothing like it. If rosary opens up its multi-repo orchestration as a primitive, that's a positioning advantage Warp doesn't compete on.

### 2. DSSE/in-toto handoff signatures (APAS L2)

Warp has **no** signed attestation on agent output. Their PR review is "Oz says LGTM" + "human says LGTM" + CI. Rosary's APAS L2 (DSSE-Ed25519-signed handoffs, in-toto Statement payload) is a more rigorous substrate. If/when supply-chain auditability matters, rosary already has it.

### 3. Versioned bead store (Dolt)

Warp's `persistence` crate is conventional state. Rosary's per-repo `.beads/` is a versioned SQL DB. Reverting bad bead writes, branching state per agent run, federating bead changes across repos — all enabled by Dolt, none of which Warp can do.

### 4. BDR lattice (Decade → Thread → Bead)

Warp has issues. That's it. No higher-level grouping primitive. Rosary's BDR (decompose ADR markdown into atoms → beads grouped into threads under decades) is structurally richer for org-wide planning across many small units of work.

## Specific things to copy

| Item | Effort | Where |
|---|---|---|
| Add `specs/<bead-id>/{PRODUCT,TECH}.md` convention to rosary self-managed repo | small | `docs/specs/` directory + bead.spec_path field |
| Add `OrchestrationApproval { None, Approved, Rejected }` field on dispatch config | small | `src/config.rs` `[dispatch]` block, opt-in default Approved for self-managed |
| Add per-run `execution_mode` override to `DispatchRecord` | small | `src/store.rs` `pub struct DispatchRecord` already exists; add field |
| Extract a `presubmit` task from Taskfile that wraps `task fmt && task lint && task test` | trivial | `Taskfile.yml` |
| `rsry feedback` command that auto-attaches `~/.rsry/rsry-serve.log` tail to a new bead | small | `src/main.rs` + `src/cli.rs` |
| Add `ready-to-spec` / `ready-to-implement` Linear-label-equivalent to bead status | medium | `src/bead.rs` BeadState — possibly via a new `phase` enum separate from the lifecycle state |

## Specific things to NOT copy

- Their crate explosion (63 crates) is appropriate for their UI/terminal complexity, not for rosary's CLI/server scope. Don't aspire to it.
- Their slack-only contributor channel. Rosary should keep bead comments + GitHub PRs as the canonical record; Slack is a private side-channel.
- Their dependency on a hosted Oz product. Rosary's "all roles in one binary, all open source" positioning is already a clearer story.

## Open questions

- **Build.warp.dev dashboard**: a public live view of agent activity. Could rosary do a static-site equivalent fed by `~/.rsry/backend.db` exports? Would dovetail with the iOS control-room app.
- **specs/ vs beads/**: should rosary spec docs live in-tree (greppable, PR-reviewable) or in Dolt (versioned, cross-repo)? Sync model?
- **Approval gate**: per-bead, per-(repo, agent), or per-run? Warp does per-OrchestrationConfig (model+harness+mode triple). Rosary's grain isn't obvious.

## References

All paths below are pinned to commit `ef00af00bf033a4d355aca5b145841e107fd2b7c` so they remain stable as upstream evolves.

- [github.com/warpdotdev/warp](https://github.com/warpdotdev/warp) — main repo
- [warp.dev/blog/warp-is-now-open-source](https://www.warp.dev/blog/warp-is-now-open-source) — announcement
- [CONTRIBUTING.md](https://github.com/warpdotdev/warp/blob/ef00af00bf033a4d355aca5b145841e107fd2b7c/CONTRIBUTING.md) — contribution flow
- [specs/APP-1915/PRODUCT.md](https://github.com/warpdotdev/warp/blob/ef00af00bf033a4d355aca5b145841e107fd2b7c/specs/APP-1915/PRODUCT.md) and [TECH.md](https://github.com/warpdotdev/warp/blob/ef00af00bf033a4d355aca5b145841e107fd2b7c/specs/APP-1915/TECH.md) — sample spec pair
- [crates/ai/src/agent/orchestration_config.rs](https://github.com/warpdotdev/warp/blob/ef00af00bf033a4d355aca5b145841e107fd2b7c/crates/ai/src/agent/orchestration_config.rs) — approval state machine
