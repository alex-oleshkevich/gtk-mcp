#!/bin/sh
# Install the gtk-mcp broker from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/alex-oleshkevich/gtk-mcp/main/install.sh | sh
#
# Environment:
#   GTK_MCP_VERSION  release tag to install, e.g. v0.1.0 (default: latest release)
#   BIN_DIR          where the gtk-mcp binary goes (default: ~/.local/bin)
#   LIB_DIR          where the agent C library goes (default: ~/.local/lib)
set -eu

REPO="alex-oleshkevich/gtk-mcp"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
LIB_DIR="${LIB_DIR:-$HOME/.local/lib}"

die() { echo "install.sh: $*" >&2; exit 1; }

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        die "need curl or wget"
    fi
}

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) die "unsupported OS: $(uname -s)" ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

version="${GTK_MCP_VERSION:-}"
if [ -z "$version" ]; then
    fetch "https://api.github.com/repos/$REPO/releases/latest" "$tmp/latest.json"
    version="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp/latest.json" | head -n 1)"
    [ -n "$version" ] || die "could not find the latest release of $REPO"
fi

archive="gtk-mcp-$version-$os-$arch.tar.gz"
echo "Downloading $archive"
fetch "https://github.com/$REPO/releases/download/$version/$archive" "$tmp/$archive" \
    || die "download failed: no $os-$arch build in release $version?"

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/gtk-mcp" ] || die "archive does not contain the gtk-mcp binary"

mkdir -p "$BIN_DIR" "$LIB_DIR"
install -m 755 "$tmp/gtk-mcp" "$BIN_DIR/gtk-mcp"
echo "Installed $BIN_DIR/gtk-mcp ($version)"

for lib in libgtk_mcp_agent_capi.so libgtk_mcp_agent_capi.dylib; do
    if [ -f "$tmp/$lib" ]; then
        install -m 644 "$tmp/$lib" "$LIB_DIR/$lib"
        echo "Installed $LIB_DIR/$lib (agent C library, only needed for C, Python and GJS apps)"
    fi
done

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: $BIN_DIR is not on your PATH. Add it, or use the full path in your MCP client config." ;;
esac
