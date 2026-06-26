# Codex / Claude Code Dispatch Parity Matrix

Status: draft
Tracking: rosary-7a1d4a, rosary-cc2d08

## Purpose

Rosary's Codex integration is not just another provider flag. Rosary already
has its own agent definitions, phase pipeline, Golden Rules, skills, handoff
records, and jj/git workspace model. Native Codex support should preserve that
architecture and make Codex usable wherever Claude Code works today.

The target shape is:

- Rosary owns beads, threads, decades, pipeline state, workspace leases,
  handoffs, observations, and dispatch records.
- Provider adapters own how a specific agent runtime is started, monitored,
  resumed, interrupted, and translated into Rosary events.
- Claude Code CLI and Gemini CLI remain compatibility adapters.
- Codex support uses provider-native threads/sessions/worktrees, not a durable
  `codex exec` shell-out boundary.

## Existing Rosary Agent Model

Rosary agents are perspective definitions, not just model names:

| Agent | Pipeline role | Reign | Codex implication |
| --- | --- | --- | --- |
| `scoping-agent` | pre-dispatch enrichment | read-only planning | Codex must support fast, isolated planning turns whose output feeds later prompts. |
| `dev-agent` | implementation | write scoped files | Codex must receive bead context, file scopes, tools, Golden Rules, and commit requirements. |
| `staging-agent` | test validity review | read-only evaluation | Codex must support read-only review phases and event/handoff capture without code edits. |
| `prod-agent` | production readiness review | read-only evaluation | Codex must preserve module-level audit instructions and evidence-backed findings. |
| `feature-agent` | cross-file coherence | review/orchestration | Codex must see enough thread/feature context to reason across related bead work. |
| `architect-agent` | ADR/design/decomposition | planning | Codex must not treat `research`/`design` as automatically write-capable. |
| `pm-agent` | strategic/cross-repo triage | planning/orchestration | Codex must support broader context without hiding provider-specific session state. |
| `skeptic-agent` | adversarial review | read-only | Codex must enforce read-only by policy, not prompt convention. |
| `janitor-agent` | scheduled hygiene | read/comment/file beads | Codex must distinguish bead-management grants from code-write grants. |

## Must-Have Parity

| Capability | Claude Code today | Codex target | Gap / implementation note | Test implication |
| --- | --- | --- | --- | --- |
| Agent definition injection | `build_system_prompt` layers base prompt, Golden Rules, and `agents/*.md`. | Same layered prompt for Codex threads/sessions. | Keep prompt assembly provider-neutral. | Unit test that each provider receives the same system prompt for a given agent. |
| Bead context | `build_prompt` injects bead id, repo, title, description, handoff chain. | Same bead and handoff context in Codex run spec. | Introduce `AgentRunSpec` or equivalent instead of passing raw prompt strings everywhere. | Fake provider captures run spec and asserts bead/handoff fields. |
| Pipeline phases | `default_pipelines` maps issue types to agent sequences. | Codex must work for every phase, not only dev-agent. | Provider should not choose pipeline shape; Rosary does. | Reconciler tests run fake native provider through multi-phase bug/feature pipelines. |
| Workspace isolation | Rosary creates jj workspace or git worktree per dispatch. | Codex thread must attach to Rosary-owned workspace lease or a Codex-managed equivalent recorded by Rosary. | Do not let provider silently create unmanaged workspaces. | Fake session with `pid=None` still records workspace, dispatch id, and phase state. |
| Permission grants | Claude uses `--allowedTools`; ACP answers permission requests. | Codex maps Rosary core grants to sandbox, approval, MCP, and filesystem policy. | Move capability policy out of Claude-specific strings. | Matrix test for ReadOnly, Implement, Plan, and agent-specific overrides. |
| Read-only agents | Some prompts say review/analyze; code maps scoping/staging to read-only, architect/pm to Plan. | Codex must enforce read-only where agent reign says read-only. | `research`/`architect-agent` is not read-only today; prompt alone is insufficient. | Prompt and grant tests prove read-only phases contain no commit/write instructions. |
| MCP context | Base prompt advertises rsry and mache MCP tools. | Codex must receive rsry, mache, and lectio when available. | Lectio may be private/new; run spec should model expected MCP grants and unavailable tools explicitly. | Fake provider receives MCP/tool grant list; docs mention unavailable tool behavior. |
| Handoff chain | `Handoff::read_chain` and `format_for_prompt` feed later phases. | Codex phases consume and emit the same handoff/observation records. | Provider events should normalize into Rosary handoff/observation writes. | Fake-agent pipeline emits final/tool/file events and produces expected handoff. |
| No shell-out for durable Codex | Claude/Gemini shell out; ACP uses protocol over child process. | Codex uses native app-server/client/protocol or embedded session API. | `codex exec` may be an ignored smoke/compat test only. | Test `provider_by_name("codex")` has no CLI command durable path. |
| Visibility and lifecycle | Claude Code background agents can be inspected in Claude tooling. | Codex should expose thread/session ids and worktree paths in dispatch records. | Rosary must record provider-native session/thread refs, not just PID. | Dispatch record test accepts `pid=None` and stores provider session metadata. |

