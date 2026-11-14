//! 原子的な書き込み（一時ファイル → 書き込み → 同期 → 置換）。
//!
//! 利用者のファイルの保存、メモ、復元データ、設定のすべてがこの実装を使う。
//!
//! 手順と保証:
//!
//! 1. 保存先を確かめる（フォルダーなら失敗、読み取り専用の属性があれば失敗、
//!    シンボリックリンクならリンク先を対象にする）。
//! 2. **保存先と同じディレクトリ**に一時ファイルを新規作成する（別のボリュームだと
//!    置換が原子的にならないため）。名前は [`is_temp_file_name`] で見分けられる。
//! 3. 内容を書き、`sync_all`（Windows: `FlushFileBuffers`、Unix: `fsync`）で確定させる。
//! 4. 置換する（[`ReplaceMode`]）。
//! 5. どの段階で失敗・中止しても、一時ファイルを消し、保存先には触れない。
//!    途中でプロセスが落ちた場合は一時ファイルが残りうるが、保存先は古い内容か
//!    新しい内容のどちらかが完全な形で残る。

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::failure::{is_directory_error, read_only_error};
use super::{sys, FileStamp};
use crate::core::{Error, Result, TaskContext};

/// 置換の方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReplaceMode {
    /// 単純な置換（Windows: `MoveFileExW(MOVEFILE_REPLACE_EXISTING |
    /// MOVEFILE_WRITE_THROUGH)`、Unix: `rename`）。ファイルシステムの単位で原子的。
    /// 新しいファイルの属性・ACL はディレクトリの既定になる。
    /// **アプリのデータ（メモ・復元・設定）向け。既定。**
    #[default]
    Rename,
    /// 既存のファイルの属性を引き継ぐ。**利用者のファイルの保存向け。**
    ///
    /// - Windows: `ReplaceFileW`（属性、ACL、作成日時、代替データストリームを引き継ぐ）。
    ///   使えないファイルシステムでは [`ReplaceMode::Rename`] と同じ置換に切り替える。
    ///   保存先が存在しなければ `Rename` と同じ。
    /// - Unix: 許可ビットを一時ファイルに写してから `rename`（所有者は引き継がない）。
    PreserveMetadata,
}

/// [`write_atomic_with`] の指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AtomicOptions {
    pub mode: ReplaceMode,
}

/// 一時ファイルの名前の先頭と末尾。
const TEMP_PREFIX: &str = ".~linote-";
const TEMP_SUFFIX: &str = ".tmp";

/// `name` が、この実装が作る一時ファイルの名前か。
///
/// ディレクトリを列挙する側（メモ、復元）は、これに当てはまるファイルを本物の
/// データとして扱わない。強制終了で残った古い一時ファイルは消してよい。
pub fn is_temp_file_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| n.starts_with(TEMP_PREFIX) && n.ends_with(TEMP_SUFFIX))
}

/// `bytes` を `path` に原子的に書く。失敗したら一時ファイルを消し、元のファイルに
/// 触れない。
///
/// 親ディレクトリは作らない（呼び出し側が `AppPaths::ensure_dir` などで用意する）。
/// 進捗はバイト数で報告する。置換は [`ReplaceMode::Rename`]。
pub fn write_atomic(path: &Path, bytes: &[u8], ctx: &TaskContext) -> Result<()> {
    write_atomic_with(path, AtomicOptions::default(), ctx, |out| {
        let total = bytes.len() as u64;
        let mut done = 0u64;
        for chunk in bytes.chunks(1 << 20) {
            ctx.cancel.check()?;
            out.write_all(chunk).map_err(|source| Error::Io {
                op: "書き込み",
                path: None,
                source,
            })?;
            done += chunk.len() as u64;
            ctx.report(done, Some(total));
        }
        Ok(())
    })
    .map(|_| ())
}

