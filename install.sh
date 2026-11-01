#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
BIN="seavx"

# Test seam, shared with the client (`SEAVX_UPDATE_BASE`): it moves the origin only.
BASE="${SEAVX_UPDATE_BASE:-https://cdn.seavx.com}"
# Trimmed once: every URL below is built with a single `/`.
BASE="${BASE%/}"

# TODO(update): paste the Task 0 release public key here, as in `PINNED_KEYS`
# (src/cli/update.rs). While it is empty the install refuses, because a checksum
# that arrives from an unsigned pointer authenticates nothing.
UPDATE_KEY=""

# Only these platforms have CI-built binaries; everything else builds from source.
# `BINARY` is the last segment of the signed route, i.e. `Target::binary`.
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) PLATFORM="linux-x86_64"; CDN="linux/x86_64"; BINARY="seavx-linux-x86_64" ;;
    Darwin-arm64) PLATFORM="macos-arm64"; CDN="macos/arm64"; BINARY="seavx-macos-arm64" ;;
    *) PLATFORM=""; CDN=""; BINARY="" ;;
esac

echo "=== Seavx Installer ==="
mkdir -p "$INSTALL_DIR"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; rm -f "${INSTALL_DIR}/${BIN}.new"' EXIT

installed=false

# `install` truncates the destination in place, so a short write would destroy the
# binary this rule must keep: stage beside it and rename, which is atomic.
place() {
    install -m 755 "$1" "${INSTALL_DIR}/${BIN}.new"
    mv -f "${INSTALL_DIR}/${BIN}.new" "${INSTALL_DIR}/${BIN}"
}

# `digest_matches` ignores case; `${x,,}` is bash 4 and macOS ships 3.2.
lower() { printf '%s' "$1" | tr 'A-F' 'a-f'; }

# 1. CDN: the pointer is signed and names an immutable build; nothing is replaced
#    until the signature, the channel, the target and the digest all hold.
if [[ -n "$CDN" ]]; then
    if [[ -z "$UPDATE_KEY" ]]; then
        echo "error: this installer pins no release key, so the signed pointer cannot be verified." >&2
        echo "       The release signing key is not published yet (SDD Task 0); refusing to install." >&2
        echo "       Build from source instead: cargo install --path ." >&2
        exit 1
    fi

    echo "Verifying the release pointer at ${BASE}/latest/${CDN}.json ..."
    curl -fsSL --retry 2 "${BASE}/latest/${CDN}.json" -o "$WORK/pointer.json"

    # CI writes the pointer as ONE line and the fixtures are pretty-printed, so the
    # key is matched wherever it sits; the first quoted value is the field.
    field() {
        grep -o "\"$1\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" "$WORK/pointer.json" \
            | head -n 1 \
            | sed 's/^[^:]*:[[:space:]]*"//; s/"$//' || true
    }
    CHANNEL="$(field channel)"
    TARGET="$(field target)"
    SHA="$(field sha)"
    KEY="$(field key)"
    VERSION="$(field version)"
    BUILT_AT="$(field built_at)"
    SHA256="$(field sha256)"
    SIGNATURE="$(field sig)"

    [[ "$CHANNEL" == stable ]] || { echo "error: the pointer is for the ${CHANNEL:-no} channel, not stable." >&2; exit 1; }
    [[ "$TARGET" == "$PLATFORM" ]] || { echo "error: the pointer is for target ${TARGET:-none}, not ${PLATFORM}." >&2; exit 1; }
    # The same shapes `Manifest::parse` enforces: a pointer the client rejects must
    # not be installable either, or the binary can never update itself.
    [[ "$SHA" =~ ^[0-9a-f]{7}$ ]] || { echo "error: sha: expected 7 lowercase hex characters, got \"${SHA}\"." >&2; exit 1; }
    [[ "$SHA256" =~ ^[0-9a-f]{64}$ ]] || { echo "error: sha256: expected 64 lowercase hex characters, got \"${SHA256}\"." >&2; exit 1; }
    [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] \
        || { echo "error: version: expected a SemVer version, got \"${VERSION}\"." >&2; exit 1; }
    [[ "$BUILT_AT" =~ ^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z$ ]] \
        || { echo "error: built_at: expected YYYY-MM-DDTHH:MM:SSZ, got \"${BUILT_AT}\"." >&2; exit 1; }
    # `key_matches_route`: the download path is exactly the one the digest names.
    [[ "$KEY" == "builds/${SHA}/${SHA256}/${BINARY}" ]] \
        || { echo "error: key: expected builds/${SHA}/${SHA256}/${BINARY}, got \"${KEY}\"." >&2; exit 1; }

    # The exact bytes minisign signed: `.github/scripts/pointer-manifest.sh` order,
    # which `manifest.rs::signed_bytes` reproduces byte for byte.
    {
        printf 'SEAVX-UPDATE-v1\n'
        printf 'channel=%s\n' "$CHANNEL"
        printf 'target=%s\n' "$TARGET"
        printf 'sha=%s\n' "$SHA"
        printf 'key=%s\n' "$KEY"
        printf 'version=%s\n' "$VERSION"
        printf 'built_at=%s\n' "$BUILT_AT"
        printf 'sha256=%s\n' "$SHA256"
    } > "$WORK/manifest"

    if command -v minisign > /dev/null; then
        # `sig` is one JSON line of `\n` escapes that already ends in a newline; a
        # second one here would make the POSIX and Windows sig files differ.
        printf '%b' "$SIGNATURE" > "$WORK/sig"
        minisign -V -m "$WORK/manifest" -x "$WORK/sig" -P "$UPDATE_KEY" > /dev/null
    else
        echo "warning: minisign is not installed; the checksum only detects transfer corruption, not a forged pointer or publisher"
    fi

    echo "Downloading ${VERSION} (${SHA}) ..."
    curl -fsSL --retry 2 "${BASE}/${KEY}" -o "$WORK/seavx"

    # macOS ships `shasum` rather than `sha256sum`; both print the same lowercase digest.
    if command -v sha256sum > /dev/null; then
        ACTUAL="$(sha256sum "$WORK/seavx" | cut -d' ' -f1)"
    else
        ACTUAL="$(shasum -a 256 "$WORK/seavx" | cut -d' ' -f1)"
    fi
    if [[ "$(lower "$ACTUAL")" != "$(lower "$SHA256")" ]]; then
        echo "error: the download does not match the published checksum: expected ${SHA256}, got ${ACTUAL}" >&2
        exit 1
    fi

    place "$WORK/seavx"
    installed=true
fi

# 2. Source build, for platforms without a prebuilt binary.
if [[ "$installed" != true ]]; then
    echo "No prebuilt binary for $(uname -s) $(uname -m); building from source ..."
    if ! command -v cargo > /dev/null; then
        echo "error: cargo is not installed and no prebuilt binary is available. Install Rust: https://rustup.rs" >&2
        exit 1
    fi
    cargo build --release
    place target/release/seavx
fi

if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    echo "note: ${INSTALL_DIR} is not in PATH. Add this to your shell config (~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo "Installed $("${INSTALL_DIR}/${BIN}" --version) -> ${INSTALL_DIR}/${BIN}"
