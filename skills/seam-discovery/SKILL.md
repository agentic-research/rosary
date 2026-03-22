---
name: seam-discovery
description: >
  Discover and document architecture seams — the interfaces, data flows, and
  integration points between projects or dependencies. Use when exploring how
  codebases connect, onboarding to a new ecosystem, or documenting service
  boundaries. Works on single projects or multi-project ecosystems.
allowed-tools: Read,Glob,Grep,Bash(git *),Bash(find *),Bash(ls *),Agent,mcp__mache__*
argument-hint: <path-to-project> [path-to-project-2 ...]
version: 0.1.0
author: ART Ecosystem
---

# /seam-discovery — Architecture Seam Analysis

Discover how projects connect by analyzing their boundary surfaces, integration points, and failure modes.

## What To Do

Given one or more project paths (`$ARGUMENTS`), produce a structured seam analysis.

### Step 1: Identify Each Project's Boundary Surface

For each project path:

1. **Read CLAUDE.md / README / ARCHITECTURE.md** for declared architecture
1. **Scan for what it EXPOSES**: APIs, CLIs, libraries, protocols, file formats
1. **Scan for what it CONSUMES**: dependencies, services, data sources
1. **Note the WIRE FORMAT** at each boundary: JSON, protobuf, SQLite, UDS, gRPC, HTTP, filesystem

### Step 2: Map Integration Seams

For each pair of connected projects (or project ↔ dependency), document:

| Field              | Description                                                                                                           |
| ------------------ | --------------------------------------------------------------------------------------------------------------------- |
| **Seam name**      | Human-readable label (e.g., "mache → ley-line UDS")                                                                   |
| **Direction**      | Who calls whom, or bidirectional                                                                                      |
| **Protocol**       | UDS socket, CGO FFI, HTTP, CLI subprocess, shared file, etc.                                                          |
| **Discovery**      | How they find each other (env var, well-known path, config)                                                           |
| **Failure mode**   | What happens when the other side is down                                                                              |
| **Classification** | `hard` (won't start), `soft` (degrades gracefully), `build` (compile-time only), `optional` (enhances when available) |
| **Data contract**  | Types, schemas, or formats that cross the boundary                                                                    |
| **Source files**   | Files implementing this seam                                                                                          |

### Step 3: Explore Strategy (in order)

Use this priority order to find seams:

1. **Declared architecture**: CLAUDE.md, README, ARCHITECTURE.md, ADRs
1. **Cross-package imports**: import/require/use crossing module boundaries
1. **Network calls**: http.Get, net.Dial, grpc.Dial, UDS connect, WebSocket
1. **FFI boundaries**: #cgo, extern "C", cbindgen, JNI, ctypes
1. **Subprocess spawning**: exec.Command, Command::new, subprocess.run
1. **Shared file paths**: hardcoded paths, env vars, well-known locations
1. **Feature flags**: conditional compilation gating optional integrations
1. **CI/CD integration**: multi-stage Docker, linking flags, build dependencies

### Step 4: Find Hidden Seams

Non-obvious connections that are easy to miss:

- Shared file formats (both projects read/write the same SQLite schema)
- Convention-based discovery (well-known paths like `~/.mache/default.sock`)
- Transitive dependencies (A → B → C — does A actually depend on C?)
- Feature flags gating integration (`#[cfg(feature = "embed")]`)
- Shared env vars or config files

### Step 5: Output

Produce a markdown document with:

1. **Project summary table**: name, language, purpose, path
1. **Seam table**: all discovered seams with the fields above
1. **Dependency graph**: ASCII art showing connections and their types
1. **Failure analysis**: what breaks when each project is unavailable
1. **Recommendations**: missing error handling, undocumented seams, tight coupling

## Output Format

```markdown
# Architecture Seams: <project(s)>

## Projects
| Project | Language | Purpose |
|---------|----------|---------|
| ... | ... | ... |

## Seams
| Seam | From → To | Protocol | Classification | Failure Mode |
|------|-----------|----------|----------------|--------------|
| ... | ... | ... | ... | ... |

## Dependency Graph
<ASCII art>

## Failure Analysis
- If <project> is down: <what breaks>

## Recommendations
- ...
```

## Examples

```
/seam-discovery ~/remotes/art/mache
→ Analyzes mache's boundaries: MCP server (HTTP/stdio), FUSE/NFS mount,
  ley-line UDS (soft dep), SQLite readers, tree-sitter grammars

/seam-discovery ~/remotes/art/mache ~/remotes/art/ley-line ~/remotes/art/kiln
→ Cross-project analysis: how kiln bundles them (CGO FFI, entrypoint.sh),
  how mache discovers ley-line (LEYLINE_SOCKET, ~/.mache/default.sock),
  build dependencies vs runtime dependencies

/seam-discovery .
→ Analyzes current project
```

## Dispatch via Rosary

```
rsry_bead_create(
  repo_path="/path/to/repo",
  title="Architecture seam discovery: <project>",
  issue_type="research"
)
rsry_dispatch(bead_id, agent="architect-agent")
```
