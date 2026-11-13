# 全タスク共通の指示

- まず `CLAUDE.md`、`docs/requirements.md`、`docs/architecture.md`、`docs/PLAN.md` を読む。共通の型・トレイトは `architecture.md` と `src/core`、`src/document`、`src/command` の定義に従う。
- **担当パス以外は編集しない。** 共有ファイル（`Cargo.toml`、`build.rs`、`src/main.rs`、`src/lib.rs`、コマンド登録表）の変更が必要なら、変更内容を報告に書く（統括が反映する）。ただし依存 crate の追加が必須で作業が進まない場合に限り、`Cargo.toml` の `[dependencies]` への追加だけは行ってよい（報告に明記）。
- 取り決め（`architecture.md` や共通型）の変更が必要だと判断したら、勝手に変えずに報告で提案する。軽微な追加（新しいエラー種別など）は自分のモジュール内で吸収する。
- 品質関門: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（Wine、`WINEDEBUG=-all`）、GUI 非依存モジュールは `cargo test --lib --target x86_64-unknown-linux-gnu` も通す。
- テストデータは `tests/fixtures/`（小）と `scripts/gen-large-fixtures.py`（大、`target/fixtures-large/`）を使う。
- push しない。作業ブランチにコミットする。コミットメッセージの末尾:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01FKh1LYTmSLTjt6QEACFaoA
  ```
- `docs/PLAN.md` の自分の行の状態を「👀 レビュー中」にする（他の行は触らない）。
- 報告（日本語・簡潔）: ブランチ名、コミット、変更ファイル、設計判断、未解決点・統括への依頼（共有ファイルの変更、取り決めの変更提案）、品質関門の結果。
