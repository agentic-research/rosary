#!/usr/bin/env bash
# Test for scripts/run-semgrep.sh degradation logic (rosary-9e5138).
# Stubs `semgrep` on PATH to exercise the three exit paths without needing a
# real semgrep or a broken trust store.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/run-semgrep.sh"
fails=0

run_case() {
  local name="$1" stub="$2" want_code="$3" want_grep="$4"
  local bindir out code
  bindir="$(mktemp -d)"
  printf '%s\n' "$stub" > "$bindir/semgrep"
  chmod +x "$bindir/semgrep"
  out=$(PATH="$bindir:$PATH" bash "$SCRIPT" .semgrep/rules.yml . 2>&1)
  code=$?
  rm -rf "$bindir"
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL [$name]: exit $code, want $want_code"; echo "  out: $out"; fails=$((fails+1)); return
  fi
  if [ -n "$want_grep" ] && ! printf '%s' "$out" | grep -qi "$want_grep"; then
    echo "FAIL [$name]: output missing /$want_grep/"; echo "  out: $out"; fails=$((fails+1)); return
  fi
  echo "ok   [$name]"
}

# 1) clean scan → exit 0
run_case "clean" '#!/usr/bin/env bash
echo "ran 5 rules, 0 findings"; exit 0' 0 ""

# 2) real finding (exit 1) → propagate failure
run_case "finding" '#!/usr/bin/env bash
echo "rule X matched at foo.rs:1"; exit 1' 1 "matched"

# 3) X509 trust-init failure → degrade to skip (exit 0)
run_case "x509-trust" '#!/usr/bin/env bash
echo "Failed to create system store X509 authenticator: ca-certs: empty trust anchors" >&2; exit 2' 0 "unusable"

# 4) other genuine error (exit 2, no trust signature) → propagate failure
run_case "other-error" '#!/usr/bin/env bash
echo "invalid config: no such file" >&2; exit 2' 2 "invalid config"

if [ "$fails" -eq 0 ]; then echo "PASS: all run-semgrep cases"; exit 0; fi
echo "FAILED: $fails case(s)"; exit 1
