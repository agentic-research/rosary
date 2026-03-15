# Conductor

OTP supervision layer for rosary's agent dispatch pipeline.

Conductor is the control plane — it connects to rosary (the data plane) over HTTP/MCP, supervises agent processes via OTP DynamicSupervisor, and manages pipeline progression through agent phases.

## Why Elixir?

Rosary's Rust reconciler hand-rolls process supervision: polling `try_wait()`, manual timeout detection, `recover_stuck_beads()` on crash, lost continuations during phase advancement. These are solved problems in OTP. Conductor replaces ~500 lines of buggy lifecycle code with BEAM primitives:

- **`:DOWN` messages** replace `check_completed()` polling
- **`Process.send_after`** replaces elapsed-time timeout checks
- **GenServer state** replaces in-memory HashMaps lost on crash
- **DynamicSupervisor** replaces manual process registry

## Quick Start

```bash
# Requires rsry serve running on :8383
rsry serve --transport http --port 8383 &

# Start conductor
cd conductor
mix deps.get
mix run --no-halt

# Or in IEx
iex -S mix
Conductor.status()
Conductor.dispatch("rsry-abc123", "/path/to/repo", issue_type: "bug")
Conductor.agents()
```

## Architecture

```
Conductor.Application (supervisor)
  +-- RsryClient      (HTTP/MCP session to rsry)
  +-- AgentSupervisor  (DynamicSupervisor)
  |     +-- AgentWorker (bead-1)  ← Port-monitored OS process
  |     +-- AgentWorker (bead-2)
  +-- Orchestrator     (periodic poll/triage/dispatch)
```

## Pipelines

Pipelines are first-class data structures — not index arithmetic. They're serializable, inspectable, and modifiable at runtime:

```elixir
# Build from issue type
p = Pipeline.for_bead("rsry-abc", "/repo", "bug")
#=> steps: [dev-agent, staging-agent]

# Navigate
Pipeline.current_agent(p)  #=> "dev-agent"
Pipeline.advance(p)        #=> {:next, %Pipeline{current: 1}}

# Mutate at runtime (PM agent inserts a review step)
Pipeline.insert_step(p, 1, %{agent: "review-agent"})

# Serialize for Dolt persistence
Pipeline.to_map(p) |> Jason.encode!()
```

## Configuration

```elixir
# config/config.exs
config :conductor,
  rsry_url: "http://127.0.0.1:8383/mcp",
  scan_interval_ms: 30_000,
  agent_timeout_ms: 600_000,
  max_concurrent: 3
```

## Design

See [ADR-002](ADR-002-otp-conductor.md) for the full design rationale, Symphony comparison, and migration plan.
