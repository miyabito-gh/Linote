//! 一時メモの保存と検索（タスク E の担当。`src/memo/` 配下）。
//!
//! 責務: メモの作成・読み込み・更新・名前変更・削除、名前と本文での検索、一覧。
//! GUI に依存せず、`windows` crate を使わない（GUI と将来の MCP プロセスの両方から呼ぶ）。
//! メモの小窓は `ui::memo_panel`（同じくタスク E）。
//!
//! 取り決め:
//! - 保存先は [`crate::appdata::AppPaths::memos_dir`]。中の形式（ファイル名、メタデータ）は
//!   このモジュールが決めて `docs/architecture.md` に追記する。本文は UTF-8。
//! - 識別子は [`crate::core::MemoId`]。新規は `MemoId::generate()` で作り、ファイルは
//!   「新規作成でのみ成功する」方法で作る。
//! - **版の照合が必須**: メモごとに [`crate::core::Version`] を保存し、更新・名前変更・
//!   削除は前提の版を受け取る。食い違えば [`crate::core::Error::VersionMismatch`] に
//!   現在の版を載せて拒否する。黙って上書きしない。照合と書き込みはロックの中で行う。
//! - 書き込みは `file` モジュールの原子的な書き込みとロックファイルを使う（別ウィンドウ・
//!   別プロセスからの同時アクセスで壊れないこと）。
//! - 一覧は初めて開くときに読み込む。起動時には何も読まない。ディレクトリは最初の
//!   メモを保存するときに [`crate::appdata::AppPaths::ensure_dir`] で作る。
//! - 検索は [`crate::core::TaskContext`] を受け取り、[`crate::core::Outcome`] を返す。
//! - 開いているメモのタブは [`crate::document::DocumentKind::Memo`]。自動保存が成功したら
//!   [`crate::document::DocumentState::record_saved`] を呼ぶ。

mod search;
mod store;
mod tab;

pub use search::{fold_for_search, MemoHit};
pub use store::{Listing, Memo, MemoStore, SaveOutcome, MAX_BODY_BYTES};
pub use tab::{Finished, MemoTab};
