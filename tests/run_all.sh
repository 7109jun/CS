#!/usr/bin/env bash
# Full verification: unit tests, language tests, fixtures, MCP client compatibility.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --quiet
export CLS="${CLS:-$PWD/target/debug/cls}"
cargo test --quiet
echo "== language tests";            (cd tests/fixtures/lang && "$CLS" test)
echo "== header tests";              (cd tests/fixtures/headers && "$CLS" test)
echo "== example project tests";     (cd examples/projecttools && "$CLS" test)
echo "== diagnostics (must fail)";   if (cd tests/fixtures/errors && "$CLS" check >/dev/null 2>&1); then echo "errors fixture unexpectedly passed"; exit 1; else echo "  ok   rejected invalid program"; fi
echo "== permissions & limits";      python3 tests/fixtures/perms/run.py
echo "== stdio limits/cancellation"; python3 tests/stdio_limits.py
echo "== MCP SDK compatibility";     python3 tests/integration.py
echo "ALL TESTS PASSED"
