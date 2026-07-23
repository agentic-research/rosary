#!/usr/bin/env bash
# jj-lib adoption cost gate (rosary-efd300).
#
# Settles, with numbers instead of inherited belief, what adding jj-lib would
# actually cost rosary. `src/vcs.rs` today drives jj by subprocess; a prior
# decision (rosary-30374f, recorded in Cargo.toml next to the leyline-core
# dep) rejected `leyline-vcs` because of "the heavy leyline-fs/jj-lib
# subtree" — a premise nobody had measured.
#
# Measures, for TWO candidate integration paths against the real rosary
# dependency graph:
#   1. jj-lib DIRECT   — `jj-lib = { version = "0.42", default-features =
#      false }`, matching how ley-line-open's own vcs crate declares it.
#   2. leyline-vcs      — path dep on ../ley-line-open's vcs crate (which
#      pulls leyline-fs with default-features = false, i.e. no FUSE/NFS/
#      tree-sitter) — the actual thing rosary-30374f rejected.
#
# Across three axes:
#   - transitive dependency count (cargo tree, exact — not noisy)
#   - cold + warm release build time (noisy — see NOTE below)
#   - final `rsry` release binary size
#
# Each probe injects a REAL call into jj-lib (init an actual jj repo) rather
# than an unused declared dependency, so the linker can't dead-code-eliminate
# the thing being measured — see the injected `jj_lib_cost_probe` module.
#
# Isolation: all work happens in scratch git worktrees pinned to a single
# resolved commit, each with its own $CARGO_TARGET_DIR. The committed
# Cargo.toml/Cargo.lock are NEVER touched. `trap cleanup EXIT INT TERM`
# removes the worktrees and scratch dir unconditionally, including on
# Ctrl-C or failure — the real working tree is byte-identical after any run.
#
# NOTE ON NOISE: cold build time is a single sample (a full release rebuild
# of rosary's heavy existing tree — sqlx-mysql, reqwest+vendored-openssl,
# rusqlite bundled — already takes minutes; running repeats for 3
# configurations was not a defensible use of CI/dev time). Treat the cold
# numbers as ballpark, not precise. Warm build time (touch src/main.rs +
# rebuild) IS repeated (WARM_REPEATS, default 3) and reported as min/median,
# because that is the number that matters for the day-to-day edit-compile
# loop and is cheap enough to repeat. Dependency count and binary size are
# exact, not sampled — treat those as reliable.
#
# Usage:
#   scripts/jj-lib-cost-gate.sh              # full run: tree + cold + warm x3 + size, both probes
#   scripts/jj-lib-cost-gate.sh --quick       # tree + size only, no cold/warm build timing
#   scripts/jj-lib-cost-gate.sh --jjlib-only  # skip the leyline-vcs probe entirely
#
# Env overrides:
#   LEYLINE_DIR      default: $ROOT_DIR/../ley-line-open (matches Taskfile's LEY_LINE_DIR convention)
#   WARM_REPEATS     default: 3
#   BUDGET_NEW_DEPS  default: 40   (max NET NEW transitive crates vs baseline; FAILS above this)
#   BUDGET_BIN_MB    default: 15   (max release binary size growth in MiB; FAILS above this)
#   BUDGET_COLD_PCT  default: 40   (max cold build time growth, percent; FAILS above this — advisory only, see NOTE)
set -euo pipefail

MODE_QUICK=0
JJLIB_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --quick) MODE_QUICK=1 ;;
    --jjlib-only) JJLIB_ONLY=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

LEYLINE_DIR="${LEYLINE_DIR:-$ROOT_DIR/../ley-line-open}"
LLVCS_PATH="$LEYLINE_DIR/rs/ll-open/vcs"
WARM_REPEATS="${WARM_REPEATS:-3}"
BUDGET_NEW_DEPS="${BUDGET_NEW_DEPS:-40}"
BUDGET_BIN_MB="${BUDGET_BIN_MB:-15}"
BUDGET_COLD_PCT="${BUDGET_COLD_PCT:-40}"

