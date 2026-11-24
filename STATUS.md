# STATUS

## テスト失敗の調査（2026-11-25）

- 実行方法（Windows ホスト）: `C:\msys64\mingw64\bin` を PATH に置き、`.cargo/config.toml` の `runner = "wine64"` を環境変数 `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=C:/msys64/usr/bin/env.exe` で上書きして `cargo test --no-fail-fast`（`env.exe` は渡された EXE をそのまま起動するだけ）。
- 「検索パネルのスクロールバー検証」（`tests/search_panel.rs` の `panel_greps_opens_hits_and_disables_replace`、旧 232 行目の `WS_VSCROLL` の検証）: 作業前の HEAD `04fe8bf` で再現（`left: 0, right: 0`）。
  - 原因: テスト側のタイミング依存。検索・置換の用途では `set_status` は `WM_PANEL_FIT` を投函するだけで、状態欄の配置とスクロールバーの表示切り替え（`ShowScrollBar`）は投函の処理時に行われる。テストは直前の短い状態文（巨大モードの案内）の配置で `WS_VSCROLL` が外れた状態のまま、メッセージを処理せずにスタイルを読んでいた。実装は投函の処理後に正しくスクロールバーを出すので、実装の不具合ではない。
  - 修正: `set_status` の後にメッセージを処理しながら `WS_VSCROLL` が付くのを待つ（`pump`、上限 10 秒）よう変更。実装（`src/ui/find.rs`）は無変更。
- 「自動保存待ち」: 再現しなかった。GUI 自己テスト（`tests/ui_selftest.rs`、`selftest` の「メモの自動保存」の待ち合わせ）を単体で 3 回、全体の中で 4 回実行し、すべて成功。`tests/memo_store.rs`・`tests/recovery_process.rs` も全回成功。根拠がないため変更していない。
  - 参考: 全コアに空ループの負荷をかけた状態で GUI 自己テストを 1 回実行すると、別の待ち合わせ「プレビューの初期化の結果」が 10 秒の上限でタイムアウトした（自動保存の待ちではない）。自己テストの待ち合わせ（`pump_until`、10 秒）は高負荷時に落ちうる。
- 結果: 修正後、`cargo test --no-fail-fast` 全体を 3 回連続で実行し、すべて成功（15 のテスト対象すべて ok）。`tests/search_panel.rs` 単体も 3 回連続で成功。`cargo fmt --check`／`cargo clippy -D warnings` の既知失敗は対象外で未確認。

## main への取り込み（2026-11-22）

- `fix/build-scintilla-cstdint` を `main` へ取り込んだ。マージコミットは `7d618767e5df9d42d9bf9a9a111a9e8cff5aa137`。基点の `wip/linote-recovery-preview` の保全内容も含む。
- マージ後の `main` で、Windows ホストの `C:\msys64\mingw64\bin` を PATH に追加し、`WINDRES` 未指定で `cargo check` が成功した。

## 保全時の記録

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
- Windows ホストでのビルド（今回確認した経路）: rustup の `stable-x86_64-pc-windows-msvc`（既定）に `x86_64-pc-windows-gnu` ターゲットを追加し、MSYS2 mingw64（GCC 15.2、`C:\msys64\mingw64\bin` を PATH に置く）でビルドする。`.cargo/config.toml` の `runner = "wine64"` は Windows ホストでは使わない（`cargo run`／`cargo test` は別途実行方法が必要）。
- `WINDRES` の指定: 修正後は不要。build.rs が `WINDRES` → `x86_64-w64-mingw32-windres` → `windres` の順で探す。修正前は MSYS2 で `WINDRES=C:\msys64\mingw64\bin\windres.exe` の指定が必要だった。
- 2026-11-22 修正（ブランチ `fix/build-scintilla-cstdint`）:
  - `ce34cc9` GCC 15 系で同梱 Scintilla（CellBuffer.cxx）が `INT32_MAX`／`uint8_t` 未宣言で失敗する問題を、build.rs の `-include cstdint`（GCC/Clang のみ）で回避。`third_party/` は無変更。
  - `942d3f2` windres 探索に接頭辞なし `windres` へのフォールバックを追加。
- 検証: Windows ホスト（MSYS2 GCC 15.2、WINDRES 未指定）で `cargo check`・`cargo build` が成功。Linux クロス環境（Wine）での `cargo build`／`cargo test` と `scripts/check-imports.sh` は未実施。
- 既知の残課題（今回の修正対象外・HEAD から存在）: `cargo fmt --check`（`src/ui/app/commands.rs`）と `cargo clippy --all-targets -- -D warnings`（`thread_local` の const 化、`items_after_test_module` など `src/` 内）が失敗する。
