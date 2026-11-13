# テスト用固定データ

`python3 tests/fixtures/gen-fixtures.py` で生成する（標準ライブラリのみ）。生成物もコミットする。`.gitattributes` の `tests/fixtures/** -text` により改行は変換されない。巨大ファイルは `scripts/gen-large-fixtures.py` で別途生成する（リポジトリには入れない）。

共通の本文（UTF-8 換算）:

```
日本語のテキスト、半角ｶﾀｶﾅ、機種依存文字①Ⅱ㈱、ASCII abc 123
二行目：「ありがとう」
三行目 end
```

## 文字コード

本文は上記 3 行、改行は LF、末尾改行あり。

| ファイル | 文字コード | BOM | 改行 | 備考 |
| --- | --- | --- | --- | --- |
| `utf8-nobom.txt` | UTF-8 | なし | LF | |
| `utf8-bom.txt` | UTF-8 | あり（EF BB BF） | LF | |
| `utf16le-bom.txt` | UTF-16LE | あり（FF FE） | LF | |
| `utf16be-bom.txt` | UTF-16BE | あり（FE FF） | LF | |
| `cp932.txt` | CP932（Shift_JIS） | なし | LF | 半角カナと機種依存文字 ①Ⅱ㈱ を含む。デコードすると `utf8-nobom.txt` と同一の文字列 |

## 改行

UTF-8、BOM なし。

| ファイル | 内容 | 期待する改行形式 |
| --- | --- | --- |
| `eol-crlf.txt` | `one` `two` `three`、各行末 CRLF | CRLF（3 行、末尾改行あり） |
| `eol-lf.txt` | 同上、各行末 LF | LF |
| `eol-cr.txt` | 同上、各行末 CR | CR |
| `eol-mixed.txt` | `crlf`(CRLF) `lf`(LF) `cr`(CR) `last`(CRLF) | 混在（4 行） |
| `no-trailing-newline.txt` | `one\ntwo\nthree`（最後に改行なし） | LF、末尾改行なし、3 行 |
| `empty.txt` | 0 バイト | 空（文字コードは既定の UTF-8 扱い、改行は既定） |

## 変換不能

| ファイル | 内容 | 期待 |
| --- | --- | --- |
| `unmappable-cp932.txt` | UTF-8（BOM なし、LF）。`絵文字 😀 と 𠮷野家 と通常の日本語` | CP932 では 😀（U+1F600）と 𠮷（U+20BB7）が変換できない。保存前に警告が出ること |