if [ "$JJLIB_ONLY" -eq 0 ] && [ ! -d "$LLVCS_PATH" ]; then
  echo "SKIP: leyline-vcs probe — no checkout at $LLVCS_PATH (set LEYLINE_DIR or pass --jjlib-only)."
  JJLIB_ONLY=1
fi

# Resolve ONE commit up front. HEAD can move under us mid-run on a shared
# branch (observed in practice — do not re-resolve HEAD per worktree).
BASE_COMMIT="$(git rev-parse HEAD)"
if ! git diff --quiet -- Cargo.toml Cargo.lock 2>/dev/null; then
  echo "NOTE: Cargo.toml/Cargo.lock have uncommitted changes — the gate measures"
  echo "      the last COMMIT ($BASE_COMMIT), not your working tree, by design"
  echo "      (worktrees are pinned to a commit)."
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/rosary-jjcost.XXXXXX")"
# Worktree paths are tracked in a manifest FILE, not a bash array: every
# creation happens inside `new_worktree()`, which callers invoke via `$(...)`
# command substitution (`BASELINE_WT="$(new_worktree baseline)"`) — that
# forks a subshell, so an array mutated inside the function would silently
# vanish when the subshell exits, and `cleanup()` would iterate an empty
# array while `rm -rf "$SCRATCH"` deletes the worktree directories out from
# under git (leaving orphaned "prunable" entries in `git worktree list`,
# observed in practice while developing this script). A file survives the
# subshell.
WORKTREE_MANIFEST="$SCRATCH/.worktrees"
: > "$WORKTREE_MANIFEST"

cleanup() {
  local ec=$?
  set +e
  if [ -f "$WORKTREE_MANIFEST" ]; then
    while IFS= read -r wt; do
      [ -n "$wt" ] && git -C "$ROOT_DIR" worktree remove --force "$wt" >/dev/null 2>&1
    done < "$WORKTREE_MANIFEST"
  fi
  rm -rf "$SCRATCH"
  # Byte-identical guarantee: we never wrote to the real tree, only to
  # worktrees + $SCRATCH, both removed above.
  exit "$ec"
}
trap cleanup EXIT INT TERM

echo "== jj-lib cost gate (rosary-efd300) =="
echo "root:        $ROOT_DIR"
echo "base commit: $BASE_COMMIT"
echo "scratch:     $SCRATCH"
echo "mode:        $([ "$MODE_QUICK" -eq 1 ] && echo quick || echo full) $([ "$JJLIB_ONLY" -eq 1 ] && echo '(jj-lib direct only)' || echo '(jj-lib direct + leyline-vcs)')"
echo

new_worktree() {
  local name="$1"
  local path="$SCRATCH/$name"
  git -C "$ROOT_DIR" worktree add --detach "$path" "$BASE_COMMIT" >/dev/null 2>&1
  echo "$path" >> "$WORKTREE_MANIFEST"
  echo "$path"
}

# Inject the always-linked, env-gated probe: `mod jj_lib_cost_probe;` +
# a short-circuit at the top of main() before Cli::parse(). Never touches
# argument parsing or any other behavior; ROSARY_JJ_COST_PROBE is unset in
# normal operation.
inject_main_hook() {
  local dir="$1"
  python3 - "$dir/src/main.rs" <<'PYEOF'
import sys
p = sys.argv[1]
s = open(p).read()
mod_anchor = "mod cas;\n"
assert mod_anchor in s, "mod cas; anchor not found — main.rs module list changed shape"
s = s.replace(mod_anchor, mod_anchor + "mod jj_lib_cost_probe;\n", 1)
main_anchor = "async fn main() -> Result<()> {\n"
assert main_anchor in s, "async fn main() anchor not found — main.rs signature changed"
inject = (
    main_anchor
    + "    if std::env::var_os(\"ROSARY_JJ_COST_PROBE\").is_some() {\n"
    + "        return jj_lib_cost_probe::run();\n"
    + "    }\n"
)
s = s.replace(main_anchor, inject, 1)
open(p, "w").write(s)
PYEOF
}

