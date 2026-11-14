# Linote

Windows 11 x64 向けの軽量テキストエディタ（Rust）。日本語の編集、検索・Grep、巨大ファイルの閲覧、一時メモ、Markdown プレビューを備え、EXE 1 つで動きます。

- 要件: [docs/requirements.md](docs/requirements.md)
- 仕様（実装どおりの動作）: [docs/spec.md](docs/spec.md)
- 設計: [docs/architecture.md](docs/architecture.md)
- 計画と進捗: [docs/PLAN.md](docs/PLAN.md)
- Windows 実機での確認項目: [docs/verification/](docs/verification/)

## 使い方

ZIP を展開して `linote.exe` を起動します。設定・メモ・復元データは `%LOCALAPPDATA%\Linote` に作られます。

| 操作 | キー |
|---|---|
| 新規／開く／保存／名前を付けて保存 | Ctrl+N ／ Ctrl+O ／ Ctrl+S ／ Ctrl+Shift+S |
| 行へ移動 | Ctrl+G |
| 検索／置換／次・前を検索 | Ctrl+F ／ Ctrl+H ／ F3・Shift+F3 |
| Grep | Ctrl+Shift+F |
| 一時メモの一覧 | Ctrl+Shift+M |
| Markdown プレビュー（右ペイン） | Ctrl+Shift+P |
| クリップボードを新しいタブに展開（グローバル） | Ctrl+Alt+V |
| 矩形選択 | Alt+ドラッグ、Alt+Shift+矢印 |

本文のフォントは「表示 > フォント...」、または「設定 > 表示 > 本文フォント」で選択できます。

ステータスバーは「表示 > ステータスバーを表示」で表示／非表示を切り替えられます。

キーの割り当ては `%LOCALAPPDATA%\Linote\settings.json` の `keys` で変えられます。

## ビルド

Linux から Windows 向けにクロスビルドします（MinGW-w64、テストは Wine）。詳しくは [CLAUDE.md](CLAUDE.md)。

```bash
cargo build --release
```

配布用 ZIP（`dist/linote-<版>-win-x64.zip`）:

```bash
scripts/package.sh
```

Windows 用インストーラー（NSIS 3.11 以降が必要）:

```bash
scripts/package-installer.sh
```

`dist/linote-<版>-win-x64-setup.exe` を作成します。管理者権限なしで
`%LOCALAPPDATA%\Programs\Linote` に導入し、スタートメニューへ登録します。
Windows の「インストールされているアプリ」から削除できます。
アンインストール後も `%LOCALAPPDATA%\Linote` の設定・メモ・復元データは保持します。
インストーラーはコード署名していません。
ツールチェーンを別のルートへ展開した環境では、ライセンス収集用に
`LINOTE_TOOLCHAIN_ROOT` へそのルートを指定します（通常は不要）。

## 第三者ソフトウェア

Scintilla 5.4.1 と Lexilla 5.3.0 を同梱しています（[third_party/VENDORED.md](third_party/VENDORED.md)）。配布物には `THIRD_PARTY_LICENSES.txt` を含めます。
