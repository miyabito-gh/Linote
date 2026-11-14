//! ロックファイル（別プロセス・別ウィンドウとの排他）。
//!
//! OS の排他ロック（Windows: `LockFileEx`、Unix: `flock`。std の `File::try_lock`）を
//! 専用のファイルに掛ける。**プロセスが落ちれば OS が解放する**ので、強制終了の後に
//! ロックが残ることはない。「ファイルが存在すればロック中」という方式ではないので、
//! ロックファイル自体は消さずに残す（消すと、同時に取りに来た相手と別々のファイルを
//! ロックしてしまう競合が起きる）。
//!
//! 使い方の取り決め:
//! - ロックファイルは**データとは別の専用ファイル**にする（例: `memos/.lock`、
//!   `recovery/<id>.lock`）。Windows では、ロックされた範囲は他のハンドルから読み書き
//!   できなくなるため、データのファイルそのものには掛けない。
//! - 取れなければ待たずに [`Error::Busy`]。待つ・やり直すのは呼び出し側が
//!   ワーカーかタイマーで行う（UI スレッドで待たない）。
//! - 同じプロセスの中でも、同じパスを 2 回取ることはできない（2 回目は `Busy`）。
//! - 親ディレクトリは作らない（呼び出し側が `AppPaths::ensure_dir` などで用意する）。

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::core::{Error, Result};

/// 保持している間、同じパスのロックを他の誰も取れない。Drop で解放する。
#[derive(Debug)]
pub struct LockFile {
    file: File,
    path: PathBuf,
}

impl LockFile {
    /// ロックを取る。ファイルがなければ作る。他が保持していれば待たずに
    /// [`Error::Busy`]。それ以外の失敗は [`Error::Io`]。
    pub fn try_acquire(path: &Path) -> Result<LockFile> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| Error::io("ロックファイルを開く", path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(LockFile {
                file,
                path: path.to_path_buf(),
            }),
            Err(TryLockError::WouldBlock) => Err(Error::Busy {
                what: path.display().to_string(),
            }),
            Err(TryLockError::Error(e)) => Err(Error::io("ロック", path, e)),
        }
    }

    /// ロックファイルのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 明示的に解放する（Drop と同じ）。
    pub fn release(self) {}
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // 閉じれば OS が解放するが、明示的に外しておく。
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::testutil::TempDir;

    #[test]
    fn second_acquire_is_busy_until_released() {
        let dir = TempDir::new("lock-basic");
        let path = dir.path().join(".lock");
        let first = LockFile::try_acquire(&path).unwrap();
        assert_eq!(first.path(), path);
        let err = LockFile::try_acquire(&path).unwrap_err();
        assert_eq!(err.code(), "busy");
        drop(first);
        // ロックファイルは残るが、取り直せる。
        assert!(path.exists());
        let second = LockFile::try_acquire(&path).unwrap();
        second.release();
        LockFile::try_acquire(&path).unwrap();
    }

    #[test]
    fn different_paths_are_independent() {
        let dir = TempDir::new("lock-two");
        let _a = LockFile::try_acquire(&dir.path().join("a.lock")).unwrap();
        let _b = LockFile::try_acquire(&dir.path().join("b.lock")).unwrap();
    }

    #[test]
    fn missing_directory_is_an_io_error() {
        let dir = TempDir::new("lock-nodir");
        let err = LockFile::try_acquire(&dir.path().join("missing").join(".lock")).unwrap_err();
        assert_eq!(err.code(), "io");
    }

    #[test]
    fn only_one_thread_wins() {
        let dir = TempDir::new("lock-threads");
        let path = dir.path().join(".lock");
        let barrier = std::sync::Barrier::new(8);
        let wins = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    if let Ok(lock) = LockFile::try_acquire(&path) {
                        wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // 他のスレッドが試し終わるまで持ち続ける。
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        drop(lock);
                    }
                });
            }
        });
        assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
