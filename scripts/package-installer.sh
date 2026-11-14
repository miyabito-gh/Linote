#!/bin/bash
# NSIS 3.11+: produce a per-user Windows installer without changing portable ZIP packaging.
set -euo pipefail
cd "$(dirname "$0")/.."
repo=$PWD
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
command -v makensis >/dev/null || { echo "NSIS 3.11+ (makensis) is required" >&2; exit 1; }
cargo build --locked --release
exe=target/x86_64-pc-windows-gnu/release/linote.exe
scripts/check-imports.sh "$exe"
stage=$(mktemp -d "$repo/target/installer.XXXXXX")
trap 'rm -rf "$stage"' EXIT
mkdir -p dist
cp "$exe" "$stage/linote.exe"
python3 scripts/collect-licenses.py "$stage/THIRD_PARTY_LICENSES.txt"
output="$repo/dist/linote-$ver-win-x64-setup.exe"
makensis -V2 -DVERSION="$ver" -DSTAGE="$stage" -DOUTPUT="$output" scripts/installer.nsi
(cd dist && sha256sum "$(basename "$output")" > "$(basename "$output").sha256")
cat "$output.sha256"
