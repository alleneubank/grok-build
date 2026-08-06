#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# Fork release builder — run from an Apple Silicon Mac
# ═══════════════════════════════════════════════════════════════════════════
#
# Builds BOTH fleet platforms from one Mac host (codex-style):
#   - darwin.arm64  → native cargo release
#   - linux.x86_64  → OrbStack/Docker linux/amd64 container
#
# A release that only ships macOS breaks the fleet: mise.lock pins per-platform
# URLs, and Linux hosts fail `mise install` / `dotfiles-upgrade` when the
# linux asset is missing. This script refuses --publish until both exist.
#
# Shape: https://gist.github.com/alleneubank/bf7d25542a49b136671db0e4bb65226d
# See also: FORK.md
#
# Usage:
#   ./scripts/release-fork.sh                 # dry-run: print the plan
#   ./scripts/release-fork.sh --run           # build macOS + Linux, stage archives
#   ./scripts/release-fork.sh --run --publish # build both, then tag + gh prerelease
#   ./scripts/release-fork.sh --check-fleet   # verify staged archives only
#   ./scripts/release-fork.sh --publish       # publish already-staged archives
#                                              (requires literal --publish; no rebuild)
#
# Prerequisites (Mac arm64):
#   - clean git tree at the commit you intend to ship
#   - rust toolchain (rust-toolchain.toml)
#   - OrbStack or Docker Desktop with `docker` on PATH and engine running
#   - gh authenticated for github.com/alleneubank/grok-build
#
# ═══════════════════════════════════════════════════════════════════════════
set -euo pipefail

REPO="${FORK_REPO:-alleneubank/grok-build}"
# Fleet platforms that MUST be present before publish (comma-separated).
FORK_REQUIRED_PLATFORMS="${FORK_REQUIRED_PLATFORMS:-darwin.arm64,linux.x86_64}"
LINUX_IMAGE="${FORK_LINUX_IMAGE:-grok-fork-release-linux:rust-1.94.0}"
LINUX_DOCKERFILE=".github/docker/fork-release-linux.Dockerfile"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  sed -n '2,45p' "$0"
}

die() {
  echo "$*" >&2
  exit 1
}

RUN_CONFIRMED=false
PUBLISH_CONFIRMED=false
MODE="plan"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run) RUN_CONFIRMED=true; MODE="run"; shift ;;
    --publish)
      PUBLISH_CONFIRMED=true
      # --publish alone = publish staged; with --run = build then publish
      if [[ "$MODE" != "run" ]]; then MODE="publish"; fi
      shift
      ;;
    --check-fleet) MODE="check-fleet"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1 (try --run | --publish | --check-fleet)" ;;
  esac
done

DIST="$ROOT/dist/fork-release"
mkdir -p "$DIST"

# ── identity ──────────────────────────────────────────────────────────────

compute_version() {
  local base short9 date_utc
  base="$(grep -m1 '^version' crates/codegen/xai-grok-pager-bin/Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
  short9="$(git rev-parse --short=9 HEAD)"
  date_utc="$(date -u +%Y%m%d)"
  echo "${base}-fork.${date_utc}.g${short9}"
}

VERSION="$(compute_version)"
TAG="v${VERSION}"
MACOS_ARCH="darwin.arm64"
LINUX_ARCH="linux.x86_64"
MACOS_ARCHIVE="grok.${VERSION}.${MACOS_ARCH}.tar.gz"
LINUX_ARCHIVE="grok.${VERSION}.${LINUX_ARCH}.tar.gz"

# ── helpers ───────────────────────────────────────────────────────────────

require_clean_tree() {
  [[ -z "$(git status --porcelain)" ]] \
    || die "dirty tree — commit or stash before release (reproducible tag)"
}

require_macos_builder() {
  [[ "$(uname -s)" == Darwin ]] || die "Fork releases must be built on macOS (Apple Silicon)"
  [[ "$(uname -m)" == arm64 ]] || die "Fork releases must be built on Apple Silicon (arm64)"
  command -v docker >/dev/null 2>&1 || die "docker is required for the Linux x86_64 build (OrbStack)"
  docker info >/dev/null 2>&1 || die "Docker engine not reachable — start OrbStack / Docker Desktop"
}

archive_path() {
  echo "$DIST/$1"
}

write_checksums() {
  (
    cd "$DIST"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum grok.*.tar.gz > checksums.txt
    else
      shasum -a 256 grok.*.tar.gz | awk '{print $1"  "$2}' > checksums.txt
    fi
  )
}

list_present_platforms() {
  local f base rest
  for f in "$DIST"/grok.*.tar.gz; do
    [[ -e "$f" ]] || continue
    base="$(basename "$f" .tar.gz)"
    rest="${base#grok.}"
    if [[ "$rest" =~ \.((darwin|linux)\.(arm64|x86_64))$ ]]; then
      echo "${BASH_REMATCH[1]}"
    fi
  done | sort -u
}