add_dep_line() {
  # Insert a new [dependencies] line right after rosary's rusqlite dep line
  # (a stable, always-present anchor).
  local dir="$1" line="$2"
  python3 - "$dir/Cargo.toml" "$line" <<'PYEOF'
import sys
p, line = sys.argv[1], sys.argv[2]
s = open(p).read()
anchor = 'rusqlite = { version = "0.34", features = ["bundled"] }'
assert anchor in s, "rusqlite anchor line not found in Cargo.toml — pin changed"
s = s.replace(anchor, anchor + "\n" + line, 1)
open(p, "w").write(s)
PYEOF
}

bump_rusqlite_line() {
  # leyline-fs's workspace pins rusqlite 0.39.0; rosary pins 0.34. Cargo's
  # `links = "sqlite3"` uniqueness rule means ONE version must win across the
  # whole graph — this bump IS part of the leyline-vcs integration's real
  # cost, not an artifact of our probe. Recorded, not hidden.
  local dir="$1"
  sed -i.bak 's/rusqlite = { version = "0.34", features = \["bundled"\] }/rusqlite = { version = "0.39", features = ["bundled"] }/' "$dir/Cargo.toml"
  rm -f "$dir/Cargo.toml.bak"
}

write_probe_jjlib_direct() {
  cat > "$1/src/jj_lib_cost_probe.rs" <<'RS'
//! SCRATCH-ONLY probe injected by scripts/jj-lib-cost-gate.sh (rosary-efd300).
//! Never part of the committed tree. Forces a REAL, non-dead-code-
//! eliminated call into jj-lib (init an actual jj repo) so build-time/
//! binary-size deltas reflect genuine usage, not an unused declared dep.
use anyhow::Result;
use jj_lib::config::StackedConfig;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;
use pollster::FutureExt as _;

pub fn run() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rosary-jj-cost-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())
        .map_err(|e| anyhow::anyhow!("settings: {e}"))?;
    Workspace::init_simple(&settings, &dir)
        .block_on()
        .map_err(|e| anyhow::anyhow!("jj init failed: {e}"))?;
    println!("jj-lib cost probe: initialized a real jj repo at {}", dir.display());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
RS
}

write_probe_leyline_vcs() {
  cat > "$1/src/jj_lib_cost_probe.rs" <<'RS'
//! SCRATCH-ONLY probe injected by scripts/jj-lib-cost-gate.sh (rosary-efd300).
//! Never part of the committed tree. Forces a REAL, non-dead-code-
//! eliminated call through leyline-vcs into jj-lib (JjIntegration::init)
//! so build-time/binary-size deltas reflect genuine usage.
use anyhow::Result;

pub fn run() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rosary-llvcs-cost-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    leyline_vcs::JjIntegration::init(&dir)?;
    println!("leyline-vcs cost probe: initialized a real jj repo at {}", dir.display());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
RS
}

dep_names() {
  # Unique transitive crate names for the `rosary` binary's normal-dependency
  # graph (workspace has 2 other members; -p rosary scopes to just what
  # `rsry` actually needs).
  local dir="$1"
  (cd "$dir" && cargo tree -p rosary -e normal --prefix none 2>/dev/null) \
    | sed 's/ v[0-9].*//; s/ (\*)$//' | sort -u
}

human_bytes() {
  local b="$1"
  awk -v b="$b" 'BEGIN { printf "%.1f MiB", b/1048576 }'
}

# --- baseline -----------------------------------------------------------
BASELINE_WT="$(new_worktree baseline)"

