//! OS ごとの「置換」の実装。
//!
//! Windows では `kernel32` の `MoveFileExW`／`ReplaceFileW` を直接宣言して呼ぶ
//! （中核は `windows` crate を使わない取り決めのため。std の `fs::rename` は
//! `MOVEFILE_WRITE_THROUGH` を付けず、`ReplaceFileW` 相当もない）。

use std::io;
use std::path::Path;

use super::atomic::ReplaceMode;

/// `temp` を `target` に置き換える。`target_exists` は直前に確かめた結果。
/// 失敗した場合、`target` は元のまま（または元から存在しないまま）で、`temp` が残る。
pub(super) fn replace(
    temp: &Path,
    target: &Path,
    mode: ReplaceMode,
    target_exists: bool,
) -> io::Result<()> {
    imp::replace(temp, target, mode, target_exists)
}

/// 置換をディスクに確定させる（Unix: 親ディレクトリの fsync。失敗は無視する）。
pub(super) fn sync_dir(dir: &Path) {
    imp::sync_dir(dir)
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn replace(temp: &Path, target: &Path, _: ReplaceMode, _: bool) -> io::Result<()> {
        // rename(2) は同じファイルシステム内で原子的に置き換える。
        // 属性（許可ビット）の引き継ぎは呼び出し側が rename の前に済ませている。
        std::fs::rename(temp, target)
    }

    pub fn sync_dir(dir: &Path) {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x2;
    const REPLACEFILE_IGNORE_ACL_ERRORS: u32 = 0x4;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
    }

    /// NUL 終端の UTF-16。絶対パスは `\\?\` 形式にして MAX_PATH の制限を避ける
    /// （std の入出力関数が内部で行っているのと同じ変換）。
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let absolute = std::path::absolute(path)?;
        let mut out: Vec<u16> = Vec::new();
        match absolute.components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::Disk(_) => {
                    out.extend(r"\\?\".encode_utf16());
                    out.extend(absolute.as_os_str().encode_wide());
                }
                Prefix::UNC(_, _) => {
                    // `\\server\share\…` → `\\?\UNC\server\share\…`
                    out.extend(r"\\?\UNC\".encode_utf16());
                    out.extend(absolute.as_os_str().encode_wide().skip(2));
                }
                _ => out.extend(absolute.as_os_str().encode_wide()),
            },
            _ => out.extend(absolute.as_os_str().encode_wide()),
        }
        if out.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "パスに NUL が含まれています",
            ));
        }
        out.push(0);
        Ok(out)
    }

    fn move_replace(temp: &[u16], target: &[u16]) -> io::Result<()> {
        // SAFETY: どちらも NUL 終端の UTF-16 で、呼び出しの間は生きている。
        let ok = unsafe {
            MoveFileExW(
                temp.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// `ReplaceFileW` を呼ぶ前に、保存先を「削除（置換）できる通常のファイル」として
    /// 開けるか試す。開けなければ、そのエラーを返す（他のプログラムが共有を許さずに
    /// 開いている、フォルダーに変わっていた、権限がない等）。
    ///
    /// `ReplaceFileW` は内部で保存先を別名に退避してから置き換えるので、失敗が
    /// 分かっている場合は呼ばずに済ませ、退避用のファイル（`~RF….TMP` の類）を
    /// 残さないようにする。保存先が消えていたら `Ok(false)`（単純な置換に回す）。
    fn probe_replaceable(target: &Path) -> io::Result<bool> {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        match std::fs::OpenOptions::new().access_mode(DELETE).open(target) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn replace(
        temp: &Path,
        target: &Path,
        mode: ReplaceMode,
        target_exists: bool,
    ) -> io::Result<()> {
        let temp_w = wide(temp)?;
        let target_w = wide(target)?;
        if mode == ReplaceMode::PreserveMetadata && target_exists && probe_replaceable(target)? {
            // ReplaceFileW は置き換えられる側の属性・ACL・作成日時・代替データストリームを
            // 引き継ぐ。
            // SAFETY: パスは NUL 終端の UTF-16。省略できる引数は NULL。
            let ok = unsafe {
                ReplaceFileW(
                    target_w.as_ptr(),
                    temp_w.as_ptr(),
                    std::ptr::null(),
                    REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
            ) {
                // 他のプログラムが開いている。回り道をせず、そのまま知らせる。
                return Err(error);
            }
            // ReplaceFileW が使えないファイルシステム（FAT、一部のネットワーク共有）や、
            // 途中で止まった場合。一時ファイルが残っていれば、単純な置換でやり直す
            // （属性は引き継げないが、内容は守られる）。
            if !temp.exists() {
                return Err(error);
            }
        }
        move_replace(&temp_w, &target_w)
    }

    pub fn sync_dir(_dir: &Path) {
        // MOVEFILE_WRITE_THROUGH が、置換がディスクに書かれるまで待つ。
    }
}
