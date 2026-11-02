#!/usr/bin/env bash
# Canonical bytes minisign signs. Format is SEAVX-UPDATE-v1; the Rust client
# reproduces it byte for byte (src/cli/update/manifest.rs::signed_bytes).
set -euo pipefail
[[ $# -eq 7 ]] || { echo "usage: $0 channel target sha key version built_at sha256" >&2; exit 2; }
printf 'SEAVX-UPDATE-v1\n'
printf 'channel=%s\n' "$1"
printf 'target=%s\n'  "$2"
printf 'sha=%s\n'     "$3"
printf 'key=%s\n'     "$4"
printf 'version=%s\n' "$5"
printf 'built_at=%s\n' "$6"
printf 'sha256=%s\n'  "$7"
