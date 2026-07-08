# The semgrep gate

Rosary runs a small set of **custom** semgrep rules as part of `task lint`
(hence `task check`, `task install`, and CI). This documents why it runs where
it does, what the rules catch, and how to add one.

## Why `task install` runs semgrep

`task install` depends on `[release, lint]` — it builds the binary **and** runs
the full lint gate (clippy + semgrep) before placing `rsry` in your PATH and
restarting your live MCP service on it. The intent: don't install (and run as a
service) a binary built from code that wouldn't pass CI.

One wrinkle worth knowing: Taskfile only caches task results **within a single
invocation**. So `task check` followed by a *separate* `task install` re-runs
clippy+semgrep even though check already passed. For a standalone `task install`
the gate is valuable; right after a green `task check` it's redundant work. If
that ever annoys you, `install` could depend on `release` only and leave lint to
`check` — a deliberate trade, not a bug.

## What the gate runs

`task lint` runs three semgrep steps, all via wrappers that **degrade cleanly**
when semgrep is installed but unusable in a sandbox (an X509 trust-init failure
is harness friction, not a code defect — it skips+warns; real findings still
fail):

1. `semgrep --validate` — the rules are well-formed (no duplicate patterns /
   missing fields). Via `scripts/check-semgrep-rules.sh`.
2. `semgrep --test` — the fixtures in `.semgrep/rules.rs` still match (a rule
   that silently stops matching is caught here). Same script.
3. `semgrep --config .semgrep/rules.yml .` — the actual scan. Via
   `scripts/run-semgrep.sh`.

## The rules (`.semgrep/rules.yml`)

Targeted, high-signal custom rules — **not** the `r/rust` registry. Like the
mache structural-smell gate, a few precise rules beat a broad noisy import.

| id | severity | catches |
| --- | --- | --- |
| `blocking-subprocess-in-async` | ERROR | `std::process::Command::…{output,status,wait}()` inside an `async fn` — it stalls the executor thread. Use `tokio::process::Command` + `.await`. Caught the real #324→#327 regression. |
| `audit-log-before-status-change` | WARNING | `persist_status(…)` before its `log_event`/`add_comment` — a crash between them leaves an audit gap. Log first, then transition. |

Both carry `metadata` (category, confidence, references, `why`) per the
[rule-syntax docs](https://semgrep.dev/docs/writing-rules/rule-syntax).

### Why no autofix

`blocking-subprocess-in-async` looks like an autofix candidate
(`std::process` → `tokio::process`), and we evaluated it — but deliberately
skipped it. The rule matches multiple builder-chain shapes
(`new().output()`, `new().args(…).output()`, …) and a single `fix:` can't
reproduce all of them safely (the args-form fix would drop `.args(…)`), and the
replacement must also append `.await`. A mis-suggesting autofix across unbounded
builder shapes is worse than the explicit ERROR message + reference. Autofix
fits mechanical *single-token* rules; add it *there* when such a rule arises.

## Adding or changing a rule

1. Edit `.semgrep/rules.yml`. Include `metadata` (`category`, `confidence`,
   `references`, `why`).
2. Add fixtures to `.semgrep/rules.rs` (same stem as the rules file — that's how
   `semgrep --test` pairs them). Mark each case:
   - `// ruleid: <rule-id>` on the line that MUST match (true positive).
   - `// ok: <rule-id>` on a line that must NOT (true negative).
   The fixture is excluded from the main scan via `.semgrepignore` — its
   deliberate violations would otherwise fail the scan.
3. Verify locally: `bash scripts/check-semgrep-rules.sh` (validate + test), then
   `task lint`.

## Versions (a note, since it confuses)

- **semgrep** is `1.x` (we're on 1.159.0). It is never 3.x.
- **`version: '3'` in `Taskfile.yml`/`.taskfiles/*.yml` is the Taskfile *schema*
  major** — current and correct. It is **not** the go-task binary version.
- The **go-task binary** versions independently (we're on 3.49.1; upgrade with
  `brew upgrade go-task`). Nothing in the Taskfiles pins or lags because of it.
