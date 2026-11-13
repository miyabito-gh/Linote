# B: 画面の骨格（ウィンドウ、タブ、ステータスバー、テーマ、Scintilla 組み込み）

担当: Opus。共通指示は `_common.md`。

## 担当パス
`src/ui/`（メインウィンドウ、タブバー、ステータスバー、テーマ、Scintilla ラッパー）、`src/platform/` のうち UI 共通部分（DPI、テーマ検出）。`src/main.rs` は統括が管理するので、`ui::run()` のような入口を用意し、`main.rs` の変更案を報告する（スモーク用の現 `main.rs` は統括が置き換える）。

## 要件
1. メインウィンドウ（Win32、`windows` crate）。上部に細いタブバー（独自描画）、中央に Scintilla、下部にステータスバー（独自描画: 行・列、文字コード、改行形式、読み取り専用表示、状態メッセージ）。右ペイン（プレビュー用の領域）を後から差し込めるレイアウト。
2. タブ: 追加・閉じる・切り替え・未保存の小さな丸・選択中タブの控えめな強調。中クリックで閉じる。
3. Scintilla ラッパー（`ui::scintilla`）: `SendMessageW` を型付きのメソッドに包む（文字列の取得・設定、選択、矩形選択、行ジャンプ、行番号の余白、Undo 単位のまとめ `SCI_BEGINUNDOACTION` 等、変更通知）。**文書ごとに Scintilla のドキュメント（`SCI_CREATEDOCUMENT`／`SCI_SETDOCPOINTER`）を切り替え**、1 つのエディタウィンドウで複数タブを扱う。Rust 側に全文コピーを常時持たない。
4. 版: `SCN_MODIFIED` で文書の版（`architecture.md` の `Version`）を進める仕組み。
5. 構文着色: 拡張子から Lexilla の字句解析器を選ぶ（`linote_create_lexer`、`SCI_SETILEXER`）。対象言語は `third_party/VENDORED.md`。Java は `cpp` にJava のキーワード。テーマに合わせたスタイル。巨大ファイルは対象外。
6. テーマ: ダーク／ライト、Windows の設定（`AppsUseLightTheme` レジストリ、`WM_SETTINGCHANGE`）に連動。ダーク初案は本文背景 `#191D24`、上部バー `#212731`、本文 `#E3E8EF`、アクセント `#91ABD0`。選択とフォーカスが見分けやすいように調整。タイトルバーのダークモード（`DwmSetWindowAttribute` の `DWMWA_USE_IMMERSIVE_DARK_MODE`）。テーマ定義は共通の型にし、後でプレビューにも渡せるようにする。
7. DPI: Per-Monitor V2 対応（`WM_DPICHANGED`）。マニフェストが必要なら、その内容と埋め込み方法（`build.rs` でリソースをリンク）を報告で提案する。
8. 日本語 IME: Scintilla の IME 対応（`SCI_SETIMEINTERACTION` など）を有効にする。
9. 起動を軽く: 起動時にやることは最小限に。

## テスト
- Wine＋Xvfb で起動し、ウィンドウ生成・タブ追加・切り替え・閉じる・テキスト設定と取得が通る自動テスト（`#[cfg(windows)]` の統合テスト、または `--self-test` のような隠しオプションで EXE を起動して終了コードで判定する方式）。
- テーマ色の計算など純粋な部分は単体テスト。
