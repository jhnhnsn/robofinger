#!/bin/sh
# robofinger installer for private-repo builds.
#
# The cargo-dist installer at /releases/latest/download/robofinger-installer.sh
# needs the repo to be public. Until then this does the same job through `gh`,
# which carries your GitHub credentials.
#
#   gh auth login          # once per machine
#   ./install.sh           # or: curl -sSL <raw url> | sh
#
# Env:
#   ROBOFINGER_VERSION   tag to install (default: latest release)
#   ROBOFINGER_BIN_DIR   install target (default: ~/.local/bin)

set -eu

REPO="jhnhnsn/robofinger"
BIN_DIR="${ROBOFINGER_BIN_DIR:-$HOME/.local/bin}"

die() { echo "robofinger: $*" >&2; exit 1; }

command -v gh >/dev/null 2>&1 || die "gh not found — install GitHub CLI: https://cli.github.com"
gh auth status >/dev/null 2>&1 || die "not logged in — run: gh auth login"

# Map uname to the cargo-dist target triple.
os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
  Linux-x86_64)          target="x86_64-unknown-linux-gnu";  ext="tar.xz" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu"; ext="tar.xz" ;;
  Darwin-arm64)          target="aarch64-apple-darwin";      ext="tar.xz" ;;
  Darwin-x86_64)         target="x86_64-apple-darwin";       ext="tar.xz" ;;
  *) die "unsupported platform: $os $arch (build from source: cargo build --release)" ;;
esac

version="${ROBOFINGER_VERSION:-}"
if [ -z "$version" ]; then
  version=$(gh release view --repo "$REPO" --json tagName --jq .tagName) \
    || die "no releases found in $REPO"
fi

asset="robofinger-$target.$ext"
tmp=$(mktemp -d)
# Clean up even on failure — the archive and extracted tree are throwaway.
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "robofinger $version ($target)"
gh release download "$version" --repo "$REPO" --dir "$tmp" \
  --pattern "$asset" --pattern "$asset.sha256" \
  || die "download failed — does $version have an asset for $target?"

# Verify before trusting the binary. sha256sum on Linux, shasum on macOS.
( cd "$tmp"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$asset.sha256" >/dev/null 2>&1 || die "checksum mismatch"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$asset.sha256" >/dev/null 2>&1 || die "checksum mismatch"
  else
    echo "  (no sha256 tool found — skipping verification)" >&2
  fi
) || exit 1
echo "  checksum ok"

tar -xf "$tmp/$asset" -C "$tmp"
bin=$(find "$tmp" -type f -name robofinger -perm -u+x | head -1)
[ -n "$bin" ] || die "no robofinger binary in $asset"

mkdir -p "$BIN_DIR"
install -m 755 "$bin" "$BIN_DIR/robofinger" 2>/dev/null \
  || { cp "$bin" "$BIN_DIR/robofinger" && chmod 755 "$BIN_DIR/robofinger"; }
echo "  installed to $BIN_DIR/robofinger"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "
  NOTE: $BIN_DIR is not on your PATH. Add it:
    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.profile && . ~/.profile" ;;
esac

echo "
Next:
  robofinger init --url <relay url> --ns <namespace>
  robofinger id       # share this line with peers"
