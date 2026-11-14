//! Markdown プレビュー（右ペインと別ウィンドウ）。変換は [`crate::markdown`]、
//! WebView2 の読み込みは [`crate::platform::webview2_loader`]。
//!
//! 初めてプレビューを開く操作まで、WebView2 に関する処理（ランタイムの確認を含む）を
//! 一切行わない。ランタイムがなければ、開いた時点で案内し、編集は続けられるようにする。
//! 対象は [`crate::document::Capability::Preview`] を持つ文書。
//!
//! 担当: タスク H。
