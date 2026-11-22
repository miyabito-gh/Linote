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
- 2026-11-22: 仕様書の正本を docs/spec.md に確定。プロジェクト直下に残っていた spec.md は docs/spec.md 初版（2026-11-14 コミット dae4d53）とバイト単位で一致する残骸だったため、Rust\_archive\Linote-stray-2026-11-22\ へ退避した。

## ビルド環境（2026-11-22 記録）

- 普段の経路: Linux（クラウド環境）から `x86_64-pc-windows-gnu` へクロスビルド（Debian/Ubuntu の MinGW-w64、テストは Wine）。`.cargo/config.toml` の既定ターゲットが GNU。MSVC は使っていない。
- Windows ホストでのビルド（今回確認した経路）: rustup の `stable-x86_64-pc-windows-msvc`（既定）に `x86_64-pc-windows-gnu` ターゲットを追加し、MSYS2 mingw64（GCC 15.2、`C:\msys64\mingw64in` を PATH に置く）でビルドする。`.cargo/config.toml` の `runner = "wine64"` は Windows ホストでは使わない（`cargo run`／`cargo test` は別途実行方法が必要）。
- `WINDRES` の指定: 修正後は不要。build.rs が `WINDRES` → `x86_64-w64-mingw32-windres` → `windres` の順で探す。修正前は MSYS2 で `WINDRES=C:\msys64\mingw64in\windres.exe` の指定が必要だった。
- 2026-11-22 修正（ブランチ `fix/build-scintilla-cstdint`）:
  - `ce34cc9` GCC 15 系で同梱 Scintilla（CellBuffer.cxx）が `INT32_MAX`／`uint8_t` 未宣言で失敗する問題を、build.rs の `-include cstdint`（GCC/Clang のみ）で回避。`third_party/` は無変更。
  - `942d3f2` windres 探索に接頭辞なし `windres` へのフォールバックを追加。
- 検証: Windows ホスト（MSYS2 GCC 15.2、WINDRES 未指定）で `cargo check`・`cargo build` が成功。Linux クロス環境（Wine）での `cargo build`／`cargo test` と `scripts/check-imports.sh` は未実施。
- 既知の残課題（今回の修正対象外・HEAD から存在）: `cargo fmt --check`（`src/ui/app/commands.rs`）と `cargo clippy --all-targets -- -D warnings`（`thread_local` の const 化、`items_after_test_module` など `src/` 内）が失敗する。