/// 内容を少しずつ書く版。`write` が一時ファイルへ書き、`Ok` を返したら置換する。
/// `write` が `Err` を返したら一時ファイルを消して、そのエラーを返す
/// （`path` のない [`Error::Io`] には保存先のパスを補う）。
///
/// 置換後のファイルの [`FileStamp`] を返す。
pub fn write_atomic_with(
    path: &Path,
    options: AtomicOptions,
    ctx: &TaskContext,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<FileStamp> {
    write_atomic_checked(path, options, ctx, write, || Ok(()))
}

/// [`write_atomic_with`] に、置換の直前（一時ファイルを書き終えて確定させた後）の
/// 確認を足した版。`before_replace` が `Err` を返したら置換せずに片付ける。
pub(super) fn write_atomic_checked(
    path: &Path,
    options: AtomicOptions,
    ctx: &TaskContext,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
    before_replace: impl FnOnce() -> Result<()>,
) -> Result<FileStamp> {
    ctx.cancel.check()?;
    let target = resolve_target(path)?;
    let file_name = target.file_name().ok_or_else(|| {
        Error::invalid_argument(
            "path",
            format!("ファイル名がありません: {}", path.display()),
        )
    })?;
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    debug_assert!(!file_name.is_empty());

    let existing = match fs::metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::io("保存先の確認", &target, e)),
    };
    if let Some(meta) = &existing {
        if meta.is_dir() {
            return Err(Error::io("保存先の確認", &target, is_directory_error()));
        }
        if meta.permissions().readonly() {
            return Err(Error::io("保存先の確認", &target, read_only_error()));
        }
    }

    let (mut file, temp_path) = create_temp(&dir).map_err(|e| {
        // 利用者に見せるのは保存先のパス（一時ファイルの名前は意味がない）。
        Error::io("一時ファイルの作成", &target, e)
    })?;
    let mut guard = TempGuard {
        path: temp_path,
        armed: true,
    };

    let mut counting = Counting {
        inner: &mut file,
        written: 0,
    };
    write(&mut counting).map_err(|e| match e {
        Error::Io {
            path: None, source, ..
        } => Error::io("一時ファイルの書き込み", &target, source),
        other => other,
    })?;
    let written = counting.written;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|e| Error::io("一時ファイルの書き込みの確定", &target, e))?;

    #[cfg(unix)]
    if options.mode == ReplaceMode::PreserveMetadata {
        if let Some(meta) = &existing {
            fs::set_permissions(&guard.path, meta.permissions())
                .map_err(|e| Error::io("属性の引き継ぎ", &target, e))?;
        }
    }
    // 置換の前に閉じる（Windows の ReplaceFileW は開いたままのファイルを扱えない）。
    drop(file);

    // ここが中止できる最後の場所。置換を始めたら最後までやる。
    ctx.cancel.check()?;
    before_replace()?;
    sys::replace(&guard.path, &target, options.mode, existing.is_some())
        .map_err(|e| Error::io("ファイルの置換", &target, e))?;
    guard.armed = false;
    sys::sync_dir(&dir);

    // 置換は済んでいる。状態を取れなくても保存は成功なので、失敗にはしない
    // （更新時刻なしの控えを返す。次の照合で食い違い、外部変更として安全側に倒れる）。
    Ok(match fs::metadata(&target) {
        Ok(meta) => FileStamp::from_metadata(&meta),
        Err(_) => FileStamp {
            len: written,
            modified: None,
        },
    })
}

/// シンボリックリンクなら、リンク先（実体）を保存先にする。リンクそのものを通常の
/// ファイルで置き換えてしまわないため。
fn resolve_target(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|e| Error::io("保存先の確認", path, e))
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// `dir` に、まだ存在しない名前で一時ファイルを作る。
fn create_temp(dir: &Path) -> io::Result<(File, PathBuf)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let mut last_error = None;
    for _ in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("{TEMP_PREFIX}{pid:x}-{nanos:x}-{n:x}{TEMP_SUFFIX}");
        let path = dir.join(name);
        // create_new: 既にあれば失敗する（他人のファイルを壊さない）。
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_error = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("一時ファイルの名前を決められない")))
}

/// 置換が済むまで、スコープを抜けたら（失敗・中止・パニック）一時ファイルを消す。
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Counting<'a> {
    inner: &'a mut File,
    written: u64,
}

