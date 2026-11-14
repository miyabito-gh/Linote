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

use std::fs::Metadata;
use std::time::SystemTime;

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
