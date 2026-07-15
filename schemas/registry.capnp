@0xfcd1eacb3664858a;

# registry.capnp — the annotated tool registry (ADR-0006 revival, bead
# rosary-08a278). Each `$Traits.op`-annotated struct is one MCP tool; the
# leyline-schema-bridge `tooldefs` plugin lowers this file into the bare
# MCP tools/list array committed at src/serve/tools.generated.json, which
# src/serve/tools.rs splices into tool_definitions(). The drift gate
# (`task registry:check-drift`, wired into `task check`) regenerates and
# byte-diffs against that committed file, so the capnp schema is the source
# of truth for the tools it covers.
#
# INCREMENTAL — covers 6 of the 41 live MCP tools. It carries ONLY the tools
# whose every field name is a single word, because `capnp compile` rejects
# underscores in field names ("declaration names ... must not contain
# underscores") and the tooldefs emitter uses the capnp field name verbatim
# as the JSON property name (no camelCase→snake_case conversion, and it does
# not honor `$Json.name`). Every other rosary tool has snake_case fields
# (repo_path, issue_type, test_files, comment_id, …) that cannot survive the
# capnp frontend today. That field-name gap — plus inline nested objects
# (session_ref), free-form objects (payload: additionalProperties:true),
# array-of-object (bead_import.beads), and numeric bounds (bead_search.limit
# minimum/maximum) — is filed as LLO + rosary follow-ups on rosary-08a278.
# NOTE: LLO's bead_create tooldefs fixture reproduces rsry_bead_create only
# because it pokes raw field-name bytes, bypassing `capnp compile`; it does
# NOT prove bead_create round-trips through the real plugin pipeline.
#
# Only $doc/$optional/$default/$op are applied here — any other trait is a
# loud UnmappedConstruct at generate time. Integer fields are :Int32
# (signed) NOT :UInt* so the emitter does not inject a spurious "minimum":0
# that the hand-written tools.rs schemas omit.

using Traits = import "/_traits.capnp";

struct ScanInput $Traits.op(name = "rsry_scan")
                 $Traits.doc("Scan all configured repos for beads (work items). Returns a JSON array of beads with their status, priority, and metadata.") {
}

struct StatusInput $Traits.op(name = "rsry_status")
                   $Traits.doc("Return aggregated status counts across all repos: open, ready, dispatchable, in_progress, and blocked bead counts.") {
}

struct ActiveInput $Traits.op(name = "rsry_active")
                   $Traits.doc("Show the merged active view: live session-registry entries plus backend active dispatch and pipeline rows.") {
}

struct RepoListInput $Traits.op(name = "rsry_repo_list")
                     $Traits.doc("List repos registered by the current user.") {
}

struct ExpandRefInput $Traits.op(name = "rsry_expand_ref")
                      $Traits.doc("Fetch a demoted context blob by its content hash (from the bounded pipeline-context envelope). Use when the prompt shows an 'Earlier context' ref you need in full.") {
  hash @0 :Text $Traits.doc("hex content hash of the demoted blob");
}

struct ListBeadsInput $Traits.op(name = "rsry_list_beads")
                      $Traits.doc("List beads with optional filters. Paginated to avoid oversized responses. Returns beads array + total count.") {
  status @0 :Text  $Traits.doc("Filter by status (open, in_progress, blocked, ready, dispatchable, done, etc.). If omitted, returns all beads.") $Traits.optional;
  repo   @1 :Text  $Traits.doc("Filter by repo name (e.g. 'rosary', 'mache'). If omitted, returns beads from all repos.") $Traits.optional;
  limit  @2 :Int32 $Traits.doc("Max beads to return (default 50, max 200).") $Traits.optional $Traits.default("50");
  offset @3 :Int32 $Traits.doc("Skip this many beads before returning results (for pagination).") $Traits.optional $Traits.default("0");
}
