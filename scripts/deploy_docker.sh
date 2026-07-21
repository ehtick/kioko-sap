#!/usr/bin/env bash
#
# Builds the saps-base image for all supported platforms and pushes it to
# Docker Hub: https://hub.docker.com/repository/docker/maxwellflitton/saps-base/general
#
# The toolchain versions below are pinned here (single source of truth) and
# passed into the Dockerfile as build args. They are also baked into the image
# tag so consumers can tell which toolchain a given image ships with.
#
# Tag format:  r<rust>-n<node>-d<deno>
#   r = Rust version      (e.g. r1.93.0)
#   n = Node major version (e.g. n24)
#   d = Deno version      (e.g. d2.1.4)
# Order is always rust, node, deno. wasm-pack is pinned too but kept out of the
# tag to stop it ballooning; see the README for the full mapping.
#
# The same build is also pushed as `latest`.
#
# Usage:
#   ./scripts/deploy_docker.sh            # build + push all platforms
#   PUSH=false ./scripts/deploy_docker.sh # build only (single platform, local)
#
set -euo pipefail

# -----------------------------------------------------------------------------
# Pinned toolchain versions (single source of truth)
# -----------------------------------------------------------------------------
RUST_VERSION="1.93.0"
NODE_MAJOR="24"
DENO_VERSION="2.1.4"
WASM_PACK_VERSION="0.13.1"

# -----------------------------------------------------------------------------
# Image / registry config
# -----------------------------------------------------------------------------
IMAGE="maxwellflitton/saps-base"
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PUSH="${PUSH:-true}"

# Compact, ordered version tag: rust -> node -> deno
VERSION_TAG="r${RUST_VERSION}-n${NODE_MAJOR}-d${DENO_VERSION}"

# Resolve repo root so the script works regardless of the current directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "==> Image:      ${IMAGE}"
echo "==> Tags:       ${VERSION_TAG}, latest"
echo "==> Platforms:  ${PLATFORMS}"
echo "==> Rust:       ${RUST_VERSION}"
echo "==> Node:       ${NODE_MAJOR}.x"
echo "==> Deno:       ${DENO_VERSION}"
echo "==> wasm-pack:  ${WASM_PACK_VERSION}"
echo

# -----------------------------------------------------------------------------
# Ensure a buildx builder exists (required for multi-platform builds).
# -----------------------------------------------------------------------------
BUILDER="saps-base-builder"
if ! docker buildx inspect "${BUILDER}" >/dev/null 2>&1; then
  echo "==> Creating buildx builder '${BUILDER}'"
  docker buildx create --name "${BUILDER}" --driver docker-container --bootstrap
fi
docker buildx use "${BUILDER}"

# -----------------------------------------------------------------------------
# Build (and optionally push).
# -----------------------------------------------------------------------------
BUILD_ARGS=(
  --build-arg "RUST_VERSION=${RUST_VERSION}"
  --build-arg "NODE_MAJOR=${NODE_MAJOR}"
  --build-arg "DENO_VERSION=${DENO_VERSION}"
  --build-arg "WASM_PACK_VERSION=${WASM_PACK_VERSION}"
  --tag "${IMAGE}:${VERSION_TAG}"
  --tag "${IMAGE}:latest"
  --file "${REPO_ROOT}/Dockerfile"
)

if [[ "${PUSH}" == "true" ]]; then
  # Multi-platform images cannot be loaded into the local daemon; they must be
  # pushed straight to the registry. Make sure you are logged in first:
  #   docker login
  #
  # --provenance=false / --sbom=false disable the BuildKit attestation manifests.
  # They add extra blobs that Docker Hub sometimes rejects with a spurious
  # "400 Bad request" during the push, so we leave them off for a plain base image.
  #
  # Docker Hub also returns transient 400s on large-layer blob uploads, so retry
  # the build/push a few times before giving up.
  echo "==> Building all platforms and pushing to Docker Hub"
  attempt=1
  max_attempts=3
  until docker buildx build \
    --platform "${PLATFORMS}" \
    --provenance=false \
    --sbom=false \
    --push \
    "${BUILD_ARGS[@]}" \
    "${REPO_ROOT}"; do
    if (( attempt >= max_attempts )); then
      echo "==> Push failed after ${max_attempts} attempts." >&2
      echo "    If this is a 400 from registry-1.docker.io, re-run 'docker login'," >&2
      echo "    check any active VPN/proxy, and confirm push access to ${IMAGE}." >&2
      exit 1
    fi
    echo "==> Push attempt ${attempt} failed; retrying ($(( attempt + 1 ))/${max_attempts})..." >&2
    attempt=$(( attempt + 1 ))
    sleep 5
  done
  echo
  echo "==> Pushed ${IMAGE}:${VERSION_TAG} and ${IMAGE}:latest"
else
  # Local-only build: buildx --load supports a single platform at a time, so
  # build for the host arch only.
  echo "==> Building locally (single platform, no push)"
  docker buildx build \
    --load \
    "${BUILD_ARGS[@]}" \
    "${REPO_ROOT}"
  echo
  echo "==> Built ${IMAGE}:${VERSION_TAG} locally (not pushed)"
fi