missing_required_platforms() {
  local present required p
  present="$(list_present_platforms | tr '\n' ' ')"
  IFS=',' read -r -a required <<< "$FORK_REQUIRED_PLATFORMS"
  for p in "${required[@]}"; do
    p="$(echo "$p" | tr -d '[:space:]')"
    [[ -n "$p" ]] || continue
    if ! echo " $present " | grep -q " $p "; then
      echo "$p"
    fi
  done
}

require_fleet_complete() {
  local missing
  missing="$(missing_required_platforms || true)"
  if [[ -n "$missing" ]]; then
    echo "refusing: fleet platforms incomplete" >&2
    echo "required: ${FORK_REQUIRED_PLATFORMS}" >&2
    echo "present:  $(list_present_platforms | tr '\n' ' ')" >&2
    echo "missing:" >&2
    echo "$missing" | sed 's/^/  - /' >&2
    echo >&2
    echo "A single-platform release breaks mise on the other OS when the pin" >&2
    echo "is shared fleet-wide. Build both platforms (Mac host + Linux container)." >&2
    exit 1
  fi
}

stage_binary() {
  # stage_binary <binary-path> <os.arch>
  local bin_src="$1" os_arch="$2" archive stage
  archive="grok.${VERSION}.${os_arch}.tar.gz"
  [[ -x "$bin_src" ]] || die "missing binary: $bin_src"
  stage="$(mktemp -d)"
  cp -f "$bin_src" "$stage/grok"
  chmod +x "$stage/grok"
  tar -C "$stage" -czf "$DIST/$archive" grok
  rm -rf "$stage"
  echo "staged $DIST/$archive"
}

# ── build ─────────────────────────────────────────────────────────────────

docker_no_keychain() {
  # Avoid osxkeychain prompts in non-interactive agent sessions.
  if [[ -z "${DOCKER_CONFIG:-}" || ! -f "${DOCKER_CONFIG}/config.json" ]]; then
    export DOCKER_CONFIG
    DOCKER_CONFIG="$(mktemp -d)"
    printf '%s\n' '{"auths":{},"credsStore":""}' >"${DOCKER_CONFIG}/config.json"
  fi
}

ensure_linux_image() {
  docker_no_keychain
  echo "Ensuring Linux builder image ${LINUX_IMAGE}..."
  # Host-arch Linux container (arm64 on Apple Silicon) that cross-compiles to
  # x86_64-unknown-linux-gnu. Avoid --platform linux/amd64: OrbStack often
  # tags host-arch images anyway, then `docker run --platform` fails to match.
  # --pull=false: use a local base (e.g. debian:bookworm-slim) when present.
  docker build \
    --pull=false \
    --tag "${LINUX_IMAGE}" \
    --file "${ROOT}/${LINUX_DOCKERFILE}" \
    "${ROOT}/.github/docker"
}

build_macos() {
  echo "Building ${VERSION} for ${MACOS_ARCH} (native macOS)..."
  export GROK_VERSION="${VERSION}"
  cargo build -p xai-grok-pager-bin --release
  stage_binary "$ROOT/target/release/xai-grok-pager" "$MACOS_ARCH"
  "$ROOT/target/release/xai-grok-pager" --version || true
}

build_linux() {
  echo "Building ${VERSION} for ${LINUX_ARCH} (Docker + cross-compile to x86_64)..."
  ensure_linux_image
  docker_no_keychain

  # Persistent cargo/target volumes speed rebuilds (same idea as codex).
  docker run --rm --pull=never \
    --mount "type=bind,src=${ROOT},dst=/workspace" \
    --mount "type=bind,src=${DIST},dst=/output" \
    --mount type=volume,src=grok-fork-release-cargo,dst=/cargo \
    --mount type=volume,src=grok-fork-release-target,dst=/target \
    --env CARGO_HOME=/cargo \
    --env CARGO_TARGET_DIR=/target \
    --env GROK_VERSION="${VERSION}" \
    --env PROTOC=/usr/bin/protoc \
    --env CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    --env CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    --env CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    "${LINUX_IMAGE}" \
    bash -lc '
      set -euo pipefail
      git config --global --add safe.directory /workspace
      cd /workspace
      # Fleet/mise linux-x64: always produce x86_64-unknown-linux-gnu.
      rustup target add x86_64-unknown-linux-gnu
      cargo build -p xai-grok-pager-bin --release --target x86_64-unknown-linux-gnu
      install -m 0755 /target/x86_64-unknown-linux-gnu/release/xai-grok-pager /tmp/grok
      file /tmp/grok | tee /dev/stderr
      file /tmp/grok | grep -q "x86-64" \
        || { echo "expected x86_64 ELF, got: $(file /tmp/grok)" >&2; exit 1; }
      tar -C /tmp -czf "/output/grok.'"${VERSION}"'.linux.x86_64.tar.gz" grok
      echo "linux binary version (may fail under qemu if static checks only):"
      /tmp/grok --version || true
    '
  echo "staged $DIST/$LINUX_ARCHIVE"
}

