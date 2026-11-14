//! ファイルハンドルと、位置を指定した読み取り・外部変更の検知。
//!
//! - 共有読み取りで開く。Windows では共有モードを読み取り・書き込み・削除のすべてにして、
//!   他のアプリの書き込み・置換・削除を妨げない。
//! - 読み取りは位置を指定する形（`pread`／`seek_read`）だけを使い、ハンドルの現在位置に
//!   依存しない。複数のスレッドから同時に読める。
//! - 外部変更は、開いたときに控えた [`FileStamp`] と比べて検知する。**ハンドル**の状態
//!   （その場での切り詰め・追記・上書き）と、**パス**の状態（別のファイルへの置換・削除）
//!   の両方を見る。

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::core::{Error, Result};
use crate::file::FileStamp;

/// ファイルの同一性の手掛かり。Unix は装置番号と i ノード番号、Windows は作成時刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity(u64, u64);

impl Identity {
    #[cfg(unix)]
    fn of(metadata: &Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Identity(metadata.dev(), metadata.ino())
    }

    #[cfg(windows)]
    fn of(metadata: &Metadata) -> Self {
        use std::os::windows::fs::MetadataExt;
        Identity(metadata.creation_time(), 0)
    }
}

pub(crate) struct RawFile {
    file: File,
    path: PathBuf,
    /// 開いたときのハンドルの状態。
    handle_stamp: FileStamp,
    /// 開いたときのパスの状態。
    path_stamp: FileStamp,
    identity: Identity,
}

impl RawFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            options.share_mode(0x1 | 0x2 | 0x4);
        }
        let file = options.open(path).map_err(|e| Error::io("開く", path, e))?;
        let handle_meta = file.metadata().map_err(|e| Error::io("開く", path, e))?;
        if !handle_meta.is_file() {
            return Err(Error::invalid_argument(
                "path",
                "通常のファイルではありません",
            ));
        }
        let path_meta = std::fs::metadata(path).map_err(|e| Error::io("開く", path, e))?;
        let handle_stamp = FileStamp::of_file(&file).map_err(|e| Error::io("開く", path, e))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            handle_stamp,
            path_stamp: FileStamp::read(path)?
                .unwrap_or_else(|| FileStamp::from_metadata(&path_meta)),
            identity: Identity::of(&handle_meta),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn len(&self) -> u64 {
        self.handle_stamp.len
    }

    pub(crate) fn stamp(&self) -> FileStamp {
        self.path_stamp
    }

    /// `offset` から `buf` が埋まるか末尾に達するまで読む。
    pub(crate) fn read_at(&self, mut offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            match self.read_once(offset, &mut buf[done..]) {
                Ok(0) => break,
                Ok(n) => {
                    done += n;
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    #[cfg(unix)]
    fn read_once(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
    }

    #[cfg(windows)]
    fn read_once(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        match std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset) {
            // ERROR_HANDLE_EOF: 末尾より先を読もうとした。
            Err(e) if e.raw_os_error() == Some(38) => Ok(0),
            other => other,
        }
    }

    /// ハンドルの指すファイルが、開いたときから変わったか（長さ・更新時刻）。
    pub(crate) fn handle_changed(&self) -> io::Result<bool> {
        let now = FileStamp::of_file(&self.file)?;
        Ok(!now.matches(&self.handle_stamp))
    }

    /// パスの指すファイルが、開いたときから変わったか（置換・削除・長さ・更新時刻）。
    pub(crate) fn path_changed(&self) -> io::Result<bool> {
        match std::fs::metadata(&self.path) {
            Ok(meta) => {
                let now = match FileStamp::read(&self.path) {
                    Ok(Some(stamp)) => stamp,
                    Ok(None) => return Ok(true),
                    Err(_) => FileStamp::from_metadata(&meta),
                };
                Ok(!now.matches(&self.path_stamp) || Identity::of(&meta) != self.identity)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        }
    }
}
