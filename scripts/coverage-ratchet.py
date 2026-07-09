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


def check(cov_json_path):
    import os
    base = json.load(open(BASELINE))
    # Bootstrap: in CI, don't enforce a locally-generated baseline (CI lacks
    # dolt/jj so its numbers differ). A main-push regenerates it env=ci; after
    # that, CI enforces. Locally we always enforce (the decomposition net).
    if os.environ.get("CI") and base.get("env") != "ci":
        print(f"coverage ratchet: baseline env={base.get('env')!r} not CI-native — "
              "enforcement skipped until a main-push regenerates it (bootstrap).")
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
    sys.exit(main())
