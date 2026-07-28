#!/usr/bin/env python3
"""Coverage ratchet — the test-coverage analog of the mache smell baseline.

The number is ONLY ever the tool's output (cargo-llvm-cov's JSON export),
committed as data and re-derived by the gate. No human/agent ever types a
coverage figure — which is the structural guard against the fabrication in
rosary-5732ee (a prior agent claimed 110/6/0 lines that never existed).

Usage:
  # generate/refresh the committed baseline from a fresh run
  cargo llvm-cov --json --output-path /tmp/cov.json --workspace
  python3 scripts/coverage-ratchet.py --write /tmp/cov.json

  # gate: fail if any file regressed below its committed floor
  cargo llvm-cov --json --output-path /tmp/cov.json --workspace
  python3 scripts/coverage-ratchet.py --check /tmp/cov.json

The floor is per-file line-coverage percent (plus a total floor). A decomposition
PR (e.g. splitting handoff.rs, rosary-6a143f) must not drop any file's coverage —
that's the net that makes "break it apart" safe instead of hopeful.
"""
import json
import sys

BASELINE = "docs/coverage-baseline.json"
# Tolerance for float jitter / trivially-small files (percentage points).
EPSILON = 0.5


def per_file_pct(cov_json_path):
    """Parse llvm-cov export JSON → {filename: line_percent}. Repo-relative paths."""
    import os
    root = os.getcwd() + os.sep
    data = json.load(open(cov_json_path))
    out = {}
    total_cov = total_cnt = 0
    for export in data.get("data", []):
        for f in export.get("files", []):
            name = f["filename"]
            if name.startswith(root):
                name = name[len(root):]
            lines = f.get("summary", {}).get("lines", {})
            cnt = lines.get("count", 0)
            cov = lines.get("covered", 0)
            if cnt:
                out[name] = round(100.0 * cov / cnt, 2)
                total_cov += cov
                total_cnt += cnt
    total = round(100.0 * total_cov / total_cnt, 2) if total_cnt else 0.0
    return out, total


def write_baseline(cov_json_path):
    import os
    files, total = per_file_pct(cov_json_path)
    # Record where the baseline was generated. CI enforces only against a
    # CI-native baseline (COVERAGE_ENV=ci); a locally-generated one (dolt/jj
    # present → different coverage) is bootstrap-only in CI.
    env = os.environ.get("COVERAGE_ENV", "local")
    doc = {"version": 1, "env": env, "total_line_pct": total, "files": dict(sorted(files.items()))}
    json.dump(doc, open(BASELINE, "w"), indent=1)
    open(BASELINE, "a").write("\n")
    print(f"wrote {BASELINE}: {len(files)} files, total {total}% lines (env={env})")


def set_floor(path, pct):
    """Lower ONE file's floor, preserving every other entry and the `env` marker.

    Why this exists: a full `--write` from a dev machine stamps `env=local`,
    which makes CI skip enforcement entirely — the exact way this gate sat
    disarmed for its whole life (rosary-f78208). And a local full rewrite would
    replace CI-native floors with local numbers, which are HIGHER for the
    dolt/jj-dependent files (those tests skip in CI), making the CI floors
    unreachable and CI red forever.

    So an intentional decrease edits exactly one floor and touches nothing else.
    The resulting one-line diff is the reviewable artifact — the same control the
    committed smell baseline relies on. Use the percentage CI REPORTED, not a
    local measurement, whenever the file's coverage depends on dolt/jj.
    """
    base = json.load(open(BASELINE))
    if path not in base["files"]:
        raise SystemExit(f"{path} is not in the baseline; nothing to lower")
    old = base["files"][path]
    if pct > old:
        raise SystemExit(
            f"{path}: {pct} is ABOVE the current floor {old} — floors rise by "
            "regeneration on main, not by hand"
        )
    base["files"][path] = pct
    json.dump(base, open(BASELINE, "w"), indent=1)
    open(BASELINE, "a").write("\n")
    print(f"lowered {path}: {old}% -> {pct}% (env={base.get('env')!r} preserved)")


def check(cov_json_path):
    import os
    base = json.load(open(BASELINE))
    # Bootstrap: in CI, don't enforce a locally-generated baseline (CI lacks
    # dolt/jj so its numbers differ). A main-push regenerates it env=ci; after
    # that, CI enforces. Locally we always enforce (the decomposition net).
    if os.environ.get("CI") and base.get("env") != "ci":
        # LOUD. This returns 0, so the check goes green while enforcing nothing
        # — which is exactly how the gate sat disarmed from the day it was built
        # (the main-push commit-back was blocked by the branch ruleset and the
        # failure was swallowed into a warning). A `::warning::` annotation
        # surfaces on the PR itself rather than only in the job log, so a
        # vacuous pass cannot look like a real one.
        print(
            f"::warning::coverage ratchet ENFORCED NOTHING: baseline env="
            f"{base.get('env')!r} is not CI-native. This check is green but "
            "vacuous until a main-push regenerates the baseline and that PR is "
            "merged (rosary-f78208)."
        )
        return 0
    cur, cur_total = per_file_pct(cov_json_path)
    regressions = []
    for name, floor in base["files"].items():
        now = cur.get(name)
        if now is None:
            continue  # file removed/renamed — not a regression here
        if now < floor - EPSILON:
            regressions.append((name, floor, now))
    if cur_total < base["total_line_pct"] - EPSILON:
        regressions.append(("<total>", base["total_line_pct"], cur_total))
    if regressions:
        print("coverage ratchet: %d regression(s) below baseline:" % len(regressions))
        for name, floor, now in sorted(regressions):
            print(f"  {name}: {floor}% -> {now}%")
        print("\nIf intentional, refresh: python3 scripts/coverage-ratchet.py --write <cov.json>")
        return 1
    print(f"coverage ratchet OK — total {cur_total}% (floor {base['total_line_pct']}%), no per-file regressions")
    return 0


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("--write", "--check"):
        print(__doc__)
        return 2
    mode, path = sys.argv[1], sys.argv[2]
    return write_baseline(path) or 0 if mode == "--write" else check(path)


if __name__ == "__main__":
    import sys
    if "--set-floor" in sys.argv:
        i = sys.argv.index("--set-floor")
        set_floor(sys.argv[i + 1], float(sys.argv[i + 2]))
        raise SystemExit(0)

    sys.exit(main())
