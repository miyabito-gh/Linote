# STATUS

- ブランチ: `wip/linote-recovery-preview`
- 基点: `762dcbe`（main）
- 今回の保全コミット:
  1. `616e2b5` 外部変更と復元の安全網を整備
  2. `22a675e` Markdown プレビューの表示と設定を拡充
  3. `f9ba739` メモ一覧と競合時の操作を調整
  4. `ba0ba27` タブ一覧と編集 UI を拡充
- 残件:
  - 今回の変更に対する動作確認の完了状況は不明。
  - リポジトリ外のトップフォルダ `spec.md` と `docs/spec.md` のハッシュ不一致は未解決。トップフォルダ側は変更していない。
- 2026-11-22 フォルダ統一: リポジトリを Linote\linote から Linote 直下へ引き上げ（linote.exe と test-data は Rust\_archive\Linote-stray-2026-11-22 へ退避、トップレベル spec.md は現状のまま残置）
