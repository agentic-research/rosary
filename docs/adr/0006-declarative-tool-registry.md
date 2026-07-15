# ADR-006: Declarative Tool Registry — Unified Source for MCP, CLI, Pipeline, and Permissions

## Status

Accepted — partially implemented (2026-07-15, rosary-08a278). See
**Implementation Status** below. The registry format is capnp (not the TOML
sketched under *Decision* — that shape is illustrative only), consuming
ley-line-open's schema-bridge annotation vocabulary + tooldefs emitter.

## Implementation Status (rosary-08a278)

The revival consumes LLO's `leyline-schema-bridge` (PR #232,
ley-line-open-beb8bb): `_traits.capnp` annotations `$doc`/`$optional`/
`$default`/`$op(OpInfo{name,input,…})` + a `capnpc-schema-bridge-tooldefs`
plugin that lowers a `$op`-annotated struct into the MCP `tools/list` entry
`{name, description, inputSchema}` verbatim.

Shipped:

- `schemas/registry.capnp` — the annotated tool schema; `schemas/_traits.capnp`
  vendored from LLO (byte-identical annotation ordinals).
- `src/serve/tools.generated.json` — committed plugin output; `tool_definitions()`
  splices it in (the covered tools' hand-written literals are deleted — the
  capnp registry is their source of truth).
- Drift gate `task registry:check-drift` (in `task check`): regenerate + diff
  against the committed JSON, mirroring cloister's `cluster:zod:check-drift`.

**Incremental — 6 of 41 tools** (`rsry_scan`, `rsry_status`, `rsry_active`,
`rsry_repo_list`, `rsry_expand_ref`, `rsry_list_beads`). Coverage is bounded by
gaps in the current annotation vocabulary, NOT by the wiring:

1. **snake_case field names (the load-bearing blocker).** `capnp compile`
   rejects underscores in field names, and the tooldefs emitter uses the capnp
   field name verbatim as the JSON property name (no camelCase→snake_case
   conversion; it does not honor `$Json.name`). Every other rosary tool has
   snake_case fields (`repo_path`, `issue_type`, `test_files`, `comment_id`, …).
   LLO's `bead_create` tooldefs fixture passes only because it pokes raw
   field-name bytes, bypassing the capnp frontend — it does not prove
   `rsry_bead_create` round-trips through the real plugin pipeline.
2. Inline nested objects (`session_ref`) — struct-typed fields emit a `$ref`,
   and tooldefs has no `$defs` bag.
3. Free-form objects (`payload`: `additionalProperties: true`) — capnp has no
   any-object type.
4. Array-of-object (`rsry_bead_import.beads`).
5. Numeric bounds (`rsry_bead_search.limit`: `minimum`/`maximum`).

Follow-ups (filed on rosary-08a278): an LLO annotation-vocabulary bead for the
five gaps above, and rosary beads to migrate the remaining reproducible tools +
add clap generation (Decision §1 second bullet) and the `agent_pipeline()`
mapping (Decision §2) once the vocabulary lands. `src/bead_ops.rs` cores stay
hand-written; the CLI/registry is a generated skin over them.

## Context

Rosary has three surfaces that expose the same operations through different code paths:

1. **MCP tools** (serve.rs) — 22 `tool_*` handlers with JSON schema, descriptions, and argument parsing
1. **CLI commands** (main.rs/cli.rs) — clap subcommands with separate argument parsing and output formatting
1. **Pipeline mapping** (dispatch.rs) — hardcoded `agent_pipeline()` match arms

These are maintained independently, leading to:

- **Duplication**: `create_bead` has an MCP handler (`tool_bead_create`), a CLI handler (`bead create`), and they parse arguments differently, format output differently, and have subtly different validation
- **Drift**: The CLI supports operations the MCP doesn't expose, and vice versa. The pipeline mapping is a fourth independent hardcoded thing
- **No shared schema**: The Elixir conductor (conductor/) communicates with rsry but the contract is ad-hoc, not schema-driven. Adding a tool means editing serve.rs + main.rs + cli.rs — three places for one capability
- **Permission scoping impossible**: Agent definitions can't reference tool capabilities by name because there's no canonical registry of what operations exist

Mache structural analysis confirms the duplication:

- `list_beads`, `add_bead_to_thread`, `find_thread_for_bead`, `active_dispatches` all have `.from_store_dolt_rs` variants (two implementations)
- `make_bead` is defined in 4 places (queue_rs, scanner_rs, thread_rs, root)
- SQL queries are inlined across dolt.rs and store_dolt.rs with no shared query layer

### The Meta-Tool Vision

Today rosary is an opinionated tool — the opinions (which tools exist, which agents run in which order, what permissions each agent gets) are hardcoded in Rust.

Tomorrow rosary should be a **meta-tool** — the engine is Rust, but the opinions come from declarative configuration that ships as defaults. Users can override the pipeline, add tools, change agent permissions, all without recompiling.

## Decision

Introduce a **declarative tool registry** that is the single source of truth for:

### 1. Tool Definitions

