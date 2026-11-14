//! Scintilla の包み。Scintilla への呼び出し（[`crate::sci_sys`]、`SendMessage`）は
//! このモジュールに集める。
//!
//! - コードページは UTF-8。位置は [`crate::document::TextRange`] と同じバイト位置。
//! - [`crate::document::EditTarget`] を実装する（`begin_undo_group` は
//!   `SCI_BEGINUNDOACTION`）。
//! - `SCN_MODIFIED`（`SC_MOD_INSERTTEXT`／`SC_MOD_DELETETEXT`）を受けるたびに
//!   文書の版を進める。`SCN_SAVEPOINTREACHED` では
//!   [`crate::document::DocumentState::record_unmodified`] を呼ぶ。
//! - 全文の写しを常時持たない。保存・復元・変換のために写しを作るときは、
//!   その時点の [`crate::core::DocStamp`] と組にしてバックグラウンドへ渡す。
//!
//! 担当: タスク B。
