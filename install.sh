#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
BIN="seavx"

# Only these platforms have CI-built binaries; everything else builds from source.
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) CDN="linux/x86_64" ;;
    Darwin-arm64) CDN="macos/arm64" ;;
    *) CDN="" ;;
esac

echo "=== Seavx Installer ==="
mkdir -p "$INSTALL_DIR"

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

installed=false

# 1. CDN: /latest redirects to an immutable sha-tagged build published by CI.
if [[ -n "$CDN" ]]; then
    echo "Downloading the newest build from cdn.seavx.com ..."
    if curl -fsSL --retry 2 "https://cdn.seavx.com/latest/${CDN}" -o "$TMP" \
        && install -m 755 "$TMP" "${INSTALL_DIR}/${BIN}"; then
        installed=true
    fi
fi

# 2. Source build, for platforms without a prebuilt binary.
if [[ "$installed" != true ]]; then
    echo "No prebuilt binary for $(uname -s) $(uname -m); building from source ..."
    if ! command -v cargo > /dev/null; then
        echo "error: cargo is not installed and no prebuilt binary is available. Install Rust: https://rustup.rs" >&2
        exit 1
    fi
    cargo build --release
    install -m 755 target/release/seavx "${INSTALL_DIR}/${BIN}"
fi

if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "note: ${INSTALL_DIR} is not in PATH. Add this to your shell config (~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo "Installed $("${INSTALL_DIR}/${BIN}" --version) -> ${INSTALL_DIR}/${BIN}"