build_all() {
  require_clean_tree
  require_macos_builder
  rm -rf "$DIST"
  mkdir -p "$DIST"
  build_macos
  build_linux
  write_checksums
  require_fleet_complete
  echo
  echo "Built fleet-complete set for ${VERSION}:"
  list_present_platforms | sed 's/^/  - /'
  cat "$DIST/checksums.txt"
}

# ── publish ───────────────────────────────────────────────────────────────

publish_release() {
  require_clean_tree
  require_fleet_complete
  write_checksums

  if git rev-parse "$TAG" >/dev/null 2>&1; then
    die "tag $TAG already exists locally"
  fi

  local archives
  mapfile -t archives < <(ls -1 "$DIST"/grok.*.tar.gz)

  echo
  echo "Publishing prerelease ${TAG} to github.com/${REPO}"
  echo "  platforms: $(list_present_platforms | tr '\n' ' ')"
  echo "  assets:"
  printf '    %s\n' "${archives[@]##*/}"
  echo "    checksums.txt"

  git tag -a "$TAG" -m "fork release ${VERSION}"
  local token
  token="$(gh auth token 2>/dev/null || true)"
  if [[ -n "$token" ]]; then
    git push "https://x-access-token:${token}@github.com/${REPO}.git" "refs/tags/${TAG}"
  else
    git push origin "refs/tags/${TAG}"
  fi

  local notes
  notes="$(mktemp)"
  cat >"$notes" <<EOF
Personal fork of xai-org/grok-build.

Includes:
- lifecycle PermissionRequest for interactive tool approvals
- plugin hooks loaded at session cold start (no /hooks reload required)

Version stamp: \`${VERSION}\`

## Fleet platforms (required)

Built from an Apple Silicon Mac: native darwin.arm64 + Docker linux/amd64.

- required: \`${FORK_REQUIRED_PLATFORMS}\`
- shipped: \`$(list_present_platforms | paste -sd, -)\`

Do **not** pin a version that is missing a fleet platform.

## Auto-update

Still embeds the official updater client. Keep:

\`\`\`toml
[cli]
auto_update = false
\`\`\`

or \`GROK_DISABLE_AUTOUPDATER=1\`.

## Install (mise)

\`\`\`toml
"github:alleneubank/grok-build" = { version = "${VERSION}", exe = "grok", os = ["linux", "macos"] }
\`\`\`

Then \`mise lock\` and commit \`mise.lock\`.
EOF

  gh release create "$TAG" \
    --repo "$REPO" \
    --prerelease \
    --title "$VERSION" \
    --notes-file "$notes" \
    "${archives[@]}" \
    "$DIST/checksums.txt"
  rm -f "$notes"
  echo "published: https://github.com/${REPO}/releases/tag/${TAG}"
}

# ── plan (dry-run) ────────────────────────────────────────────────────────

print_plan() {
  cat <<EOF
Fork release plan (dry-run — no build, no publish)

  host:       $(uname -s) $(uname -m)  (must be Darwin arm64 for --run)
  commit:     $(git rev-parse --short=12 HEAD)
  version:    ${VERSION}
  tag:        ${TAG}
  repo:       ${REPO}
  required:   ${FORK_REQUIRED_PLATFORMS}
  dist:       ${DIST}

  builds (from this Mac):
    1. ${MACOS_ARCH}  → native cargo build -p xai-grok-pager-bin --release
    2. ${LINUX_ARCH}  → Docker Linux container cross-compile to x86_64 (${LINUX_IMAGE})

  archives:
    - ${MACOS_ARCHIVE}
    - ${LINUX_ARCHIVE}
    - checksums.txt

Next steps:
  ./scripts/release-fork.sh --run              # build both platforms
  ./scripts/release-fork.sh --run --publish    # build + tag + gh prerelease
  # or after a successful --run:
  ./scripts/release-fork.sh --publish          # publish staged artifacts only

After publish, widen the mise pin to os = ["linux", "macos"], bump version,
run mise lock, commit.

See FORK.md for the full maintenance notes.
EOF
}

# ── main ──────────────────────────────────────────────────────────────────

case "$MODE" in
  plan)
    print_plan
    ;;
  check-fleet)
    echo "version:  ${VERSION}"
    echo "required: ${FORK_REQUIRED_PLATFORMS}"
    echo "present:  $(list_present_platforms | tr '\n' ' ')"
    require_fleet_complete
    echo "ok — fleet set complete"
    cat "$DIST/checksums.txt" 2>/dev/null || true
    ;;
  run)
    [[ "$RUN_CONFIRMED" == true ]] || die "internal: --run required"
    build_all
    if [[ "$PUBLISH_CONFIRMED" == true ]]; then
      publish_release
    else
      echo
      echo "dry-build complete. To publish:"
      echo "  ./scripts/release-fork.sh --publish"
    fi
    ;;
  publish)
    [[ "$PUBLISH_CONFIRMED" == true ]] || die "publish requires the literal --publish flag"
    publish_release
    ;;
esac
