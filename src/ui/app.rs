//! アプリの状態とメッセージループ。
//!
//! - 開いている文書の一覧（[`crate::document::DocumentState`] と表示部品の組）を持つ。
//! - [`crate::command::CommandHost`] を実装し、[`crate::command::Registry`] を保持する。
//!   `begin_edit`〜`end_edit` の間に届いた Scintilla の変更通知は、その実行者の変更として
//!   [`crate::document::DocumentState::record_edit`] に渡す。
//! - [`crate::core::UiWaker`] を `PostMessageW`（アプリ定義のメッセージ 1 種類）で実装し、
//!   そのメッセージを受けたら [`crate::core::UiReceiver::drain`] で結果を取り出す。
//!   取り出した結果は文書 ID と版を照合してから反映する。
//! - 1 プロセスにメインウィンドウは 1 つ。2 つ目のウィンドウは別プロセスになる。
//!
//! 担当: タスク B（統合は I1）。
