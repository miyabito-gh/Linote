//! 全モジュール共通の基礎型（タスク C0）。
//!
//! ここにある型は GUI にも OS にも依存しない。`windows` crate を使わないこと。
//! 取り決めの全体像は `docs/architecture.md` を参照。
//!
//! - [`ids`] — 文書 ID・メモ ID・版・実行者
//! - [`error`] — 共通エラー型 [`Error`] と [`Result`]
//! - [`cancel`] — 中止（[`CancelToken`]）
//! - [`progress`] — 進捗通知（[`Progress`]、[`ProgressSink`]）
//! - [`limits`] — 件数・バイト数の上限と、途中で終わった結果の表現（[`Outcome`]）
//! - [`background`] — バックグラウンド実行と UI スレッドへの結果の受け渡し

pub mod background;
pub mod cancel;
pub mod error;
pub mod ids;
pub mod limits;
pub mod progress;

pub use background::{ui_channel, TaskContext, UiReceiver, UiSender, UiWaker, WorkerPool};
pub use cancel::CancelToken;
pub use error::{Error, Result};
pub use ids::{Actor, DocStamp, DocumentId, MemoId, TargetRef, Version};
pub use limits::{Budget, Completion, Limits, Outcome};
pub use progress::{NoProgress, Progress, ProgressSink};
