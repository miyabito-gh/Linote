#!/usr/bin/env python3
"""固定テストデータ（tests/fixtures/*）を生成する。標準ライブラリのみ。

使い方: python3 tests/fixtures/gen-fixtures.py
生成物はコミット対象。内容の説明は tests/fixtures/README.md を参照。
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))

JP = "日本語のテキスト、半角ｶﾀｶﾅ、機種依存文字①Ⅱ㈱、ASCII abc 123"
SAMPLE = JP + "\n二行目：「ありがとう」\n三行目 end\n"


def w(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data)


def main():
    # 文字コード（改行は LF、末尾改行あり）
    w("utf8-nobom.txt", SAMPLE.encode("utf-8"))
    w("utf8-bom.txt", b"\xef\xbb\xbf" + SAMPLE.encode("utf-8"))
    w("utf16le-bom.txt", b"\xff\xfe" + SAMPLE.encode("utf-16-le"))
    w("utf16be-bom.txt", b"\xfe\xff" + SAMPLE.encode("utf-16-be"))
    w("cp932.txt", SAMPLE.encode("cp932"))

    # 改行（UTF-8 BOM なし、3 行）
    lines = ["one", "two", "three"]
    w("eol-crlf.txt", "\r\n".join(lines).encode() + b"\r\n")
    w("eol-lf.txt", "\n".join(lines).encode() + b"\n")
    w("eol-cr.txt", "\r".join(lines).encode() + b"\r")
    w("eol-mixed.txt", b"crlf\r\nlf\ncr\rlast\r\n")
    w("no-trailing-newline.txt", b"one\ntwo\nthree")
    w("empty.txt", b"")

    # CP932 に変換できない文字を含む UTF-8
    w("unmappable-cp932.txt", "絵文字 😀 と 𠮷野家 と通常の日本語\n".encode("utf-8"))


if __name__ == "__main__":
    main()
