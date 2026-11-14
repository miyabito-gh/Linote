//! Markdown プレビュー（右ペインと別ウィンドウ）。変換は [`crate::markdown`]、
//! WebView2 の読み込みは [`crate::platform::webview2_loader`]。
//!
//! 初めてプレビューを開く操作まで、WebView2 に関する処理（ランタイムの確認を含む）を
//! 一切行わない。ランタイムがなければ、開いた時点で案内し、編集は続けられるようにする。
//! 対象は [`crate::document::Capability::Preview`] を持つ文書。
//!
//! 担当: タスク H。

mod host;
mod pane;
mod policy;
mod window;

pub use host::PreviewHost;
pub use pane::{PaneStatus, PreviewPane};
pub use policy::{
    decide_navigation, fits_navigate_to_string, guidance_text, oversize_notice_html, NavDecision,
    MAX_NAVIGATE_BYTES,
};
pub use window::PreviewWindow;
