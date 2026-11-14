//! ファイル入出力の中核（タスク A の担当。`src/file/` 配下）。
//!
//! 責務: ファイルを読んで編集用 UTF-8 と [`TextFormat`](crate::text::TextFormat) にする、
//! 保存（同じディレクトリの一時ファイルに書いてから置換）、失敗時に元ファイルを守る、
//! 外部変更の検知、ロックファイル。GUI に依存せず、`windows` crate を使わない
//! （Windows 固有の処理が必要なら `std::os::windows` の範囲で `#[cfg(windows)]`）。
//!
//! このモジュールが提供する共通部品（`docs/architecture.md` の「同時アクセス」参照）:
//! - **原子的な書き込み**: 一時ファイル → 書き込み → 同期 → 置換。メモ（タスク E）、
//!   復元（タスク D）、設定の保存もこの関数を使う。独自に実装しない。
//! - **ロックファイル**: 別プロセスとの排他。取れなければ待たずに
//!   [`Error::Busy`](crate::core::Error::Busy) を返す形を基本にする。
//! - **外部変更の検知**: [`FileStamp`] の比較。
//!
//! 使う共通型: [`crate::core::Error`]／[`Result`](crate::core::Result)、
//! [`crate::core::TaskContext`]（中止・進捗）、[`crate::text::TextFormat`]。
//! 保存はバックグラウンドで行えるよう、本文の写し（UTF-8 のバイト列）と
//! [`crate::core::DocStamp`] を受け取り、終わったら同じ印を返す形にする。
//!
//! ここに定義済みの [`FileStamp`] は共有の取り決め。変える場合は報告すること。
//!
//! ## 公開している部品（タスク A。`docs/architecture.md` §7 にも記載）
//!
//! | 用途 | 部品 |
//! |---|---|
//! | 原子的な書き込み（メモ・復元・設定） | [`write_atomic`]、[`write_atomic_with`]、[`is_temp_file_name`] |
//! | ロック | [`LockFile::try_acquire`] |
//! | 利用者のファイルを開く | [`read_text`]（[`ReadOptions`] → [`LoadedText`]）、[`read_bytes`] |
//! | 利用者のファイルを保存する | [`save_text`]（[`SaveRequest`] → [`SaveOutcome`]） |
//! | 外部変更の検知 | [`FileStamp::read`]、[`check_external_change`]、[`DiskExpectation`] |
//! | 失敗の原因の分類 | [`classify`] → [`IoFailure`] |
//!
//! ## 保存で守ること
//!
//! - 元ファイルを直接開いて書くことはしない。必ず同じディレクトリの一時ファイルに
//!   書いて確定させてから置換する。失敗・中止・パニックでは一時ファイルを消す。
//! - 変換できない文字があれば、置換の前に止めて一覧を返す。
//! - 保存の直前と置換の直前に、ディスク上の状態が控えと同じか確かめる。
//! - 読み取り専用の属性が付いたファイルは上書きしない（属性を外して書くことはしない）。
//!
//! ## 既知の制限
//!
//! - 置換は「新しいファイルで置き換える」方式なので、ハードリンクは切れる。
//! - Windows の属性・ACL・代替データストリームは `ReplaceFileW` が引き継ぐ。
//!   `ReplaceFileW` が使えないファイルシステムでは単純な置換になり、引き継がれない。
//! - 外部変更の検知は長さと更新時刻による。同じ長さで、更新時刻の刻み
//!   （NTFS は 100 ナノ秒、FAT は 2 秒）の中で書き換えられた場合は検知できない。

mod atomic;
mod failure;
mod lock;
mod sys;
mod text_io;

#[cfg(test)]
pub(crate) mod testutil;

pub use atomic::{is_temp_file_name, write_atomic, write_atomic_with, AtomicOptions, ReplaceMode};
pub use failure::{classify, classify_io, IoFailure};
pub use lock::LockFile;
pub use text_io::{
    decode_file_bytes, read_bytes, read_text, save_text, DiskExpectation, LoadedText, ReadOptions,
    SaveOutcome, SaveRequest, Saved, MAX_UNMAPPABLE_REPORT,
};

use std::fs::Metadata;
use std::path::Path;
use std::time::SystemTime;

use crate::core::{Error, Result};

/// ディスク上のファイルの状態の控え。外部変更の検知に使う。
///
/// 開いたとき・保存したときに控え、ウィンドウが前面に来たとき・保存の直前・
/// 復元のときに取り直して比べる。食い違えば外部で変更されたとみなす
/// （切り詰め・置換・更新時刻の変化を拾う。内容の比較はしない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub len: u64,
    /// 更新時刻。取得できない環境では `None`。
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    /// `path` のいまの状態を控える。存在しなければ `None`。
    /// シンボリックリンクはリンク先を見る。
    pub fn read(path: &Path) -> Result<Option<FileStamp>> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self::from_metadata(&metadata))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io("ファイルの状態の確認", path, e)),
        }
    }
}

/// 控え（`known`）と比べた、ディスク上のファイルの変化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalChange {
    /// 変化なし（控えと同じ。または、控えもファイルもない）。
    None,
    /// 内容が変わった（長さか更新時刻が違う）。新しい状態つき。
    Modified(FileStamp),
    /// 消された（または名前を変えられた）。
    Removed,
    /// 控えがない（未保存の文書の保存先など）のに、ファイルが現れた。
    Created(FileStamp),
}

impl ExternalChange {
    pub fn is_changed(&self) -> bool {
        !matches!(self, ExternalChange::None)
    }
}

/// `path` が、控え（開いたとき・保存したときの [`FileStamp`]）から変化したか調べる。
///
/// ウィンドウが前面に来たとき・保存の直前・復元のときに呼ぶ。変化していれば
/// `DocumentState::record_external_change()`。状態を取れない（権限がない、ドライブが
/// 外された等）場合は [`Error::Io`] で、変化の有無は分からない。
pub fn check_external_change(path: &Path, known: Option<FileStamp>) -> Result<ExternalChange> {
    Ok(match (known, FileStamp::read(path)?) {
        (None, None) => ExternalChange::None,
        (Some(k), Some(now)) if k == now => ExternalChange::None,
        (Some(_), Some(now)) => ExternalChange::Modified(now),
        (Some(_), None) => ExternalChange::Removed,
        (None, Some(now)) => ExternalChange::Created(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_changes_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!(
            "linote-filestamp-{}-{}",
            std::process::id(),
            crate::core::MemoId::generate()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, b"abc").unwrap();
        let before = FileStamp::from_metadata(&std::fs::metadata(&path).unwrap());
        assert_eq!(before.len, 3);
        assert_eq!(
            before,
            FileStamp::from_metadata(&std::fs::metadata(&path).unwrap())
        );
        std::fs::write(&path, b"abcdef").unwrap();
        let after = FileStamp::from_metadata(&std::fs::metadata(&path).unwrap());
        assert_ne!(before, after);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
