#!/usr/bin/env bash
#
# Runs every test suite the crate has, across the feature combinations that
# actually ship. A bare `cargo test` is not enough here for two reasons:
#
#   * it builds the default feature set (`server`) only, so it never compiles
#     the `files` module at all, and
#   * it runs on the host, so the `#[wasm_bindgen_test]` IndexedDB tests are
#     compiled out and silently contribute nothing.
#
# `cargo publish` verifies only the default feature set too, so without this
# script a feature-gated break reaches crates.io unnoticed.
#
# Requires: wasm32-unknown-unknown target, and (for the browser step) wasm-pack
# plus a chromedriver matching the installed Chrome.
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-pack
#
# Set SKIP_BROWSER_TESTS=1 to skip the final step.
set -euo pipefail

SCRIPTPATH="$( cd "$(dirname "$0")" ; pwd -P )"
cd "$SCRIPTPATH/.."

# The whole workspace on default features — the macros, the examples, and saps
# itself. This is the run that has to stay green regardless of the file layer.
echo "── 1/5 · workspace (default features) ───────────────────────────────"
cargo test

# The remaining steps are scoped with `-p saps`: at a virtual workspace root a
# bare `--no-default-features` does not scope to one member, so without `-p` the
# feature flags are silently not applied to the crate under test.
echo "── 2/5 · saps baseline (errors, constants, kernel) ──────────────────"
cargo test -p saps --no-default-features

echo "── 3/5 · saps files: disk + in-memory engines ───────────────────────"
cargo test -p saps --no-default-features --features files

echo "── 4/5 · saps files-kv: the redb engine ─────────────────────────────"
cargo test -p saps --no-default-features --features files-kv

echo "── 5/5 · saps wasm ──────────────────────────────────────────────────"
# The compile check is the always-on gate: it proves the browser build is
# sound without needing a browser on the machine.
echo "   compiling for wasm32-unknown-unknown"
cargo check -p saps --no-default-features --features files-indexed-db \
	--target wasm32-unknown-unknown

if [ "${SKIP_BROWSER_TESTS:-0}" = "1" ]; then
	echo "   SKIP_BROWSER_TESTS=1 — skipping the headless browser run"
	echo
	echo "all suites passed (browser tests skipped)"
	exit 0
fi

# The IndexedDB tests declare `wasm_bindgen_test_configure!(run_in_browser)`, so
# they need a real browser — Node has no IndexedDB. Note the feature flags are
# positional here: `wasm-pack test <path> <cargo args>`. Putting them after a
# `--` separator sends them to the test harness instead of the build, and the
# build then falls back to the default features and fails on tokio's native
# reactor.
echo "   running the IndexedDB tests in headless Chrome"
if ! wasm-pack test --headless --chrome saps \
	--no-default-features --features files-indexed-db; then
	echo >&2
	echo "error: the browser test run failed." >&2
	echo "  If the driver died with SIGKILL or a 404, chromedriver does not match" >&2
	echo "  the installed Chrome. Check both versions and pass a matching driver:" >&2
	echo "    wasm-pack test --headless --chrome --chromedriver <path> saps \\" >&2
	echo "      --no-default-features --features files-indexed-db" >&2
	echo "  Or set SKIP_BROWSER_TESTS=1 to skip this step." >&2
	exit 1
fi

echo
echo "all suites passed"