## Should-Have Parity

| Capability | Claude Code today | Codex target | Gap / implementation note | Test implication |
| --- | --- | --- | --- | --- |
| Scoping handoff | Scoping agent comments/plans before dev work. | Codex scoping output should feed subsequent Codex or non-Codex phases. | Make scoping output a structured Rosary artifact, not hidden chat context. | Multi-provider fake pipeline: scoping by one provider, dev by another. |
| Thread/worktree visibility | Claude agent view exposes background sessions. | Codex Desktop threads/worktrees are closest visible unit. | Rosary-native Codex should prefer full Codex threads/worktrees over invisible internal subagents for bead workers. | Integration design test records thread/worktree ref in dispatch history. |
| Resume/kill | `AgentSession` supports `wait`, `try_wait`, `kill`, optional session id capture. | Codex session must support resume/interrupt/kill where provider API allows. | Session trait should not assume OS PID. | Fake native session exercises resume metadata and kill state transitions. |
| Provider-neutral events | ACP can capture tool calls; CLI providers mostly rely on stream logs. | Codex events translate to `started`, `message`, `tool_call`, `file_change`, `final`, `error`, `usage`. | Add normalized `AgentEvent` layer before adding Codex. | Event translation unit tests with fake event stream. |
| Config/docs parity | Provider list appears in code, MCP schema, CLI help, docs. | `codex` appears consistently once supported. | Avoid partial provider registration. | Tests/docs checks for provider lists if available. |

## Could-Have Parity

| Capability | Claude Code today | Codex target | Note |
| --- | --- | --- | --- |
| Claude<->Codex handoff | Mostly implicit through bead comments/handoffs. | Explicit cross-provider handoff design after native Codex works. | Tracked by rosary-7d37bf. |
| Codex app thread creation | Manual app/UI supports parallel worktree threads. | Rosary may eventually create visible Codex threads per bead. | Useful for dogfooding and user inspection. |
| Cloud Codex workers | Not part of current local dispatch path. | Possible later through Codex cloud/app-server surfaces. | Keep out of first implementation unless native local path requires it. |

## Implementation Order

1. Finish seam audit and validate file scopes.
2. Extract a provider-neutral run/session/event/grant contract.
3. Build deterministic fake-agent harness against that contract.
4. Port Claude/ACP through the contract without changing behavior.
5. Add native Codex provider using Codex-native thread/session APIs.
6. Add ignored live Codex dispatch smoke.
7. Dogfood fan-out with one bead per Codex thread/workspace.

## Non-Goals For First Pass

- Do not implement durable Codex support via `codex exec`.
- Do not require Codex and Claude to talk directly to each other before Codex is a first-class provider.
- Do not collapse Rosary's agent pipeline into a single provider prompt.
- Do not treat prompt wording as a permission boundary.
