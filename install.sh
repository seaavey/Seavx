#!/usr/bin/env bash
set -e

REPO="seaavey/Seavx"
INSTALL_DIR="${HOME}/.local/bin"
mkdir -p "${INSTALL_DIR}"

echo "=== Seavx One-Line Installer ==="

# Detect OS and Architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

BINARY_NAME=""
if [[ "$OS" == "Linux" && "$ARCH" == "x86_64" ]]; then
    BINARY_NAME="seavx-linux-x86_64"
elif [[ "$OS" == "Darwin" && "$ARCH" == "arm64" ]]; then
    BINARY_NAME="seavx-macos-arm64"
fi

# 1. Try to download pre-built binary from latest GitHub release
DOWNLOAD_SUCCESS=false
if [[ -n "$BINARY_NAME" ]]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${BINARY_NAME}"
    echo "Attempting to download pre-built binary for ${OS} ${ARCH}..."
    if curl -sL --fail "$DOWNLOAD_URL" -o "${INSTALL_DIR}/seavx"; then
        chmod +x "${INSTALL_DIR}/seavx"
        DOWNLOAD_SUCCESS=true
        echo "Successfully downloaded pre-built binary to ${INSTALL_DIR}/seavx"
    fi
fi

# 2. Fallback to local cargo build if pre-built binary is not found or fails
if [[ "$DOWNLOAD_SUCCESS" != "true" ]]; then
    echo "Pre-built binary not available. Building from source via Cargo..."
    if ! command -v cargo &> /dev/null; then
        echo "Error: Cargo is not installed and prebuilt binary is not available. Please install Rust: https://rustup.rs"
        exit 1
    fi
    cargo build --release
    install -m 755 target/release/seavx "${INSTALL_DIR}/seavx"
fi

# Ensure ~/.local/bin is in PATH
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "Notice: ${INSTALL_DIR} is not in your current PATH."
    echo "Add this to your shell config (~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo "=== Installation Complete! ==="
echo "You can now run 'seavx' or 'seavx --help'."
