//! データ保存場所の解決と設定の形式（タスク C0）。GUI に依存しない。
//!
//! 配置（`docs/architecture.md` の「データ保存場所」）:
//!
//! ```text
//! %LOCALAPPDATA%\Linote\
//!   settings.json          設定（[`settings::Settings`]）
//!   session.json           前回のタブの記録（[`session::Session`]）
//!   memos\                 一時メモ（タスク E が中の形式を決める）
//!   recovery\              未保存変更の復元データ（タスク D が中の形式を決める）
//!   mcp\authorization.json MCP の認可（次の版）
//!   mcp\operations.jsonl   AI の操作記録（次の版）
//! ```
//!
//! - ディレクトリとファイルは**初めて必要になった時点で**作る。パスを求めるだけでは
//!   何も作らない（[`AppPaths::ensure_dir`] を書き込みの直前に呼ぶ）。
//! - 書き込みは必ず `file` モジュールの原子的な書き込みを使う。
//! - テストでは [`AppPaths::with_root`] で一時ディレクトリを指す。実行ファイル全体を
//!   別の場所に向けたいとき（結合テスト、検証）は環境変数 [`DATA_DIR_ENV`] を使う。

pub mod handoff;
pub mod session;
pub mod settings;

pub use settings::{SearchHistory, Settings, ThemeMode, WindowSettings};

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::core::{Error, Result};

/// `%LOCALAPPDATA%` の下に作るディレクトリの名前。
pub const APP_DIR_NAME: &str = "Linote";

/// データの保存場所を丸ごと差し替える環境変数（絶対パス）。
/// 設定されていれば `%LOCALAPPDATA%\Linote` の代わりに使う。
pub const DATA_DIR_ENV: &str = "LINOTE_DATA_DIR";

/// データ保存場所のパス。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// 実行環境の環境変数から決める。
    pub fn from_env() -> Result<Self> {
        Self::resolve(|name| std::env::var_os(name))
    }

    /// 環境変数の引き方を差し替えられる版（テスト用）。
    ///
    /// [`DATA_DIR_ENV`] があればそれを、なければ `LOCALAPPDATA` の下の
    /// [`APP_DIR_NAME`] を使う。どちらもない・絶対パスでない場合は
    /// [`Error::Unavailable`]（メモ・復元・設定の保存ができないことを画面で知らせ、
    /// 編集は続けられるようにする）。
    pub fn resolve(lookup: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        let pick = |name: &str| lookup(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let root = match pick(DATA_DIR_ENV) {
            Some(dir) => dir,
            None => pick("LOCALAPPDATA")
                .map(|dir| dir.join(APP_DIR_NAME))
                .ok_or_else(|| {
                    Error::unavailable("データの保存場所", "LOCALAPPDATA が設定されていません")
                })?,
        };
        if !root.is_absolute() {
            return Err(Error::unavailable(
                "データの保存場所",
                format!("絶対パスではありません: {}", root.display()),
            ));
        }
        Ok(Self { root })
    }

    /// 保存場所を直接指定する（テスト用）。
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 設定ファイル。
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// 前回のタブの記録（V1、`session` モジュール）。
    pub fn session_file(&self) -> PathBuf {
        self.root.join("session.json")
    }

    /// 一時メモのディレクトリ（タスク E）。
    pub fn memos_dir(&self) -> PathBuf {
        self.root.join("memos")
    }

    /// 復元データのディレクトリ（タスク D）。
    pub fn recovery_dir(&self) -> PathBuf {
        self.root.join("recovery")
    }

    /// 別ウィンドウ（別プロセス）への文書の受け渡しファイル置き場（U4、`handoff` モジュール）。
    pub fn handoff_dir(&self) -> PathBuf {
        self.root.join("handoff")
    }

    /// MCP 関連のディレクトリ（次の版）。
    pub fn mcp_dir(&self) -> PathBuf {
        self.root.join("mcp")
    }

    /// MCP の認可ファイル（次の版）。
    pub fn mcp_authorization_file(&self) -> PathBuf {
        self.mcp_dir().join("authorization.json")
    }

    /// AI の操作記録（1 行 1 件の JSON。次の版）。
    pub fn mcp_operations_file(&self) -> PathBuf {
        self.mcp_dir().join("operations.jsonl")
    }

    /// 保存場所の中のディレクトリを、なければ作る。書き込みの直前に呼ぶ。
    /// 保存場所の外のパスは [`Error::Internal`]。
    pub fn ensure_dir(&self, dir: &Path) -> Result<()> {
        if !dir.starts_with(&self.root) {
            return Err(Error::Internal(format!(
                "保存場所の外のディレクトリ: {}",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir).map_err(|e| Error::io("ディレクトリの作成", dir, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(vars: &'a [(&'a str, &'a Path)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.as_os_str().to_owned())
        }
    }

    #[test]
    fn root_comes_from_localappdata_unless_overridden() {
        let base = std::env::temp_dir();
        let paths = AppPaths::resolve(lookup(&[("LOCALAPPDATA", &base)])).unwrap();
        assert_eq!(paths.root(), base.join("Linote"));

        let custom = base.join("custom");
        let paths =
            AppPaths::resolve(lookup(&[("LOCALAPPDATA", &base), (DATA_DIR_ENV, &custom)])).unwrap();
        assert_eq!(paths.root(), custom);

        // 空の値は未設定として扱う。
        let paths = AppPaths::resolve(lookup(&[
            ("LOCALAPPDATA", &base),
            (DATA_DIR_ENV, Path::new("")),
        ]))
        .unwrap();
        assert_eq!(paths.root(), base.join("Linote"));
    }

    #[test]
    fn missing_or_relative_location_is_reported() {
        assert!(matches!(
            AppPaths::resolve(lookup(&[])),
            Err(Error::Unavailable { .. })
        ));
        assert!(matches!(
            AppPaths::resolve(lookup(&[("LOCALAPPDATA", Path::new("relative"))])),
            Err(Error::Unavailable { .. })
        ));
    }

    #[test]
    fn layout_is_fixed() {
        let paths = AppPaths::with_root("root");
        let root = Path::new("root");
        assert_eq!(paths.settings_file(), root.join("settings.json"));
        assert_eq!(paths.memos_dir(), root.join("memos"));
        assert_eq!(paths.recovery_dir(), root.join("recovery"));
        assert_eq!(
            paths.mcp_authorization_file(),
            root.join("mcp").join("authorization.json")
        );
        assert_eq!(
            paths.mcp_operations_file(),
            root.join("mcp").join("operations.jsonl")
        );
    }

    #[test]
    fn nothing_is_created_until_ensure_dir() {
        let root = std::env::temp_dir().join(format!(
            "linote-appdata-{}-{}",
            std::process::id(),
            crate::core::MemoId::generate()
        ));
        let paths = AppPaths::with_root(&root);
        let memos = paths.memos_dir();
        assert!(!root.exists());
        paths.ensure_dir(&memos).unwrap();
        paths.ensure_dir(&memos).unwrap();
        assert!(memos.is_dir());
        assert!(matches!(
            paths.ensure_dir(&std::env::temp_dir()),
            Err(Error::Internal(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
