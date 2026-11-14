//! WebView2 の自前ローダー。`WebView2Loader.dll` も `webview2-com-sys` のローダー関数も使わない
//! （単体 EXE 要件。`docs/architecture.md` の「WebView2」）。
//!
//! 手順:
//! 1. レジストリからインストール済みランタイムの場所と版を探す。EdgeUpdate の Clients キー
//!    `{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` の値 `pv`（版）と `location` を、
//!    HKLM の `SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients`、
//!    HKLM の `SOFTWARE\Microsoft\EdgeUpdate\Clients`、HKCU の同じキーの順に調べる。
//! 2. `<runtime>\EBWebView\x64\EmbeddedBrowserWebView.dll` を `LoadLibraryW` で読み込む。
//! 3. その DLL の `CreateWebViewEnvironmentWithOptionsInternal` を `GetProcAddress` で取り、呼ぶ。
//!
//! ランタイムが見つからなければ [`crate::core::Error::Unavailable`] を返す。
//! プレビューを初めて開くまで、このモジュールの関数を呼ばない。
//!
//! 担当: タスク H。
