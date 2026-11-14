//! 共通エラー型。
//!
//! モジュールの境界を越える公開関数は [`Result`] を返す。モジュール内部で細かい
//! エラー型を持つのは自由だが、境界では [`Error`] に変換する（`From` を実装する）。
//! [`Error::code`] は機械可読な安定した名前で、将来の MCP 応答にそのまま載せる。

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use super::ids::{TargetRef, Version};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// ファイル入出力の失敗。`op` は「読み込み」「一時ファイルの書き込み」など
    /// 利用者に見せられる短い説明。
    Io {
        op: &'static str,
        path: Option<PathBuf>,
        source: io::Error,
    },
    /// 利用者または呼び出し側が中止した。失敗として表示しない。
    Cancelled,
    /// 前提にした版と現在の版が食い違う。`actual` を返して再試行できるようにする。
    /// 黙って上書きしないための中心的なエラー。
    VersionMismatch {
        target: TargetRef,
        expected: Version,
        actual: Version,
    },
    /// 対象（文書、メモ、コマンド、ファイル）が存在しない・既に閉じられた。
    NotFound { what: String },
    /// 対象がその操作に対応していない（巨大モードでの保存など）。
    Unsupported { what: String },
    /// 引数が不正（無効な正規表現、範囲外の位置など）。`name` は引数名。
    InvalidArgument { name: String, reason: String },
    /// 上限を超えた（入力サイズなど）。件数上限で結果を打ち切る場合はエラーではなく
    /// [`Outcome`](super::limits::Outcome) を使う。
    LimitExceeded { what: String, limit: u64 },
    /// 文字コードの問題（変換できない文字、不正なバイト列）。
    Encoding { reason: String },
    /// 他のウィンドウ・プロセスが使用中（ロックを取れない）。
    Busy { what: String },
    /// 元ファイルが外部で変更されている。
    ExternalChange { path: PathBuf },
    /// 権限・認可・実行者の制約で拒否した。
    Denied { reason: String },
    /// 保存データが壊れている・読めない形式。
    Corrupt { what: String, reason: String },
    /// 必要な部品が使えない（WebView2 ランタイムがない、GUI が起動していない等）。
    Unavailable { what: String, reason: String },
    /// 想定外の状態。バグの可能性。
    Internal(String),
}

impl Error {
    /// 入出力エラーを作る。
    pub fn io(op: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        Error::Io {
            op,
            path: Some(path.as_ref().to_path_buf()),
            source,
        }
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Error::NotFound { what: what.into() }
    }

    pub fn unsupported(what: impl Into<String>) -> Self {
        Error::Unsupported { what: what.into() }
    }

    pub fn invalid_argument(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::InvalidArgument {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Error::Denied {
            reason: reason.into(),
        }
    }

    pub fn corrupt(what: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::Corrupt {
            what: what.into(),
            reason: reason.into(),
        }
    }

    pub fn unavailable(what: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::Unavailable {
            what: what.into(),
            reason: reason.into(),
        }
    }

    /// 機械可読な安定した名前。値を変えると互換性が壊れるので変えない。
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io { .. } => "io",
            Error::Cancelled => "cancelled",
            Error::VersionMismatch { .. } => "version_mismatch",
            Error::NotFound { .. } => "not_found",
            Error::Unsupported { .. } => "unsupported",
            Error::InvalidArgument { .. } => "invalid_argument",
            Error::LimitExceeded { .. } => "limit_exceeded",
            Error::Encoding { .. } => "encoding",
            Error::Busy { .. } => "busy",
            Error::ExternalChange { .. } => "external_change",
            Error::Denied { .. } => "denied",
            Error::Corrupt { .. } => "corrupt",
            Error::Unavailable { .. } => "unavailable",
            Error::Internal(_) => "internal",
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { op, path, source } => match path {
                Some(p) => write!(f, "{op}に失敗しました: {} ({source})", p.display()),
                None => write!(f, "{op}に失敗しました ({source})"),
            },
            Error::Cancelled => f.write_str("中止しました"),
            Error::VersionMismatch {
                target,
                expected,
                actual,
            } => write!(
                f,
                "{target} は変更されています（前提 {expected}、現在 {actual}）"
            ),
            Error::NotFound { what } => write!(f, "見つかりません: {what}"),
            Error::Unsupported { what } => write!(f, "この対象では実行できません: {what}"),
            Error::InvalidArgument { name, reason } => write!(f, "{name} が不正です: {reason}"),
            Error::LimitExceeded { what, limit } => {
                write!(f, "上限を超えました: {what}（上限 {limit}）")
            }
            Error::Encoding { reason } => write!(f, "文字コードの問題: {reason}"),
            Error::Busy { what } => write!(f, "他で使用中です: {what}"),
            Error::ExternalChange { path } => {
                write!(f, "ファイルが外部で変更されています: {}", path.display())
            }
            Error::Denied { reason } => write!(f, "拒否しました: {reason}"),
            Error::Corrupt { what, reason } => write!(f, "{what} を読めません: {reason}"),
            Error::Unavailable { what, reason } => write!(f, "{what} を使えません: {reason}"),
            Error::Internal(msg) => write!(f, "内部エラー: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::DocumentId;

    #[test]
    fn codes_are_stable() {
        let e = Error::VersionMismatch {
            target: TargetRef::Document(DocumentId::from_raw(7)),
            expected: Version::from_raw(1),
            actual: Version::from_raw(2),
        };
        assert_eq!(e.code(), "version_mismatch");
        assert_eq!(
            e.to_string(),
            "doc:7 は変更されています（前提 v1、現在 v2）"
        );
        assert_eq!(Error::Cancelled.code(), "cancelled");
        assert!(Error::Cancelled.is_cancelled());
    }

    #[test]
    fn io_error_keeps_path_and_source() {
        let e = Error::io(
            "読み込み",
            "a.txt",
            io::Error::new(io::ErrorKind::NotFound, "x"),
        );
        assert_eq!(e.code(), "io");
        assert!(e.to_string().contains("a.txt"));
        assert!(std::error::Error::source(&e).is_some());
    }
}
