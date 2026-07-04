# The feedback contract (rosary-0908bc)

Status: implemented + proven live (2026-07-04)
Relates: agent-native run/session substrate (#247), rosary-5361f4 (runaway),
rosary-59ff84 (verify poison), rosary-7f7eff (phase-aware fold)

## The problem (from dogfooding)

Dispatched dev-agents kept failing verification and **retrying blind**: the
workspace was preserved, but nothing told the next attempt *why* it failed, so it
repeated the mistake — the nuke-and-restart anti-pattern. And agents left **no
durable record** of what they tried or why they thought it would (not) pass. The
"job" had no closing contract.

## The contract

A dispatched run is a **job**, and a job is not complete until the agent has left
its feedback — recorded through the **agent-native run/session substrate**, not a
bolt-on. Two halves of one loop:

### 1. Closing condition — native + enforceable

The agent must record a `feedback` run-event via `rsry_agent_run_event_record`
(granted to every dispatched profile). At verify time,
`PipelineEngine::dispatch_left_feedback` checks the backend for a `feedback`
event recorded **at or after this run's start** — the start is parsed from the
`{bead_id}-{started_millis}` dispatch_id, so a *prior* attempt's feedback can't
satisfy the current run. If a run would otherwise pass (`Advance`/`Terminal`) but
has no such event, the verify path **downgrades the action to `Retry`**.

Fail-open: with no backend store configured there is nothing to enforce against,
so the run proceeds.

### 2. Fix-forward — resumable

On any verification failure, `on_fail` writes `.rsry-retry.md` (the failed tier)
into the preserved workspace. `build_prompt` reads it and renders a
`<previous_attempt>` section, so the retry **iterates** — it sees exactly why it
failed (or that it merely forgot the feedback event) and addresses that, rather
than starting over. The two halves interlock: a pass-but-no-feedback retry just
*adds the feedback*; it does not redo the work.

## Why native (not a workspace file or a bead comment)

Bead comments are for humans; the feedback contract is machine-checked. Using the
run-event substrate makes the feedback queryable by dispatch (`agent_run_events`),
foldable into the observation lattice, and identity-addressable per run — the
same coordinate the rest of the agent-native design uses.

## Proof (first successful pipeline completion)

The first dogfood run after the contract landed (rosary-765f42, gemini credential
injection — a bead that had failed every prior attempt):

- `[verify] rosary-765f42: PASS (highest_tier=Some(9))` — first full pass across
  all tiers (compile → test → review → close-condition), possible only after the
  verify `test` tier was un-poisoned (rosary-59ff84).
- The agent recorded a substantive native feedback event:
  *"Fixed gemini credential injection: added provider_cred_keys +
  resolve_provider_cred…"* — the contract satisfied, so the gate let it advance
  (the compliant path — no downgrade).
- `[dispatch] rosary-765f42 phase 2 → staging-agent` — the pipeline advanced on a
  genuine pass.

The whole dispatch-quality stack fired together: fair gate + enforced feedback +
phase-aware fold + no runaway.

## Enforcement semantics

- Compliant agent (passes + records feedback) → advances normally.
- Passes verify, forgets feedback → downgraded to retry; the fix-forward note
  tells it to record the event; next attempt satisfies the contract.
- Never records feedback → retries until `max_retries` → clean deadletter
  (rosary-5361f4 ensures the targeted run then exits rather than looping).
