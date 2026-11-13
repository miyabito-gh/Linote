# 同梱している外部ソース

| 部品 | 版 | 取得元 | SHA-256（取得したアーカイブ） |
|---|---|---|---|
| Scintilla | 5.4.1（`scintilla/version.txt` = 541） | Ubuntu 24.04 ソースパッケージ `scite_5.4.1.orig.tar.gz`（内容は scintilla.org の配布物と同じ） | `bb56420118ac885c328ae689cc3626c237447856fd1903ce191904511878e529` |
| Lexilla | 5.3.0（`lexilla/version.txt` = 530） | 同上 | 同上 |

アーカイブの SHA-256 は、Ubuntu の `scite_5.4.1-1build2.dsc` に記載された値と一致することを確認済み。

## 削除したもの

- Scintilla: `cocoa/`、`gtk/`、`qt/`、`test/`、`doc/`、`scripts/`、`bin/`、`call/`、ビルド用ファイル、`win32/` の DLL・プロジェクト関連（`ScintillaDLL.cxx`、`*.rc`、`*.def`、`*.mak`、`*.vcxproj` など）、`src/SciTE.properties`。
- Lexilla: `lexers/` は下記の字句解析器以外すべて、`src/` の元のカタログ（`Lexilla.cxx`）とビルド用ファイル、`test/`、`doc/`、`scripts/`、`examples/`、`access/`、`bin/`。

## 組み込んでいる字句解析器（`lexilla/lexers/`）

| ファイル | 字句解析器名 | Linote での用途 |
|---|---|---|
| LexCPP.cxx | `cpp` | C/C++、Java（キーワードを Java 用に設定） |
| LexRust.cxx | `rust` | Rust |
| LexMarkdown.cxx | `markdown` | Markdown |
| LexJSON.cxx | `json` | JSON |
| LexYAML.cxx | `yaml` | YAML |
| LexProps.cxx | `props` | INI、TOML（Lexilla 5.3.0 に TOML 用の字句解析器がないため代用） |
| LexHTML.cxx | `hypertext`、`xml` | HTML、XML |
| LexPython.cxx | `python` | Python |
| LexPowerShell.cxx | `powershell` | PowerShell |
| LexBatch.cxx | `batch` | バッチファイル |
| LexSQL.cxx | `sql` | SQL |
| LexErrorList.cxx | `errorlist` | ログ（簡易表示） |
| LexNull.cxx | `null` | プレーンテキスト |

## Linote 側で追加したファイル

- `lexilla/src/LinoteLexers.cxx`: 上記の字句解析器だけを登録する最小のカタログ。`linote_create_lexer(name)` を公開する。

## ローカル修正

なし。

## 更新手順

1. 新しい版の Scintilla／Lexilla を入手し、SHA-256 を記録する。
2. 上記の「削除したもの」に従って不要なファイルを除き、`third_party/` を置き換える。
3. `LinoteLexers.cxx` を再適用し、`cargo build` と `scripts/check-imports.sh` で確認する。
4. この表の版とハッシュを更新する。

ライセンス: `scintilla/License.txt`、`lexilla/License.txt`（配布物にも同梱する）。
