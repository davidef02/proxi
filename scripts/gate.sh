#!/usr/bin/env bash
# The single source of truth for the repo's check gate.
# Both callers run THIS script, so CI and local runs can never drift apart:
#   - .github/workflows/ci.yml (job `checks`) runs `bash scripts/gate.sh`
#   - AGENTS.md records it as: Gate: `bash scripts/gate.sh`
# Keep the gate as ONE bash script; do not split or inline copies of these steps elsewhere.
# Run it from the repo root with: bash scripts/gate.sh
set -euo pipefail

npm ci
npm run typecheck:app
# NOTE: CI also installs the Rust toolchain (rustup toolchain install stable --profile minimal &&
# rustup default stable) before the cargo steps below; that setup stays in ci.yml because it is
# CI-environment provisioning, not a check. Locally you are expected to have Rust installed.
cargo build --release --locked
npm run app:build # test:app expects app/web/dist
npm run test:app # needs the binary built first
npm run test:rust # ported HTTP/SSE tests against target/release/proxi
# also the test browser of the Rust render tests (PROXI_TEST_BROWSER)
if [[ "$OSTYPE" == linux* ]]; then
  npx playwright install --with-deps chromium
else
  npx playwright install chromium
fi
PROXI_TEST_BROWSER="$(node -p "require('playwright-core').chromium.executablePath()")"
test -f "$PROXI_TEST_BROWSER" || { echo "chromium not found at '$PROXI_TEST_BROWSER'" >&2; exit 1; }
export PROXI_TEST_BROWSER
cargo test --locked
npm run e2e
