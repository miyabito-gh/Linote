# Linote — エージェント向け作業ガイド

Windows 11 x64 向けの軽量テキストエディタ（Rust）。要件の正本は `docs/requirements.md`、計画と進捗は `docs/PLAN.md`、共通の取り決めは `docs/architecture.md`。

## ビルドとテスト（Linux のクラウド環境から Windows 向けにクロスビルド）

- 既定ターゲットは `.cargo/config.toml` で `x86_64-pc-windows-gnu`。テストは Wine（`wine64`）で実行される。
- `cargo build` / `cargo build --release`
- `cargo test`（Windows バイナリを Wine で実行）
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `scripts/check-imports.sh target/x86_64-pc-windows-gnu/release/linote.exe` — EXE が Windows 標準以外の DLL を読み込まないことを確認する（単体 EXE 要件）。
- GUI を起動する確認は仮想ディスプレイが必要: `xvfb-run -a wine64 target/x86_64-pc-windows-gnu/debug/linote.exe`
- 画面に依存しない中核モジュールは Linux ネイティブでもテストできる: `cargo test --lib --target x86_64-unknown-linux-gnu`（Windows 専用コードは `#[cfg(windows)]` で分ける）。

## 規約

- 単体 EXE: 新しい依存を足すときは、EXE が DLL を必要としないことを `check-imports.sh` で確認する。`WebView2Loader.dll` は使わない（`docs/architecture.md` の WebView2 の節を参照）。
- 起動を軽くする: WebView2、MCP、メモ一覧、Grep などは初めて使うときに初期化する。起動経路に重い処理を足さない。
- 中核処理（文字コード、保存、メモ、復元、検索、巨大ファイル索引、コマンド定義）は `src/` 直下の GUI 非依存モジュールに置き、`windows` crate に依存させない。Win32 依存は `src/ui/` と `src/platform/` に閉じる。
- 長時間の処理は UI スレッドで行わない。結果を反映する前に文書 ID と版を照合する。
- データを失わない: 保存は一時ファイル経由の置換。黙って上書きしない。
- `third_party/` は外部ソース。直接修正したら `third_party/VENDORED.md` の「ローカル修正」に記録する。
- 文書・コメントは日本語でよい。識別子は英語。

## ブランチと配布

- 作業は `main` で行い、`main` に直接プッシュする（作業用ブランチやプルリクエストは作らない）。
- インストーラーは指示があったときだけ `scripts/package-installer.sh` で作る。`dist/` の成果物はコミットしない。

## 共有ファイル

`Cargo.toml`、`build.rs`、`src/main.rs`、`src/lib.rs`、コマンド登録表は統括エージェントが管理する。ワーカーは担当パスだけを編集し、必要な変更は報告で依頼する。

## 作業ルール（ショート版）

> 作業ルール（標準）：
> 1. 正本はプロジェクト直下の STATUS.md。開始前に読み、終了時に更新してコミットすること。
> 2. 開始時にパス・ブランチ・HEAD・既存差分・STATUS.md を確認して要約を報告すること。出所不明の差分は上書き・一括ステージしない。
> 3. このプロジェクトで書き込みを行うのはこのスレッドだけ。読み取り専用の並行作業は可。
> 4. `git add -A` 禁止。ステージはパス明示、コミットは目的別に分割。`git add -p` は使わない。依頼外の変更はしない。
> 5. 破壊的操作（reset / clean / 履歴書き換え / force push / 削除 / push）は明示指示がある時だけ。不要物は削除せず _archive へ退避する。
> 6. 秘密情報・個人設定らしきファイルは中身を読まず、コミットもしない。存在だけ報告し、指示を仰ぐこと。
> 7. エージェント交代の前には必ずコミット＋STATUS.md 更新を済ませること。
> 8. 報告は日本語で。「受け付けた状態」と「確定した状態」を区別し、未実施の検証は未実施と明記すること。問題と判断待ちは末尾にまとめること。
