# Codex / Rosary Determinism Friction Log

Status: draft
Tracking: rosary-d88cfb
Related: rosary-59fd34, rosary-cf52cf, rosary-d18be8, rosary-d298a3

## Purpose

PR #247 added the first agent-native dispatch contract slice for Codex support.
The implementation was small and tested, but the surrounding workflow exposed a
more important class of risk: several important outcomes depended on the model
remembering and correctly following conventions.

Those conventions should become deterministic Rosary substrate wherever
possible. The working rule is:

> If correctness depends on an LLM remembering a rule, encode the rule as a
> schema, grant, state transition, artifact, test, or dependency edge.

This document records the friction encountered while using Codex with Rosary and
classifies what should become deterministic before Codex fan-out becomes routine.

## Friction Summary

| Incident                                          | What Happened                                                                                                                                                                                          | Classification                 | Deterministic Replacement                                                                                                                                        |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Skill existed but was not discoverable            | `/pr-review-kit` existed at `~/github/jamestexas/agents/skills/pr-review-kit/SKILL.md`, but Codex did not advertise it as an available skill. The orchestrator had to find and pass the file manually. | Discovery nondeterminism       | Register skill roots and agent definitions as explicit Rosary/Codex capabilities; test that named skills resolve before dispatch.                                |
| Expected MCP tool was unavailable                 | The user expected rsry, mache, and lectio. rsry and mache were available; lectio was not exposed in this session.                                                                                      | Tool-grant nondeterminism      | Model expected and available MCP tools separately in `AgentRunSpec`; dispatch should warn or fail according to issue-type policy when required tools are absent. |
| Review scope boundaries sounded optional          | Fresh-eyes review found no blockers but noted Codex provider registration and `AgentSessionRef` persistence were out of scope. In prose, those could be forgotten.                                     | Review-to-backlog loss         | Convert every scoped-out edge case into a bead with owner, dependencies, files, tests, and acceptance criteria.                                                  |
| Same-repo dependency link required legacy path    | `rsry_bead_link(scope="repo:rosary", ...)` failed and required `repo_path`. The workaround succeeded, but the canonical scope API was not enough.                                                      | MCP ergonomics nondeterminism  | Scope-only operations should resolve through the registered repo pool. Add MCP handler tests for scope-only same-repo links.                                     |
| Agents repo could not receive a bead              | `repo:agents` was not loaded in the rsry repo pool, and direct `repo_path` access hit a read-only filesystem error from this workspace.                                                                | Repo-discovery nondeterminism  | Register all orchestration-critical repos in the repo pool, including `agents`, or expose a deterministic cross-repo filing fallback.                            |
| Semgrep failed before scanning                    | `task lint` got through clippy, then Semgrep failed during system X509 trust initialization before evaluating rules. Metrics/version flags did not prevent the failure.                                | Environment nondeterminism     | Make Semgrep execution hermetic or degrade explicitly when the scanner cannot initialize. Preserve clippy and Semgrep as separate verification observations.     |
| `sccache` wrapper failed in sandbox               | Cargo commands initially failed because the inherited `RUSTC_WRAPPER=sccache` could not execute in the sandbox. `RUSTC_WRAPPER=` was required for verification.                                        | Environment nondeterminism     | Record build environment in verification artifacts; provide Rosary task wrappers that normalize known sandbox-sensitive env vars.                                |
| Build hash was stale after amend                  | `rsry --version` reported the pre-amend hash until the package was cleaned and rebuilt.                                                                                                                | Build artifact nondeterminism  | Verification should record both git SHA and binary-reported SHA, and fail if they disagree for release/PR attestations.                                          |
| PR body shell interpolation                       | An inline `gh pr create --body "..."` command interpreted Markdown backticks in the PR body as shell command substitution. The PR was created, but the body needed replacement via `--body-file`.      | Shell transport nondeterminism | Use file-backed PR/comment bodies for generated review and PR artifacts; treat shell strings as unsafe transport for rich Markdown.                              |
| Fresh-eyes review required manual prompt assembly | Codex subagent review worked, but only after the orchestrator manually supplied the kit and agent instructions.                                                                                        | Prompt-convention reliance     | A review dispatch should reference a named review harness and immutable skill digest rather than relying on prompt assembly.                                     |
| PR and GitHub checks were tempting as truth       | GitHub PR state was useful, but review history and verification should not disappear if GitHub threads/checks are edited, resolved, or unavailable.                                                    | External projection risk       | Rosary should store review matrices and verification results as first-class observations; GitHub is a projection.                                                |

## Determinism Classes

### Deterministic Substrate

These are safe foundations for fan-out:

- Beads, dependencies, threads, and decades.
- `AgentRunSpec` fields such as bead id, agent name, work directory, permissions,
  configured MCP servers, and expected MCP tool families.
