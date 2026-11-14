//! 入出力の失敗の原因の分類。
//!
//! 境界を越えるエラーは [`Error::Io`]（どの段階かは `op`、OS のエラーは `source`）の
//! まま返し、画面で原因に応じた案内を出したいときに [`classify`] で分類する。

use std::io;

use crate::core::Error;

/// 入出力の失敗の原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoFailure {
    /// ファイルまたはフォルダーが存在しない。
    NotFound,
    /// 権限がない。
    PermissionDenied,
    /// 読み取り専用（属性、または書き込めない媒体）。
    ReadOnly,
    /// 他のプログラムが使用中（共有違反・ロック）。
    InUse,
    /// 空き容量・割り当ての不足。
    DiskFull,
    /// パスまたは名前が長すぎる・使えない。
    InvalidPath,
    /// ファイルではなくフォルダーだった。
    IsDirectory,
    /// その他。
    Other,
}

impl IoFailure {
    /// 画面に出せる短い説明。
    pub const fn describe(self) -> &'static str {
        match self {
            IoFailure::NotFound => "ファイルまたはフォルダーが見つかりません",
            IoFailure::PermissionDenied => "アクセスする権限がありません",
            IoFailure::ReadOnly => "読み取り専用のため書き込めません",
            IoFailure::InUse => "他のプログラムが使用中です",
            IoFailure::DiskFull => "空き容量が足りません",
            IoFailure::InvalidPath => "パスが長すぎるか、使えない名前です",
            IoFailure::IsDirectory => "フォルダーには保存できません",
            IoFailure::Other => "入出力エラー",
        }
    }
}

/// [`Error::Io`] の原因を分類する。入出力以外のエラーでは `None`。
pub fn classify(error: &Error) -> Option<IoFailure> {
    match error {
        Error::Io { source, .. } => Some(classify_io(source)),
        _ => None,
    }
}

/// OS のエラーを分類する。
pub fn classify_io(error: &io::Error) -> IoFailure {
    if let Some(code) = error.raw_os_error() {
        if let Some(kind) = classify_os_code(code) {
            return kind;
        }
    }
    use io::ErrorKind as K;
    match error.kind() {
        K::NotFound => IoFailure::NotFound,
        K::PermissionDenied => IoFailure::PermissionDenied,
        K::ReadOnlyFilesystem => IoFailure::ReadOnly,
        K::ResourceBusy | K::ExecutableFileBusy | K::WouldBlock => IoFailure::InUse,
        K::StorageFull | K::QuotaExceeded | K::FileTooLarge => IoFailure::DiskFull,
        K::InvalidFilename => IoFailure::InvalidPath,
        K::IsADirectory => IoFailure::IsDirectory,
        _ => IoFailure::Other,
    }
}

#[cfg(windows)]
fn classify_os_code(code: i32) -> Option<IoFailure> {
    // winerror.h の値。
    Some(match code {
        2 | 3 => IoFailure::NotFound,       // FILE_NOT_FOUND, PATH_NOT_FOUND
        5 => IoFailure::PermissionDenied,   // ACCESS_DENIED
        19 => IoFailure::ReadOnly,          // WRITE_PROTECT
        32 | 33 | 1224 => IoFailure::InUse, // SHARING_VIOLATION, LOCK_VIOLATION, USER_MAPPED_FILE
        39 | 112 | 1295 => IoFailure::DiskFull, // HANDLE_DISK_FULL, DISK_FULL, DISK_QUOTA_EXCEEDED
        111 | 123 | 161 | 206 => IoFailure::InvalidPath, // BUFFER_OVERFLOW, INVALID_NAME, BAD_PATHNAME, FILENAME_EXCED_RANGE
        _ => return None,
    })
}

#[cfg(not(windows))]
fn classify_os_code(_code: i32) -> Option<IoFailure> {
    // Unix では std の ErrorKind への対応付けで足りる。
    None
}

/// 読み取り専用の属性が付いているファイルへの保存を断るときのエラー。
pub(super) fn read_only_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::ReadOnlyFilesystem,
        "読み取り専用の属性が付いています",
    )
}

/// 保存先がフォルダーだったときのエラー。
pub(super) fn is_directory_error() -> io::Error {
    io::Error::new(io::ErrorKind::IsADirectory, "フォルダーです")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_kind() {
        let io = |kind| io::Error::new(kind, "x");
        assert_eq!(
            classify_io(&io(io::ErrorKind::NotFound)),
            IoFailure::NotFound
        );
        assert_eq!(
            classify_io(&io(io::ErrorKind::StorageFull)),
            IoFailure::DiskFull
        );
        assert_eq!(classify_io(&read_only_error()), IoFailure::ReadOnly);
        assert_eq!(classify_io(&is_directory_error()), IoFailure::IsDirectory);
        assert_eq!(classify_io(&io(io::ErrorKind::Other)), IoFailure::Other);
        assert!(!IoFailure::InUse.describe().is_empty());
    }

    #[test]
    fn classifies_errors_from_the_os() {
        let missing = std::env::temp_dir().join("linote-no-such-dir-xyz/a.txt");
        let e = std::fs::read(&missing).unwrap_err();
        assert_eq!(classify_io(&e), IoFailure::NotFound);
        let wrapped = Error::io("読み込み", &missing, e);
        assert_eq!(classify(&wrapped), Some(IoFailure::NotFound));
        assert_eq!(classify(&Error::Cancelled), None);
    }

    #[cfg(windows)]
    #[test]
    fn classifies_windows_codes() {
        let os = io::Error::from_raw_os_error;
        assert_eq!(classify_io(&os(32)), IoFailure::InUse);
        assert_eq!(classify_io(&os(112)), IoFailure::DiskFull);
        assert_eq!(classify_io(&os(206)), IoFailure::InvalidPath);
        assert_eq!(classify_io(&os(5)), IoFailure::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn classifies_unix_codes() {
        let os = io::Error::from_raw_os_error;
        assert_eq!(classify_io(&os(28)), IoFailure::DiskFull); // ENOSPC
        assert_eq!(classify_io(&os(36)), IoFailure::InvalidPath); // ENAMETOOLONG
        assert_eq!(classify_io(&os(30)), IoFailure::ReadOnly); // EROFS
        assert_eq!(classify_io(&os(13)), IoFailure::PermissionDenied); // EACCES
        assert_eq!(classify_io(&os(21)), IoFailure::IsDirectory); // EISDIR
    }
}
