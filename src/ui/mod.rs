//! 画面（Win32）。**Windows 専用**で、`windows` crate を使ってよいのはここと
//! [`crate::platform`] だけ。中核モジュール（`core`、`document`、`command`、`text`、
//! `file`、`memo`、`recovery`、`search`、`bigfile`、`markdown`、`appdata`、`mcp`）から
//! このモジュールを参照しない。
//!
//! 取り決め（`docs/architecture.md`）:
//! - 文書の状態（[`crate::document::DocumentState`]）は UI スレッドだけが持ち、変更する。
//! - 操作はすべて [`crate::command::Registry::execute`] を通す。メニュー・ショートカットの
//!   処理に操作の中身を直接書かない。
//! - 長い処理は [`crate::core::WorkerPool`] に渡し、結果は [`crate::core::ui_channel`] で
//!   受け取る。反映の前に [`crate::core::DocStamp`] を照合する。
//! - 起動経路に重い初期化を置かない。メモ一覧、Grep、プレビューは初めて使うときに作る。
//!
//! 下位モジュールと担当（ファイルをディレクトリに変えてもよい）:
//!
//! | モジュール | 担当 | 内容 |
//! |---|---|---|
//! | [`app`] | B（統合は I1） | アプリの状態、メッセージループ、UI スレッドへの通路、`CommandHost` の実装 |
//! | [`window`] | B | メインウィンドウ、配置、DPI |
//! | [`tabs`] | B | タブの独自描画 |
//! | [`statusbar`] | B | ステータスバーの独自描画 |
//! | [`theme`] | B | 明暗テーマと配色（編集欄・プレビューで共有） |
//! | [`editor`] | B | Scintilla の包み（呼び出しをここに集める）、`EditTarget` の実装、版を進める |
//! | [`menu`] | I1 | メニューとショートカットからコマンドへの対応 |
//! | [`dialogs`] | I1 | ファイルを開く・保存、確認、行ジャンプなどのダイアログ |
//! | [`find`] | K | 検索・置換欄、Grep の結果 |
//! | [`memo_panel`] | E | メモの小窓 |
//! | [`preview`] | H | Markdown プレビュー（右ペインと別ウィンドウ） |
//! | [`bigview`] | L | 巨大ファイルの表示（DirectWrite） |

pub mod app;
pub mod bigview;
pub mod dialogs;
pub mod editor;
pub mod find;
pub mod memo_panel;
pub mod menu;
pub mod preview;
pub mod statusbar;
pub mod tabs;
pub mod theme;
pub mod window;

/// 画面の入口（`main.rs` から呼ぶ）。タスク B が追加。
pub use app::{run, RunOptions};