run_probe() {
  local probe_name="$1" wt="$2"
  echo
  echo "-- probe: $probe_name --"
  dep_names "$wt" > "$SCRATCH/deps-$probe_name.txt"
  local dep_count new_count
  dep_count=$(wc -l < "$SCRATCH/deps-$probe_name.txt" | tr -d ' ')
  if [ "$probe_name" = "baseline" ]; then
    : > "$SCRATCH/new-$probe_name.txt"
    new_count=0
    echo "transitive crates: $dep_count"
  else
    comm -13 "$SCRATCH/deps-baseline.txt" "$SCRATCH/deps-$probe_name.txt" > "$SCRATCH/new-$probe_name.txt"
    new_count=$(wc -l < "$SCRATCH/new-$probe_name.txt" | tr -d ' ')
    echo "transitive crates: $dep_count (net new vs baseline: $new_count)"
    echo "new crates:"
    sed 's/^/  + /' "$SCRATCH/new-$probe_name.txt"
  fi
  echo "$new_count" > "$SCRATCH/newcount-$probe_name.txt"

  if [ "$MODE_QUICK" -eq 1 ]; then
    return 0
  fi

  local target_dir="$SCRATCH/target-$probe_name"
  echo
  echo "cold build (release, fresh \$CARGO_TARGET_DIR — single sample, noisy, see NOTE)..."
  local t0 t1
  t0=$(date +%s)
  ( cd "$wt" && CARGO_TARGET_DIR="$target_dir" cargo build --release ) \
    > "$SCRATCH/cold-$probe_name.log" 2>&1
  t1=$(date +%s)
  local cold_secs=$(( t1 - t0 ))
  echo "cold build: ${cold_secs}s"
  echo "$cold_secs" > "$SCRATCH/cold-$probe_name.secs"

  local bin="$target_dir/release/rsry"
  local bin_bytes
  bin_bytes=$(stat -f%z "$bin" 2>/dev/null || stat -c%s "$bin")
  echo "binary size: $bin_bytes bytes ($(human_bytes "$bin_bytes"))"
  echo "$bin_bytes" > "$SCRATCH/size-$probe_name.bytes"

  echo
  echo "warm build ($WARM_REPEATS repeats — touch src/main.rs, incremental rebuild)..."
  local warm_times=()
  local i
  for i in $(seq 1 "$WARM_REPEATS"); do
    touch "$wt/src/main.rs"
    t0=$(date +%s)
    ( cd "$wt" && CARGO_TARGET_DIR="$target_dir" cargo build --release ) \
      > "$SCRATCH/warm-$probe_name-$i.log" 2>&1
    t1=$(date +%s)
    local w=$(( t1 - t0 ))
    warm_times+=("$w")
    echo "  rep $i: ${w}s"
  done
  printf '%s\n' "${warm_times[@]}" | sort -n > "$SCRATCH/warm-$probe_name.sorted"
  local warm_median
  warm_median=$(awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}' "$SCRATCH/warm-$probe_name.sorted")
  echo "warm median: ${warm_median}s"
  echo "$warm_median" > "$SCRATCH/warm-$probe_name.median"
}

run_probe baseline "$BASELINE_WT"

# --- probe: jj-lib direct ------------------------------------------------
JJLIB_WT="$(new_worktree probe-jjlib)"
add_dep_line "$JJLIB_WT" 'jj-lib = { version = "0.42", default-features = false }
pollster = "0.4"'
write_probe_jjlib_direct "$JJLIB_WT"
inject_main_hook "$JJLIB_WT"
run_probe jjlib "$JJLIB_WT"

# --- probe: leyline-vcs (path dep on the local ley-line-open checkout) ---
if [ "$JJLIB_ONLY" -eq 0 ]; then
  LLVCS_WT="$(new_worktree probe-llvcs)"
  add_dep_line "$LLVCS_WT" "leyline-vcs = { path = \"$LLVCS_PATH\" }"
  # Real, load-bearing version-compatibility cost: leyline-fs's workspace
  # pins rusqlite 0.39.0; rosary pins 0.34. Cargo's `links = "sqlite3"`
  # uniqueness rule forces one to win across the whole resolved graph — this
  # bump is part of what adopting leyline-vcs actually costs, not a probe
  # artifact, and is reported as such below.
  bump_rusqlite_line "$LLVCS_WT"
  write_probe_leyline_vcs "$LLVCS_WT"
  inject_main_hook "$LLVCS_WT"
  run_probe llvcs "$LLVCS_WT"
