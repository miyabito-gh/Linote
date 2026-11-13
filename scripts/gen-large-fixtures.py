#!/usr/bin/env python3
"""巨大テストデータ生成スクリプト（Python 3 標準ライブラリのみ）。

生成物（既定の出力先は target/fixtures-large/、git 管理外）:
  lines.txt     短い行が大量。1 行 80 バイト（改行 LF を含む）固定。
                書式: "<行番号 10 桁ゼロ埋め> <英数字の埋め草>\\n"
                よって行 N（1 始まり）の先頭バイトオフセットは (N-1)*80。
  longline.txt  先頭に長大な 1 行（改行なし）＋末尾に短い数行。
  mixed.txt     UTF-8 の日本語・ASCII 混在。行長は不定、LF 区切り。
  manifest.json 各ファイルのサイズ、行数と、埋め込んだ検索用文字列の位置。

乱数は固定シード（--seed、既定 20240601）で再現可能。
検索検証用の文字列 "LINOTE_NEEDLE_<種別>_<番号>" を既知の位置に埋め込み、
バイトオフセット（0 始まり）と行番号（1 始まり）を manifest.json に記録する。

例:
  python3 scripts/gen-large-fixtures.py                      # 既定サイズ（約 1.4 GiB）
  python3 scripts/gen-large-fixtures.py --out /tmp/f --lines-size 1MiB \\
      --longline-size 1MiB --mixed-size 1MiB                 # 動作確認用
"""
import argparse
import json
import os
import random
import re
import sys

ASCII_FILL = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
JP_WORDS = [
    "日本語", "テキスト", "エディタ", "検索", "置換", "巨大", "ファイル", "行番号",
    "文字コード", "保存", "復元", "タブ", "メモ", "ログ", "確認", "処理",
    "ｶﾀｶﾅ", "①②③", "㈱", "ＡＢＣ", "全角１２３", "、", "。", "「", "」",
]
NEEDLE_FRACTIONS = (0.0, 0.25, 0.5, 0.75, 0.99)
LINE_BYTES = 80


def parse_size(text):
    m = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([KMGkmg]?)(?:i?[Bb])?\s*", text)
    if not m:
        raise argparse.ArgumentTypeError("サイズの形式が不正です: %r（例: 1GiB, 256MiB, 1048576）" % text)
    mult = {"": 1, "k": 1 << 10, "m": 1 << 20, "g": 1 << 30}[m.group(2).lower()]
    n = int(float(m.group(1)) * mult)
    if n <= 0:
        raise argparse.ArgumentTypeError("サイズは 1 以上にしてください")
    return n


def needle(kind, i):
    return "LINOTE_NEEDLE_%s_%d" % (kind, i)


