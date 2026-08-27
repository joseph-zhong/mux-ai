#!/usr/bin/env sh
# Install the latest muxai release binary on macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/joseph-zhong/mux-ai/main/install.sh | sh
#
# Env overrides:
#   MUXAI_VERSION      tag to install (default: latest release)
#   MUXAI_INSTALL_DIR  where the binary lands (default: ~/.local/bin)
set -eu

REPO="joseph-zhong/mux-ai"
INSTALL_DIR="${MUXAI_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${MUXAI_VERSION:-latest}"

die() { echo "install.sh: $*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "these are macOS binaries; on Linux build from source with 'cargo install --git https://github.com/$REPO'"

for dep in tmux git; do
  command -v "$dep" >/dev/null 2>&1 || echo "install.sh: warning: '$dep' is not on your PATH; muxai needs it at runtime" >&2
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The repo may be private, in which case the unauthenticated CDN download 404s and
# we fall back to the authenticated gh CLI.
if [ "$VERSION" = "latest" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1 || true)"
else
  tag="$VERSION"
fi

asset() { echo "muxai-$1-macos-universal.tar.gz"; }

if [ -n "${tag:-}" ] && curl -fsSL -o "$tmp/$(asset "$tag")" \
     "https://github.com/$REPO/releases/download/$tag/$(asset "$tag")" 2>/dev/null; then
  curl -fsSL -o "$tmp/$(asset "$tag").sha256" \
    "https://github.com/$REPO/releases/download/$tag/$(asset "$tag").sha256"
else
  command -v gh >/dev/null 2>&1 || die "could not download the release. The repo is private — install the GitHub CLI (brew install gh), run 'gh auth login', and try again."
  if [ "$VERSION" = "latest" ]; then
    tag="$(gh release view --repo "$REPO" --json tagName --jq .tagName)" \
      || die "no releases found on $REPO (or you lack access)"
  fi
  gh release download "$tag" --repo "$REPO" --dir "$tmp" \
    --pattern 'muxai-*-macos-universal.tar.gz*' \
    || die "could not download release $tag from $REPO"
fi

cd "$tmp"
shasum -a 256 -c "$(asset "$tag").sha256" >/dev/null || die "checksum mismatch on $(asset "$tag")"
tar -xzf "$(asset "$tag")"

mkdir -p "$INSTALL_DIR"
install -m 755 muxai "$INSTALL_DIR/muxai"
# Browser-downloaded tarballs carry a quarantine flag that Gatekeeper refuses to run.
xattr -d com.apple.quarantine "$INSTALL_DIR/muxai" 2>/dev/null || true

echo "installed muxai $tag to $INSTALL_DIR/muxai"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) "$INSTALL_DIR/muxai" --version 2>/dev/null || true ;;
  *) echo "note: $INSTALL_DIR is not on your PATH. Add this to your shell profile:"
     echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
