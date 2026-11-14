//! Windows の機能への薄い包み（画面部品ではないもの）。**Windows 専用**で、
//! `windows` crate を使ってよいのはここと [`crate::ui`] だけ。中核モジュールから
//! このモジュールを参照しない。
//!
//! 下位モジュールと担当（ファイルをディレクトリに変えてもよい）:
//!
//! | モジュール | 担当 | 内容 |
//! |---|---|---|
//! | [`system`] | B | Windows の明暗設定の取得、DPI、文字列変換などの共通の補助 |
//! | [`hotkey`] | F | グローバルショートカットの登録と競合の検出 |
//! | [`clipboard`] | F | クリップボードのテキストの読み書き |
//! | [`webview2_loader`] | H | WebView2 の自前ローダー（`WebView2Loader.dll` を使わない） |

pub mod clipboard;
pub mod hotkey;
pub mod system;
pub mod webview2_loader;
