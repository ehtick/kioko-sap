#!/usr/bin/env bash
set -euo pipefail

SCRIPTPATH="$( cd "$(dirname "$0")" ; pwd -P )"
cd "$SCRIPTPATH/.."

cp README.md saps/README.md
trap 'rm -f saps/README.md' EXIT

cd saps
cargo publish
