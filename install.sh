#!/usr/bin/env bash
set -e

echo "=== Installing Seavx AI Coding Agent ==="

# Check Rust / Cargo
if ! command -v cargo &> /dev/null; then
    echo "Error: Cargo/Rust is not installed. Please install Rust via https://rustup.rs"
    exit 1
fi

INSTALL_DIR="${HOME}/.local/bin"
mkdir -p "${INSTALL_DIR}"

echo "Building release binary..."
cargo build --release

echo "Installing binary to ${INSTALL_DIR}/seavx..."
install -m 755 target/release/seavx "${INSTALL_DIR}/seavx"

# Ensure ~/.local/bin is in PATH hint
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "Warning: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your ~/.bashrc or ~/.zshrc: export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo "=== Seavx successfully installed! ==="
echo "Run 'seavx' or 'seavx --help' from anywhere."
