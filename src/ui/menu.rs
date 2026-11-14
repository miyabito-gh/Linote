//! メニューとショートカットからコマンドへの対応。
//! どちらも [`crate::command::Invocation::user`] を作って登録表に渡す。キーは
//! [`crate::appdata::Settings::key_for`] で決める。有効・無効の表示は
//! [`crate::command::Registry::check_available`]。
//!
//! 担当: タスク I1（統括）。