fi

# --- verdict --------------------------------------------------------------
echo
echo "== summary =="
printf '%-16s %10s %14s %14s %14s\n' "probe" "new-deps" "cold(s)" "warm-med(s)" "bin-size"
print_row() {
  local name="$1"
  local new cold warm size
  new=$(cat "$SCRATCH/newcount-$name.txt" 2>/dev/null || echo "n/a")
  cold=$(cat "$SCRATCH/cold-$name.secs" 2>/dev/null || echo "n/a")
  warm=$(cat "$SCRATCH/warm-$name.median" 2>/dev/null || echo "n/a")
  size="n/a"
  if [ -f "$SCRATCH/size-$name.bytes" ]; then
    size="$(human_bytes "$(cat "$SCRATCH/size-$name.bytes")")"
  fi
  printf '%-16s %10s %14s %14s %14s\n' "$name" "$new" "$cold" "$warm" "$size"
}
print_row baseline
print_row jjlib
[ "$JJLIB_ONLY" -eq 0 ] && print_row llvcs

FAIL=0
check_budget() {
  local name="$1"
  local new_count
  new_count=$(cat "$SCRATCH/newcount-$name.txt" 2>/dev/null || echo 0)
  if [ "$new_count" -gt "$BUDGET_NEW_DEPS" ]; then
    echo "FAIL[$name]: net-new transitive crates $new_count > budget $BUDGET_NEW_DEPS"
    FAIL=1
  fi

  if [ "$MODE_QUICK" -eq 1 ]; then
    return 0
  fi

  if [ -f "$SCRATCH/size-$name.bytes" ]; then
    local base_bin="$SCRATCH/target-baseline/release/rsry"
    if [ -f "$base_bin" ]; then
      local base_bytes probe_bytes delta_mb
      base_bytes=$(stat -f%z "$base_bin" 2>/dev/null || stat -c%s "$base_bin")
      probe_bytes=$(cat "$SCRATCH/size-$name.bytes")
      delta_mb=$(( (probe_bytes - base_bytes) / 1048576 ))
      if [ "$delta_mb" -gt "$BUDGET_BIN_MB" ]; then
        echo "FAIL[$name]: binary size grew ${delta_mb}MiB > budget ${BUDGET_BIN_MB}MiB"
        FAIL=1
      fi
    fi
  fi

  if [ -f "$SCRATCH/cold-$name.secs" ] && [ -f "$SCRATCH/cold-baseline.secs" ]; then
    local base_cold probe_cold pct
    base_cold=$(cat "$SCRATCH/cold-baseline.secs")
    probe_cold=$(cat "$SCRATCH/cold-$name.secs")
    if [ "$base_cold" -gt 0 ]; then
      pct=$(( (probe_cold - base_cold) * 100 / base_cold ))
      echo "note[$name]: cold build time delta ${pct}% (single-sample, advisory only — see NOTE ON NOISE)"
      if [ "$pct" -gt "$BUDGET_COLD_PCT" ]; then
        echo "FAIL[$name]: cold build time grew ${pct}% > budget ${BUDGET_COLD_PCT}% (advisory: noisy single-sample metric — investigate before treating as gospel)"
        FAIL=1
      fi
    fi
  fi
}

check_budget jjlib
[ "$JJLIB_ONLY" -eq 0 ] && check_budget llvcs

echo
if [ "$FAIL" -ne 0 ]; then
  echo "GATE: FAIL — a measured cost exceeded its declared budget. This is a real result, not a bug in the gate."
  exit 1
else
  echo "GATE: PASS — measured jj-lib adoption cost stayed within budget."
fi