- Permission profiles and tool grants when enforced outside prompt text.
- Dispatch records, manifests, handoffs, observations, and verification records.
- Taskfile targets when treated as named recipes rather than ad hoc shell text.

### Environment Nondeterminism

These are acceptable only if captured and surfaced:

- Host trust stores and Semgrep initialization.
- Sandbox permissions and `RUSTC_WRAPPER`.
- Docker or capnp availability.
- Git signing configuration inherited from the user's environment.
- Build scripts embedding stale state after amended refs.

Rosary should not pretend these are deterministic. It should record them as
verification inputs and produce explicit degraded or failed observations.

### LLM Rule-Following

These are the dangerous parts:

- "Remember to use `/pr-review-kit`."
- "Treat GitHub as a projection."
- "Do not shell out to durable `codex exec`."
- "Out-of-scope means later, not never."
- "Read-only review agents must not write."
- "Fresh-eyes review must not anchor on the PR body."

Each of these should become a checked fact in a run spec, provider contract,
permission profile, bead dependency, or review artifact.

## Conversion Rules

| LLM Rule                   | Deterministic Form                                          | Test Shape                                                                   |
| -------------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Use this skill             | Skill reference by name + resolved path + content digest    | Dispatch fails if required skill cannot resolve.                             |
| Use these MCP tools        | `required_tools` and `optional_tools` in run spec           | Fake provider captures grants; missing required tool produces a clear error. |
| Keep agent read-only       | Permission profile enforced by provider/sandbox/tool grants | Read-only fake provider cannot receive write-capable grants.                 |
| Do not shell out for Codex | Native provider has no durable `build_command` path         | `provider_by_name("codex")` has no CLI command durable path.                 |
| Preserve review history    | Review matrix stored as Rosary artifact/observation         | Store round trip keeps reviewer, SHA, evidence, verdict.                     |
| PR is a container          | WorkRef + commit SHA + patch/PR metadata in review artifact | GitHub PR state can vanish while Rosary review remains queryable.            |
| Run CI                     | Named recipe, e.g. `task ci`, plus environment capture      | Verification artifact records recipe, exit status, logs, binary SHA.         |
| Follow up later            | Bead with dependency edge and file/test scopes              | Review notes cannot close without linked follow-up beads.                    |

## Required Follow-Up Beads

- `rosary-cf52cf`: Expose symlinked agents repo skills to Codex skill discovery.
- `rosary-d6b6e6`: Persist provider-native session refs in dispatch records.
- `rosary-d6d1bb`: Register Codex provider only after native session contract is durable.
- `rosary-d18be8`: Make PR review history a Rosary-owned artifact, with GitHub as projection.
- `rosary-d298a3`: Invert CI: treat PR as review container and `task ci` as Rosary-owned verification.
- `rosary-d7a98e`: Allow `rsry_bead_link` same-repo dependencies with scope-only repo resolution.
- `rosary-9e5138`: Make `task lint` robust when Semgrep cannot initialize system X509 trust.

## Design Implications

### Agent Identity Is Not CWD

Codex and many code tools naturally treat `cwd` as the source of truth. Rosary
does not. Rosary's source of truth is the work item plus the agent lens:

- `scoping-agent` sees the same files as decomposition material.
- `dev-agent` sees them as implementation surface.
- `staging-agent` sees them as test-truth evidence.
- `prod-agent` sees them as operational risk.
- `architect-agent` sees them as system design.

`AgentRunSpec` should therefore grow around agent identity, phase, grants,
handoff chain, required artifacts, and verification recipes. The directory is a
workspace lease, not the whole task.

### Reviews Should Emit Artifacts

A review that only says "SHIP" in chat is not durable. A review should emit a
Rosary-owned artifact with:

- WorkRef and change-container reference.
- Commit SHA reviewed.
- Reviewer identity and provider/session ref.
- Skill or harness name and digest.
- Findings matrix with evidence.
- Verification commands and outputs.
- Verdict and residual risk.

GitHub comments can mirror that artifact, but they should not be canonical.

### CI Should Emit Observations

GitHub Actions can run a task. Local Codex can run the same task. A future rig can
run it elsewhere. The durable fact is not "GitHub check green"; it is:

- recipe: `task ci`
- commit SHA: exact tree under verification
- environment: relevant tool versions and sandbox data
- result: pass/fail/degraded
- artifacts: logs, binary hash, test summary

Those should fold through ADR-0010's observation substrate. GitHub is one source
of observations, not the owner of truth.

## Acceptance Criteria For This Design

This friction class is addressed when:

1. A Codex worker can request `pr-review-kit` by name and fail deterministically
   if it is missing.
1. A dispatch record can persist a provider-native session reference even when
   `pid=None`.
1. A review artifact survives without GitHub and can be queried from Rosary.
1. `task ci` results can be recorded as verification observations with the
   reviewed commit SHA and environment summary.
1. Every review "out of scope" item either links to a bead or is explicitly
   marked as intentionally discarded.
