#!/usr/bin/env python3
"""配布物に同梱する第三者ライセンス文を集める（標準ライブラリのみ）。

Windows 向けの通常依存（ビルド専用・テスト専用を除く）の crate について、
crate に同梱されたライセンスファイルを連結し、Scintilla と Lexilla のライセンスを加える。
使い方: python3 scripts/collect-licenses.py OUTPUT_FILE
"""
import json
import os
import pathlib
import subprocess
import sys

TARGET = "x86_64-pc-windows-gnu"


def main() -> int:
    out = pathlib.Path(sys.argv[1])
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET]))
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    # 通常依存だけをたどる（build-dependencies / dev-dependencies は配布物に含まれない）。
    seen, stack = set(), [root]
    while stack:
        nid = stack.pop()
        for dep in nodes[nid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    parts = ["Linote に含まれる第三者ソフトウェアのライセンス\n" + "=" * 60 + "\n"]
    repo = pathlib.Path(__file__).resolve().parent.parent
    for name, rel in [("Scintilla 5.4.1", "third_party/scintilla/License.txt"),
                      ("Lexilla 5.3.0", "third_party/lexilla/License.txt")]:
        parts.append(f"\n## {name}\n\n" + (repo / rel).read_text(encoding="utf-8", errors="replace"))
    # 静的リンクした MinGW-w64 の実行時ライブラリ（CRT、winpthreads）と GCC の実行時ライブラリ
    # （libgcc、libstdc++。GCC Runtime Library Exception 付き）。ビルド環境の Debian/Ubuntu の著作権情報を写す。
    for name, path in [("MinGW-w64 runtime (CRT, winpthreads)", "/usr/share/doc/mingw-w64-common/copyright"),
                       ("GCC runtime (libgcc, libstdc++)", "/usr/share/doc/gcc-mingw-w64-base/copyright")]:
        # Relocated toolchains can supply the root containing usr/share/doc.
        f = pathlib.Path(os.environ.get("LINOTE_TOOLCHAIN_ROOT", "/")) / path.lstrip("/")
        if f.exists():
            parts.append(f"\n## {name}\n\n" + f.read_text(encoding="utf-8", errors="replace"))
        else:
            print(f"warning: {path} not found", file=sys.stderr)
    for pid in sorted(seen, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"])):
        p = pkgs[pid]
        d = pathlib.Path(p["manifest_path"]).parent
        files = sorted(f for f in d.iterdir()
                       if f.is_file() and f.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")))
        parts.append(f"\n## {p['name']} {p['version']} ({p.get('license') or 'license unknown'})\n")
        for f in files:
            parts.append(f"\n--- {f.name} ---\n" + f.read_text(encoding="utf-8", errors="replace"))
        if not files:
            parts.append("\n（crate にライセンスファイルが同梱されていません。上記の SPDX 表記に従います。）\n")
    out.write_text("".join(parts), encoding="utf-8", newline="\r\n")
    print(f"{len(seen)} crates -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
