//! 未保存変更の復元データ（タスク D の担当。`src/recovery/` 配下）。
//!
//! 責務: 復元データの書き出し形式、原子的な書き込み、所有と排他（複数ウィンドウ・
//! 別プロセス）、起動時の検出、保存・破棄時の削除。GUI に依存せず、`windows` crate を
//! 使わない。
//!
//! 取り決め:
//! - 対象は [`crate::document::Capability::Recover`] を持つ文書
//!   （通常ファイル、新規未保存、クリップボード由来）。メモは対象外（メモ自体が自動保存）。
//! - 保存先は [`crate::appdata::AppPaths::recovery_dir`]。中の形式はこのモジュールが決めて
//!   `docs/architecture.md` に追記する。ディレクトリは最初の書き出しで作る。
//! - [`crate::core::DocumentId`] はプロセス内でしか通用しない。復元データの永続的な
//!   識別子はこのモジュールで定義する。
//! - 復元データには、本文（UTF-8）、元のパス、[`crate::text::TextFormat`]、開いたときの
//!   [`crate::file::FileStamp`]（復元時の外部変更の検知用）、書き出した版を含める。
//! - 書き出しはバックグラウンドで行う: UI が本文の写しと [`crate::core::DocStamp`] を渡し、
//!   完了の通知で同じ印を返す。古い版の書き出しが新しい版の復元データを上書きしないこと。
//! - 書き込みは `file` モジュールの原子的な書き込みとロックファイルを使う。動作中の
//!   プロセスが所有する復元データを、別のプロセスが復元・削除しないこと。
//! - 元ファイルへの保存の成功、または利用者による明示的な破棄で削除する。
//!   復元した内容を元ファイルへ書くのは、利用者が保存したときだけ。
//!
//! 構成:
//! - [`format`]: 1 件の復元データのファイル形式（版つきヘッダー + 本文、検査つき）。
//! - [`store`]: 保存先ディレクトリの操作（[`RecoveryStore`]、[`RecoverySlot`]、起動時の [`ScanReport`]）。
//! - `RecoveryPolicy`: 書き出しの間隔の方針（設定可能、既定値つき）。
//!
//! 保存先のディレクトリ構成と形式は `docs/architecture.md` §7 の「復元データの形式」。

pub mod format;
mod policy;
mod store;

pub use format::{Meta, RecoveryId, RecoveryKind, Snapshot, FORMAT_VERSION};
pub use policy::RecoveryPolicy;
pub use store::{
    Candidate, LiveEntry, QuarantineReason, Quarantined, RecoverySlot, RecoveryStore, ScanReport,
    WriteOutcome,
};