```toml
[tools.bead_create]
description = "Create a new bead (work item) in a repo's Dolt database."
category = "bead"
inputs = [
  { name = "repo_path", type = "string", required = true, description = "Path to repo with .beads/ directory" },
  { name = "title", type = "string", required = true },
  { name = "description", type = "string", default = "" },
  { name = "issue_type", type = "string", default = "task", enum = ["bug", "task", "feature", "review", "epic", "design", "research"] },
  { name = "priority", type = "integer", default = 2, min = 0, max = 3 },
  { name = "files", type = "array", items = "string" },
  { name = "test_files", type = "array", items = "string" },
]
handler = "bead::create"  # maps to a Rust function
```

From this single definition, code generation (build.rs or proc macro) produces:

- MCP tool JSON schema + handler dispatch
- clap CLI subcommand + argument parsing
- TypeScript/Elixir client types
- OpenAPI schema for HTTP transport

### 2. Pipeline Definitions

```toml
[pipelines.bug]
agents = ["dev-agent", "staging-agent"]

[pipelines.feature]
agents = ["dev-agent", "staging-agent", "prod-agent"]

[pipelines.design]
agents = ["architect-agent"]
```

Replaces the hardcoded `agent_pipeline()` match in dispatch.rs. Overridable per-project via `rosary.toml`.

### 3. Agent Permission Scopes

```toml
[agents.dev-agent]
tools = ["bead_comment", "bead_update", "bead_close"]
mache = ["search", "find_definition", "read_file"]

[agents.architect-agent]
tools = ["bead_create", "bead_search", "decompose"]
mache = ["get_overview", "get_communities", "search"]
```

Generates the `mcpServers` and `tools` frontmatter for CC subagent definitions.

### 4. Shared Query Layer

Extract all SQL from dolt.rs and store_dolt.rs into a shared query module. Each query is a named, typed function. Both `DoltClient` (beads) and `DoltBackend` (orchestrator state) use the same query primitives.

### 5. Schema-Driven Contracts

The tool registry generates typed schemas for the Elixir conductor boundary. Instead of ad-hoc JSON parsing, the conductor gets generated Elixir structs that match the Rust types exactly.

## Alternatives Considered

### A. Proc macro on Rust functions

Annotate each handler with `#[rsry_tool(name = "bead_create", ...)]` and derive everything from the annotation. **Rejected**: keeps the source of truth in Rust code, doesn't help with external config override or non-Rust consumers.

### B. OpenAPI-first

Write OpenAPI YAML, generate Rust + Elixir + TS from it. **Rejected**: OpenAPI is HTTP-centric, doesn't naturally express MCP tool semantics or pipeline DAGs. Would be forcing the wrong abstraction.

### C. Keep separate, add tests for drift

Write property tests that assert CLI and MCP produce identical results. **Rejected**: treats the symptom, not the cause. Still three places to edit for one capability.

## Consequences

### Positive

- Adding a tool = editing one TOML section + writing one handler function
- Pipeline changes don't require recompilation (TOML override)
- Agent permission scopes are declarative and auditable
- Elixir conductor gets typed contracts for free
- The tool registry IS the documentation

### Negative

- Upfront investment to migrate 22 existing tools
- Build complexity increases (code generation step)
- TOML parsing adds a runtime dependency for tool metadata
- Need to decide: build-time generation (build.rs) vs runtime parsing

### Risks

- Over-engineering: the TOML schema could become its own DSL that's harder than Rust
- Migration: can't do this incrementally per-tool without both systems running in parallel

## Implementation Plan

### Phase 1: Registry schema + code gen for one tool

- Define the TOML schema for tool definitions
- Implement build.rs or proc macro that generates MCP + CLI from one definition
- Migrate `bead_create` as proof of concept
- Validate: MCP and CLI produce identical behavior

### Phase 2: Migrate remaining tools

- Move all 22 tools to declarative definitions
- Delete the manual MCP schema construction in `tool_definitions()`
- Delete the manual CLI argument parsing

### Phase 3: Pipeline + permissions

- Move `agent_pipeline()` to TOML config
- Generate agent permission scopes from registry
- Wire into CC subagent frontmatter generation

### Phase 4: Elixir contracts

- Generate Elixir structs from tool registry
- Replace ad-hoc JSON parsing in conductor
- Add contract tests (Rust output matches Elixir expectation)

## Validation

- `cargo test` passes with zero manual MCP/CLI definitions remaining
- Adding a new tool requires exactly 1 TOML section + 1 handler function
- `rsry bead create` and `rsry_bead_create` MCP tool produce byte-identical JSON output
- Elixir conductor compiles against generated types with no manual type definitions

## References

- rosary-d954d6: Unify MCP tools, CLI commands, and pipeline config from single source
- rosary-c5266a: Pipeline as DAG config
- rosary-d5910e: Add explicit mcpServers + tools frontmatter to all agent definitions
- ADR-001: Sprint planning protocol (the planning loop this enables)
- mache community analysis: 20 communities, duplicate `.from_store_dolt_rs` variants