def gen_lines(path, size, rng):
    """80 バイト固定長の行。行 N の先頭オフセットは (N-1)*80。"""
    total = max(size // LINE_BYTES, 1)
    pool_len = LINE_BYTES - 11 - 1  # 行番号 10 桁 + 空白 + 本文 + LF
    pool = ["".join(rng.choices(ASCII_FILL, k=pool_len)) for _ in range(4096)]
    # 検索ヒット位置: 総行数に対する比率の行。本文先頭に埋め込む。
    targets = {}
    for i, frac in enumerate(NEEDLE_FRACTIONS):
        ln = min(max(int(total * frac) + 1, 1), total)
        targets.setdefault(ln, i)
    hits = []
    batch = 100000
    with open(path, "wb") as f:
        n = 1
        while n <= total:
            end = min(n + batch, total + 1)
            out = []
            for ln in range(n, end):
                body = pool[rng.randrange(4096)]
                if ln in targets:
                    nd = needle("LINES", targets[ln])
                    body = nd + body[len(nd):]
                    hits.append({"needle": nd, "line": ln,
                                 "byte_offset": (ln - 1) * LINE_BYTES + 11})
                out.append("%010d %s\n" % (ln, body))
            f.write("".join(out).encode("ascii"))
            n = end
    hits.sort(key=lambda h: h["line"])
    return {"file": os.path.basename(path), "bytes": total * LINE_BYTES,
            "lines": total, "line_bytes": LINE_BYTES, "hits": hits}


def gen_longline(path, size, rng):
    """先頭に size バイトの 1 行（改行なし）、続けて短い 4 行。"""
    table = bytes(ord(ASCII_FILL[i % len(ASCII_FILL)]) for i in range(256))
    chunk = 1 << 20
    offsets = {}
    for i, frac in enumerate(NEEDLE_FRACTIONS[1:-1]):
        offsets[int(size * frac)] = i
    hits = []
    pos = 0
    with open(path, "wb") as f:
        while pos < size:
            n = min(chunk, size - pos)
            buf = bytearray(rng.randbytes(n).translate(table))
            for off, i in offsets.items():
                nd = needle("LONGLINE", i).encode()
                if pos <= off and off + len(nd) <= pos + n:
                    buf[off - pos:off - pos + len(nd)] = nd
                    hits.append({"needle": nd.decode(), "line": 1, "byte_offset": off})
            f.write(buf)
            pos += n
        tail = ["\n", "second line\n", "third line\n",
                needle("LONGLINE", 9) + "\n", "last line\n"]
        off = size
        for k, s in enumerate(tail):
            if s.startswith("LINOTE_NEEDLE"):
                hits.append({"needle": needle("LONGLINE", 9), "line": k + 1,
                             "byte_offset": off})
            f.write(s.encode())
            off += len(s)
    # 行頭の改行 "\n" は 1 行目の終端。tail[0] が 1 行目を閉じるため行番号は k+1。
    hits.sort(key=lambda h: h["byte_offset"])
    return {"file": os.path.basename(path), "bytes": off, "lines": len(tail),
            "first_line_bytes": size, "hits": hits}


def gen_mixed(path, size, rng):
    """日本語と ASCII の混在。行長は 10〜300 文字程度で不定。"""
    targets = [int(size * f) for f in NEEDLE_FRACTIONS]
    done = [False] * len(targets)
    hits = []
    written = 0
    ln = 0
    with open(path, "wb") as f:
        while written < size:
            ln += 1
            parts = []
            for _ in range(rng.randint(3, 60)):
                r = rng.random()
                if r < 0.65:
                    parts.append(rng.choice(JP_WORDS))
                elif r < 0.85:
                    parts.append("".join(rng.choices(ASCII_FILL, k=rng.randint(2, 12))))
                else:
                    parts.append(str(rng.randint(0, 99999)))
                parts.append(" " if rng.random() < 0.4 else "")
            line = "%08d " % ln + "".join(parts)
            for i, t in enumerate(targets):
                if not done[i] and written >= t:
                    nd = needle("MIXED", i)
                    prefix = "%08d " % ln
                    line = prefix + "検索" + nd + "終" + line[len(prefix):]
                    # バイトオフセットは needle 本体の先頭
                    off = written + len((prefix + "検索").encode("utf-8"))
                    hits.append({"needle": nd, "line": ln, "byte_offset": off})
                    done[i] = True
                    break
            data = (line + "\n").encode("utf-8")
            f.write(data)
            written += len(data)
    return {"file": os.path.basename(path), "bytes": written, "lines": ln, "hits": hits}


def main():
    ap = argparse.ArgumentParser(
        description="巨大テストデータを生成する（固定シードで再現可能）。",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    ap.add_argument("--out", default=os.path.join("target", "fixtures-large"),
                    help="出力先ディレクトリ（なければ作成）")
    ap.add_argument("--lines-size", type=parse_size, default="1GiB",
                    help="短い行が大量のファイル lines.txt のサイズ（80 バイトの倍数に切り捨て）")
    ap.add_argument("--longline-size", type=parse_size, default="256MiB",
                    help="長大な単一行ファイル longline.txt の 1 行目のサイズ")
    ap.add_argument("--mixed-size", type=parse_size, default="100MiB",
                    help="日本語混在ファイル mixed.txt のおおよそのサイズ")
    ap.add_argument("--only", nargs="+", choices=["lines", "longline", "mixed"],
                    help="指定した種類だけ生成する")
    ap.add_argument("--seed", type=int, default=20240601, help="乱数シード")
    args = ap.parse_args()
    for name in ("lines_size", "longline_size", "mixed_size"):
        v = getattr(args, name)
        if isinstance(v, str):
            setattr(args, name, parse_size(v))

    os.makedirs(args.out, exist_ok=True)
    want = set(args.only or ["lines", "longline", "mixed"])
    manifest = {"seed": args.seed, "offset_base": 0, "line_base": 1, "files": {}}
    jobs = [("lines", gen_lines, args.lines_size, "lines.txt"),
            ("longline", gen_longline, args.longline_size, "longline.txt"),
            ("mixed", gen_mixed, args.mixed_size, "mixed.txt")]
    for kind, fn, size, fname in jobs:
        if kind not in want:
            continue
        path = os.path.join(args.out, fname)
        print("生成中: %s (%d bytes)" % (path, size), file=sys.stderr)
        manifest["files"][fname] = fn(path, size, random.Random(args.seed))

    mpath = os.path.join(args.out, "manifest.json")
    if os.path.exists(mpath) and args.only:
        try:
            with open(mpath, encoding="utf-8") as f:
                old = json.load(f)
            old["files"].update(manifest["files"])
            manifest = old
        except (OSError, ValueError, KeyError):
            pass
    with open(mpath, "w", encoding="utf-8") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=2)
    print(json.dumps(manifest, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
