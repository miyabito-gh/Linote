#!/bin/bash
# 配布用 ZIP を作る: dist/linote-<版>-win-x64.zip（linote.exe と第三者ライセンスのみ）。
set -euo pipefail
cd "$(dirname "$0")/.."
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
exe=target/x86_64-pc-windows-gnu/release/linote.exe
scripts/check-imports.sh "$exe"
stage=target/package/linote-$ver-win-x64
rm -rf "$stage" && mkdir -p "$stage" dist
cp "$exe" "$stage/linote.exe"
python3 scripts/collect-licenses.py "$stage/THIRD_PARTY_LICENSES.txt"
rm -f "dist/linote-$ver-win-x64.zip"
(cd "$(dirname "$stage")" && zip -q -r -X "../../dist/linote-$ver-win-x64.zip" "$(basename "$stage")")
sha256sum "dist/linote-$ver-win-x64.zip"
