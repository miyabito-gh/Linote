# F: グローバルショートカットとクリップボード展開

担当: Opus。共通指示は `_common.md`。

## 担当パス
`src/platform/hotkey.rs`、`src/platform/clipboard.rs`、`src/platform/foreground.rs`（必要なら）、設定のキー表記の解析（`src/settings/` が `architecture.md` で定義されていればその中のキー割り当て部分）。

## 要件（`requirements.md` の「グローバルショートカットでクリップボードを展開」）
1. `RegisterHotKey` でグローバルショートカットを登録。他アプリにフォーカスがあってもエディタを前面に出す（`SetForegroundWindow` の制限への対処: `AllowSetForegroundWindow`、最小化からの復元、必要なら `AttachThreadInput` は慎重に）。
2. クリップボードの `CF_UNICODETEXT` を取得し、日本語・タブ・改行を保持して、未保存の新規タブ（クリップボード由来の文書種別）に取り込む。テキストがなければ短い状態表示（ステータスバー）を出す。エディタ内でも同じキーで動作（`WM_HOTKEY` はフォーカス中も届く）。
3. キー割り当ては設定で変更可能。キー表記（例 `Ctrl+Alt+V`）の解析と表示。既定値を提案する。登録失敗（競合）時は状態を伝え、再設定できる API。
4. クリップボード展開専用のアプリ内キーは設けない。`Ctrl+V` は通常の貼り付け。

## テスト
キー表記の解析は単体テスト。クリップボード取得とホットキー登録は Wine＋Xvfb で可能な範囲を自動テストし、前面化は Windows 実機確認項目として報告に列挙する。