impl Write for Counting<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CancelToken, Limits, NoProgress};
    use crate::file::testutil::TempDir;
    use crate::file::{classify, IoFailure};
    use std::sync::Arc;

    fn ctx() -> TaskContext {
        TaskContext::detached()
    }

    #[test]
    fn creates_and_replaces() {
        let dir = TempDir::new("atomic-basic");
        let path = dir.path().join("a.txt");
        write_atomic(&path, b"one", &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        write_atomic(&path, b"two two", &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two two");
        write_atomic(&path, b"", &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn returns_the_stamp_of_the_new_file() {
        let dir = TempDir::new("atomic-stamp");
        let path = dir.path().join("a.txt");
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            let stamp = write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                out.write_all(b"hello").unwrap();
                Ok(())
            })
            .unwrap();
            assert_eq!(stamp.len, 5);
            assert_eq!(Some(stamp), FileStamp::read(&path).unwrap());
        }
    }

    #[test]
    fn failed_write_leaves_the_original_and_no_temp_file() {
        let dir = TempDir::new("atomic-fail");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            // 途中まで書いてから失敗する。
            let err = write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                out.write_all(b"partial new content").unwrap();
                Err(Error::Io {
                    op: "書き込み",
                    path: None,
                    source: io::Error::new(io::ErrorKind::StorageFull, "full"),
                })
            })
            .unwrap_err();
            assert_eq!(classify(&err), Some(IoFailure::DiskFull));
            match &err {
                Error::Io { op, path: p, .. } => {
                    assert_eq!(*op, "一時ファイルの書き込み");
                    assert_eq!(p.as_deref(), Some(path.as_path()));
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_eq!(dir.entries(), ["a.txt"]);
        }
        // 入出力以外のエラーはそのまま返る。
        let err = write_atomic_with(&path, AtomicOptions::default(), &ctx(), |_| {
            Err(Error::Encoding { reason: "x".into() })
        })
        .unwrap_err();
        assert_eq!(err.code(), "encoding");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn panic_while_writing_cleans_up() {
        let dir = TempDir::new("atomic-panic");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let result = std::panic::catch_unwind(|| {
            let _ = write_atomic_with(&path, AtomicOptions::default(), &ctx(), |out| {
                out.write_all(b"partial").unwrap();
                panic!("boom");
            });
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn cancel_before_and_during_write() {
        let dir = TempDir::new("atomic-cancel");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();

        let cancel = CancelToken::new();
        let ctx = TaskContext::new(cancel.clone(), Arc::new(NoProgress), Limits::UNLIMITED);
        // 書いている途中で中止されると、置換の直前の確認で止まる。
        let err = write_atomic_with(&path, AtomicOptions::default(), &ctx, |out| {
            out.write_all(b"new").unwrap();
            cancel.cancel();
            Ok(())
        })
        .unwrap_err();
        assert!(err.is_cancelled());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);

        // 最初から中止されていれば何も作らない。
        let err = write_atomic(&path, &vec![b'x'; 3 << 20], &ctx).unwrap_err();
        assert!(err.is_cancelled());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn missing_directory_is_an_error_and_creates_nothing() {
        let dir = TempDir::new("atomic-nodir");
        let path = dir.path().join("missing").join("a.txt");
        let err = write_atomic(&path, b"x", &ctx()).unwrap_err();
        assert_eq!(classify(&err), Some(IoFailure::NotFound));
        assert!(!dir.path().join("missing").exists());
        assert!(dir.entries().is_empty());
    }

    #[test]
    fn directory_target_is_refused() {
        let dir = TempDir::new("atomic-isdir");
        let path = dir.path().join("sub");
        fs::create_dir(&path).unwrap();
        let err = write_atomic(&path, b"x", &ctx()).unwrap_err();
        assert_eq!(classify(&err), Some(IoFailure::IsDirectory));
        assert!(path.is_dir());
        assert_eq!(dir.entries(), ["sub"]);
        assert!(fs::read_dir(&path).unwrap().next().is_none());
    }

    #[test]
    fn read_only_target_is_refused_and_kept() {
        let dir = TempDir::new("atomic-readonly");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();

        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            let err = write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                out.write_all(b"new").unwrap();
                Ok(())
            })
            .unwrap_err();
            assert_eq!(classify(&err), Some(IoFailure::ReadOnly));
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_eq!(dir.entries(), ["a.txt"]);
        }

        // 片付けられるように属性を戻す。
        let mut perms = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn replace_failure_keeps_everything_and_removes_the_temp_file() {
        // 置換そのものの失敗: 確認の後で保存先が（空でない）フォルダーに変わった場合。
        let dir = TempDir::new("atomic-replace-fail");
        let path = dir.path().join("a.txt");
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            let _ = fs::remove_dir_all(&path);
            fs::write(&path, b"original").unwrap();
            let err = write_atomic_checked(
                &path,
                AtomicOptions { mode },
                &ctx(),
                |out| {
                    out.write_all(b"new").unwrap();
                    Ok(())
                },
                || {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                    fs::write(path.join("inner.txt"), b"keep").unwrap();
                    Ok(())
                },
            )
            .unwrap_err();
            match &err {
                Error::Io { op, .. } => assert_eq!(*op, "ファイルの置換"),
                other => panic!("{other:?}"),
            }
            assert_eq!(fs::read(path.join("inner.txt")).unwrap(), b"keep");
            assert_eq!(dir.entries(), ["a.txt"]);
        }
    }

    #[test]
    fn before_replace_can_veto() {
        let dir = TempDir::new("atomic-veto");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let err = write_atomic_checked(
            &path,
            AtomicOptions::default(),
            &ctx(),
            |out| {
                out.write_all(b"new").unwrap();
                Ok(())
            },
            || Err(Error::ExternalChange { path: path.clone() }),
        )
        .unwrap_err();
        assert_eq!(err.code(), "external_change");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn long_paths_work() {
        let dir = TempDir::new("atomic-long");
        let mut deep = dir.path().to_path_buf();
        // 全体で 260 文字を超えるようにする。
        for i in 0..6 {
            deep.push(format!("{i}-{}", "d".repeat(50)));
        }
        fs::create_dir_all(&deep).unwrap();
        let path = deep.join(format!("{}.txt", "f".repeat(60)));
        assert!(path.as_os_str().len() > 300);
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            for content in [&b"first"[..], b"second"] {
                write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                    out.write_all(content).unwrap();
                    Ok(())
                })
                .unwrap();
                assert_eq!(fs::read(&path).unwrap(), content);
            }
        }
        assert_eq!(fs::read_dir(&deep).unwrap().count(), 1);
    }

    #[test]
    fn non_ascii_names_work() {
        let dir = TempDir::new("atomic-unicode");
        let sub = dir.path().join("日本語のフォルダー");
        if fs::create_dir(&sub).is_err() {
            // Wine を UTF-8 でないロケールで動かしていると、ASCII 以外の名前を
            // 作れない。その環境では確かめようがないので省略する。
            eprintln!("skipped: ASCII 以外のファイル名を作れない環境");
            return;
        }
        let dir = TempDir::adopt(sub);
        let path = dir.path().join("メモ 😀.txt");
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                out.write_all(b"x").unwrap();
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), b"x");
        assert_eq!(dir.entries(), ["メモ 😀.txt"]);
    }

    #[test]
    fn path_without_file_name_is_rejected() {
        let err = write_atomic(Path::new(""), b"x", &ctx()).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    #[test]
    fn temp_names_are_recognised() {
        let dir = TempDir::new("atomic-tempname");
        let (file, path) = create_temp(dir.path()).unwrap();
        drop(file);
        assert!(is_temp_file_name(path.file_name().unwrap()));
        assert!(!is_temp_file_name(OsStr::new("a.txt")));
        assert!(!is_temp_file_name(OsStr::new("a.tmp")));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_writers_never_expose_a_torn_file() {
        // 複数のスレッドが同じファイルへ書き続けても、読む側は常にどれか一つの
        // 完全な内容を見る。
        let dir = TempDir::new("atomic-concurrent");
        let path = dir.path().join("a.txt");
        let contents: Vec<Vec<u8>> = (0..4u8)
            .map(|i| vec![b'a' + i; 50_000 + usize::from(i) * 1000])
            .collect();
        write_atomic(&path, &contents[0], &ctx()).unwrap();
        std::thread::scope(|scope| {
            for content in &contents {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..20 {
                        // Windows では、置換の瞬間に他のスレッドが開いていると共有違反で
                        // 失敗することがある。失敗しても壊れないことを確かめる。
                        let _ = write_atomic(path, content, &ctx());
                    }
                });
            }
            let path = &path;
            let contents = &contents;
            scope.spawn(move || {
                for _ in 0..200 {
                    if let Ok(read) = fs::read(path) {
                        assert!(contents.contains(&read), "torn read: {} bytes", read.len());
                    }
                }
            });
        });
        let last = fs::read(&path).unwrap();
        assert!(contents.contains(&last));
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn unix_preserves_mode_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("atomic-unix");
        let path = dir.path().join("a.sh");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        let options = AtomicOptions {
            mode: ReplaceMode::PreserveMetadata,
        };
        write_atomic_with(&path, options, &ctx(), |out| {
            out.write_all(b"new").unwrap();
            Ok(())
        })
        .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);

        // シンボリックリンク越しに保存しても、リンクは残り、実体が更新される。
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        write_atomic(&link, b"via link", &ctx()).unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&path).unwrap(), b"via link");
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_in_use_is_reported_and_original_kept() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TempDir::new("atomic-inuse");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        // 共有を一切許さずに開く（他のプログラムが排他的に開いている状態）。
        let holder = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        for mode in [ReplaceMode::Rename, ReplaceMode::PreserveMetadata] {
            let err = write_atomic_with(&path, AtomicOptions { mode }, &ctx(), |out| {
                out.write_all(b"new").unwrap();
                Ok(())
            })
            .unwrap_err();
            assert!(
                matches!(
                    classify(&err),
                    Some(IoFailure::InUse | IoFailure::PermissionDenied)
                ),
                "{err:?}"
            );
            assert_eq!(dir.entries(), ["a.txt"]);
        }
        drop(holder);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        write_atomic(&path, b"new", &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
}
