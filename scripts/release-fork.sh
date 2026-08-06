#!/usr/bin/env bash
# Ship a personal-fork prerelease for mise github: + optional Nix overlays.
# Shape: https://gist.github.com/alleneubank/bf7d25542a49b136671db0e4bb65226d
#
# Dry-run (default): build + package only.
# Publish:  scripts/release-fork.sh --publish
set -euo pipefail

REPO="${FORK_REPO:-alleneubank/grok-build}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PUBLISH=false
[[ "${1:-}" == "--publish" ]] && PUBLISH=true

if [[ -n "$(git status --porcelain)" ]]; then
  echo "dirty tree — commit or stash before release (reproducible tag)" >&2
  git status --short >&2
  exit 1
fi

# Base = cargo package version (upstream monorepo ship number for this tree).
BASE="$(grep -m1 '^version' crates/codegen/xai-grok-pager-bin/Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
SHORT9="$(git rev-parse --short=9 HEAD)"
DATE_UTC="$(date -u +%Y%m%d)"
VERSION="${BASE}-fork.${DATE_UTC}.g${SHORT9}"
TAG="v${VERSION}"

# Platform of this host (dogfood-first; add cross-targets later).
UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"
case "${UNAME_S}-${UNAME_M}" in
  Darwin-arm64)  OS_ARCH="darwin.arm64" ;;
  Darwin-x86_64) OS_ARCH="darwin.x86_64" ;;
  Linux-x86_64)  OS_ARCH="linux.x86_64" ;;
  Linux-aarch64) OS_ARCH="linux.arm64" ;;
  *)
    echo "unsupported platform: ${UNAME_S}-${UNAME_M}" >&2
    exit 1
    ;;
esac

DIST="$ROOT/dist/fork-release"
rm -rf "$DIST"
mkdir -p "$DIST"

echo "Building grok ${VERSION} for ${OS_ARCH}..."
# GROK_VERSION stamps the user-facing version string (see xai-grok-version +
# pager-bin build.rs). Autoupdater still points at xAI channels — consumers
# should set [cli] auto_update = false or GROK_DISABLE_AUTOUPDATER=1.
export GROK_VERSION="${VERSION}"
cargo build -p xai-grok-pager-bin --release

BIN_SRC="$ROOT/target/release/xai-grok-pager"
[[ -x "$BIN_SRC" ]] || { echo "missing binary: $BIN_SRC" >&2; exit 1; }
cp -f "$BIN_SRC" "$DIST/grok"
chmod +x "$DIST/grok"

# Gist-shaped name: <tool>.<version>.<os>.<arch>.tar.gz, binary at archive root.
ARCHIVE="grok.${VERSION}.${OS_ARCH}.tar.gz"
tar -C "$DIST" -czf "$DIST/$ARCHIVE" grok

if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$DIST" && sha256sum "$ARCHIVE" > checksums.txt )
else
  ( cd "$DIST" && shasum -a 256 "$ARCHIVE" | awk '{print $1"  "$2}' > checksums.txt )
fi

echo "---"
"$DIST/grok" --version || true
echo "archive: $DIST/$ARCHIVE"
cat "$DIST/checksums.txt"
echo "tag: $TAG  repo: $REPO  prerelease: yes"

if [[ "$PUBLISH" != true ]]; then
  echo
  echo "dry run complete — re-run with --publish to tag + gh release"
  exit 0
fi

# BOUNDARY: deliberate publish only (tag + GitHub release).
echo
echo "Publishing prerelease ${TAG} to github.com/${REPO}"
echo "  asset: ${ARCHIVE}"
echo "  checksums: checksums.txt"

git tag -a "$TAG" -m "fork release ${VERSION}"
# Prefer HTTPS+token when SSH is rewritten/broken for this host.
TOKEN="$(gh auth token 2>/dev/null || true)"
if [[ -n "$TOKEN" ]]; then
  git push "https://x-access-token:${TOKEN}@github.com/${REPO}.git" "refs/tags/${TAG}"
else
  git push origin "refs/tags/${TAG}"
fi

gh release create "$TAG" \
  --repo "$REPO" \
  --prerelease \
  --title "$VERSION" \
  --notes "Personal fork of xai-org/grok-build.

Includes:
- lifecycle PermissionRequest for interactive tool approvals
- plugin hooks loaded at session cold start (no /hooks reload required)

Version stamp: \`${VERSION}\`

**Auto-update:** this binary still embeds the official updater client. Pin
\`[cli] auto_update = false\` or export \`GROK_DISABLE_AUTOUPDATER=1\` so it does
not try to replace itself with an xAI-channel build.

Install (mise):
\`\`\`toml
\"github:alleneubank/grok-build\" = { version = \"${VERSION}\", exe = \"grok\" }
\`\`\`
" \
  "$DIST/$ARCHIVE" \
  "$DIST/checksums.txt"

echo "published: https://github.com/${REPO}/releases/tag/${TAG}"
