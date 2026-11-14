//! テスト用の一時ディレクトリ。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// テストごとの一時ディレクトリ。Drop で中身ごと消す。
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "linote-test-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    /// 既にあるディレクトリを引き取る（Drop で消す）。
    pub(crate) fn adopt(path: PathBuf) -> Self {
        TempDir { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// 直下の名前の一覧（整列済み）。一時ファイルが残っていないことの確認に使う。
    pub(crate) fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
