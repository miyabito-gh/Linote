//! クリップボードのテキストの読み書き（`CF_UNICODETEXT`）。
//! 日本語・タブ・改行をそのまま保つ。テキストがなければ `None` を返し、呼び出し側が短い状態表示を出す。
//! 取り込んだ文書は [`crate::document::DocumentState::clipboard`]。
//!
//! 担当: タスク F。
