//! [`BigFile`]: 開いた巨大ファイル。部分読み込み、キャッシュ、行索引、無効化。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::core::{Error, Result, TaskContext, WorkerPool};
use crate::document::{LineCount, TextRange, TextSource};
use crate::file::FileStamp;

use super::cache::BlockCache;
use super::index::{scan_chunk, Checkpoint, LineIndex};
use super::lines::{count_breaks, find_break_end, is_continuation};
use super::raw::RawFile;
use super::{BigFileConfig, INDEX_CHUNK_BYTES};

/// キャッシュを通さずに読んだ量がこれだけたまるごとに、パスの状態（置換・削除）も確かめる。
const PATH_CHECK_INTERVAL_BYTES: u64 = 16 * 1024 * 1024;
/// 行の先頭・行番号を求めるときに一度に読む量。
const LOOKUP_CHUNK_BYTES: usize = 16 * 1024;

/// メモリ使用量（バイト）。作業領域（索引作成・検索の間だけ確保するもの）は含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryUsage {
    pub cache_bytes: u64,
    pub index_bytes: u64,
}

/// 行索引の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexStatus {
    /// 索引が末尾まで済んだか。
    pub complete: bool,
    /// 索引が済んだ位置（バイト）。これより前の位置は行番号を求められる。
    pub indexed_bytes: u64,
    /// これまでに確かめた行数。`complete` なら総行数。
    pub lines: u64,
}

/// 開いた巨大ファイル 1 つ。読み取り専用。`Send + Sync` なので `Arc` に入れて UI と
/// ワーカーで共有する。
///
/// 長さは開いたときの値で固定する。ファイルが外部で変わったら無効になり
/// （モジュールの説明を参照）、以後の操作は [`Error::ExternalChange`] を返す。
pub struct BigFile {
    raw: RawFile,
    len: u64,
    config: BigFileConfig,
    cache: Mutex<BlockCache>,
    index: Mutex<LineIndex>,
    invalid: AtomicBool,
    indexing: AtomicBool,
    /// 前回パスの状態を確かめてから、キャッシュを通さずに読んだ量。
    unchecked_bytes: AtomicU64,
}

impl std::fmt::Debug for BigFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigFile")
            .field("path", &self.raw.path())
            .field("len", &self.len)
            .field("invalid", &self.is_invalidated())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 索引作成の重複を防ぐ印。作成が終わったら（中止・失敗でも）外す。
struct IndexingGuard<'a>(&'a AtomicBool);

impl Drop for IndexingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl BigFile {
    /// ファイルを共有読み取りで開く。メタデータだけを読み、本文は読まない。
    pub fn open(path: impl Into<PathBuf>, config: BigFileConfig) -> Result<Self> {
        let path = path.into();
        let config = config.normalized();
        let raw = RawFile::open(&path)?;
        let len = raw.len();
        Ok(Self {
            raw,
            len,
            config,
            cache: Mutex::new(BlockCache::new(config.max_cached_blocks())),
            index: Mutex::new(LineIndex::new(
                config.index_stride_bytes,
                config.max_index_entries(),
            )),
            invalid: AtomicBool::new(false),
            indexing: AtomicBool::new(false),
            unchecked_bytes: AtomicU64::new(0),
        })
    }

    /// 同じパス・同じ設定で開き直す（無効になったあとに使う）。
    pub fn reopen(&self) -> Result<Self> {
        Self::open(self.raw.path(), self.config)
    }

    pub fn path(&self) -> &Path {
        self.raw.path()
    }

    /// 丸めたあとの設定。
    pub fn config(&self) -> &BigFileConfig {
        &self.config
    }

    /// 開いたときのディスク上の状態（[`DocumentState::open_file`](crate::document::DocumentState::open_file)
    /// に渡す）。
    pub fn stamp(&self) -> FileStamp {
        self.raw.stamp()
    }

    /// 外部変更を検知して無効になったか。
    pub fn is_invalidated(&self) -> bool {
        self.invalid.load(Ordering::Acquire)
    }

    pub fn memory_usage(&self) -> MemoryUsage {
        MemoryUsage {
            cache_bytes: lock(&self.cache).bytes(),
            index_bytes: lock(&self.index).memory_bytes(),
        }
    }

    fn external_change(&self) -> Error {
        Error::ExternalChange {
            path: self.raw.path().to_path_buf(),
        }
    }

    pub(crate) fn ensure_valid(&self) -> Result<()> {
        if self.is_invalidated() {
            Err(self.external_change())
        } else {
            Ok(())
        }
    }

    /// 無効にして、キャッシュと索引を捨てる。
    fn invalidate(&self) -> Error {
        self.invalid.store(true, Ordering::Release);
        lock(&self.cache).clear();
        lock(&self.index).clear();
        self.external_change()
    }

    fn io_error(&self, source: std::io::Error) -> Error {
        Error::io("読み込み", self.raw.path(), source)
    }

    /// 元ファイルが外部で変わっていないか確かめる（ハンドルとパスの両方）。
    ///
    /// 変わっていれば無効にして [`Error::ExternalChange`] を返す。ウィンドウが前面に来た
    /// とき・検索や行ジャンプの前など、UI が適宜呼ぶ。入出力はメタデータの取得だけ。
    pub fn check_external_change(&self) -> Result<()> {
        self.ensure_valid()?;
        self.unchecked_bytes.store(0, Ordering::Relaxed);
        let changed = self.raw.handle_changed().map_err(|e| self.io_error(e))?
            || self.raw.path_changed().map_err(|e| self.io_error(e))?;
        if changed {
            return Err(self.invalidate());
        }
        Ok(())
    }

    /// キャッシュを通さずに読む。`buf` が埋まるか末尾に達するまで読み、読んだ量を返す。
    ///
    /// 読んだ**あと**でハンドルの状態を確かめるので、返したデータは確かめた時点の
    /// ファイルのもの。
    pub(crate) fn read_direct(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.ensure_valid()?;
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.len - offset) as usize;
        let got = self
            .raw
            .read_at(offset, &mut buf[..want])
            .map_err(|e| self.io_error(e))?;
        let changed = self.raw.handle_changed().map_err(|e| self.io_error(e))?;
        if changed || got < want {
            return Err(self.invalidate());
        }
        let unchecked = self
            .unchecked_bytes
            .fetch_add(got as u64, Ordering::Relaxed)
            + got as u64;
        if unchecked >= PATH_CHECK_INTERVAL_BYTES {
            self.unchecked_bytes.store(0, Ordering::Relaxed);
            if self.raw.path_changed().map_err(|e| self.io_error(e))? {
                return Err(self.invalidate());
            }
        }
        Ok(got)
    }

    fn block(&self, index: u64) -> Result<Arc<[u8]>> {
        if let Some(block) = lock(&self.cache).get(index) {
            return Ok(block);
        }
        let block_bytes = self.config.block_bytes as u64;
        let start = index * block_bytes;
        let size = block_bytes.min(self.len - start) as usize;
        let mut data = vec![0u8; size];
        self.read_direct(start, &mut data)?;
        let data: Arc<[u8]> = data.into();
        let mut cache = lock(&self.cache);
        // 無効化は印を立ててからキャッシュを空にする。ここで印を見れば、空にしたあとに
        // 古いブロックを入れてしまうことはない。
        if !self.is_invalidated() {
            cache.insert(index, Arc::clone(&data));
        }
        Ok(data)
    }

    /// キャッシュ越しに読む。
    fn read_cached(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.ensure_valid()?;
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.len - offset) as usize;
        let block_bytes = self.config.block_bytes as u64;
        let mut done = 0usize;
        while done < want {
            let pos = offset + done as u64;
            let index = pos / block_bytes;
            let block = self.block(index)?;
            let skip = (pos - index * block_bytes) as usize;
            let n = (block.len() - skip).min(want - done);
            buf[done..done + n].copy_from_slice(&block[skip..skip + n]);
            done += n;
        }
        Ok(want)
    }

    /// 位置 `offset` のバイト。末尾以降なら `None`。
    pub(crate) fn byte_at(&self, offset: u64) -> Result<Option<u8>> {
        let mut b = [0u8; 1];
        Ok((self.read_cached(offset, &mut b)? == 1).then_some(b[0]))
    }

    /// `offset` を、UTF-8 の文字の途中でない位置まで前へ寄せる（高々 3 バイト）。
    /// 選択範囲や読み取り範囲の端を文字の境界に合わせるために使う。
    pub fn snap_to_char_boundary(&self, offset: u64) -> Result<u64> {
        let offset = offset.min(self.len);
        if offset == self.len {
            return Ok(offset);
        }
        let start = offset.saturating_sub(3);
        let mut buf = [0u8; 4];
        let n = self.read_cached(start, &mut buf[..(offset - start) as usize + 1])?;
        for back in 0..n {
            if !is_continuation(buf[n - 1 - back]) {
                return Ok(offset - back as u64);
            }
        }
        Ok(offset)
    }

    /// 範囲を文字列として読む（範囲コピー・表示用）。不正な UTF-8 は U+FFFD にする。
    /// `max_bytes` を超える範囲は [`Error::LimitExceeded`]。
    pub fn read_text(&self, range: TextRange, max_bytes: u64) -> Result<String> {
        let bytes = self.read_range(range, max_bytes)?;
        Ok(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        })
    }

    /// 先頭に UTF-8 の BOM があるか。BOM は本文の位置に含まれる（位置 0〜2）。
    pub fn has_bom(&self) -> Result<bool> {
        let mut head = [0u8; 3];
        Ok(self.read_cached(0, &mut head)? == 3 && head == [0xEF, 0xBB, 0xBF])
    }

    /// 先頭の 1 ブロックが UTF-8 として読めるか（末尾の文字が途中で切れているのは許す）。
    /// 巨大モードは UTF-8 だけを対象にするので、開いた直後の案内に使う。
    pub fn looks_like_utf8(&self) -> Result<bool> {
        let mut head = vec![0u8; (self.config.block_bytes as u64).min(self.len) as usize];
        let n = self.read_cached(0, &mut head)?;
        Ok(match std::str::from_utf8(&head[..n]) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none() && (n as u64) < self.len && n - e.valid_up_to() <= 3,
        })
    }

    // ---- 行索引 ----

    pub fn index_status(&self) -> IndexStatus {
        let index = lock(&self.index);
        IndexStatus {
            complete: index.is_complete(),
            indexed_bytes: index.scanned_to(),
            lines: index.lines_seen(),
        }
    }

    /// 行索引を末尾まで作る。**ワーカースレッドで呼ぶ**（[`spawn_index`](Self::spawn_index)）。
    ///
    /// - 進捗の単位はバイト（`done` = 索引が済んだ位置、`total` = ファイルの長さ）。
    ///   [`INDEX_CHUNK_BYTES`] ごとに報告し、同じ間隔で中止を確かめる。
    /// - 中止されたら [`Error::Cancelled`]。そこまでの索引は残り、もう一度呼ぶと続きから
    ///   再開する。
    /// - すでに別のスレッドが作成中なら [`Error::Busy`]。完成済みなら何もしない。
    /// - 途中で外部変更を検知したら [`Error::ExternalChange`]。
    pub fn build_index(&self, ctx: &TaskContext) -> Result<()> {
        self.ensure_valid()?;
        if lock(&self.index).is_complete() {
            return Ok(());
        }
        if self
            .indexing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::Busy {
                what: "行索引の作成".into(),
            });
        }
        let _guard = IndexingGuard(&self.indexing);
        self.check_external_change()?;

        // 1 バイト余分に読み、行区切りの判定（`\r` の次が `\n` か）に使う。
        let mut buf = vec![0u8; INDEX_CHUNK_BYTES + 1];
        let mut found: Vec<Checkpoint> = Vec::new();
        loop {
            ctx.cancel.check()?;
            let mut state = lock(&self.index).scan_state();
            if lock(&self.index).is_complete() {
                return Ok(());
            }
            let base = state.scanned_to;
            let n = self.read_direct(base, &mut buf)?;
            let at_end = base + n as u64 == self.len;
            let (chunk, after) = if at_end {
                (&buf[..n], None)
            } else {
                (&buf[..n - 1], Some(buf[n - 1]))
            };
            found.clear();
            scan_chunk(chunk, after, &mut state, &mut found);
            if at_end {
                // 完成とする前に、置換・削除も確かめる。
                self.check_external_change()?;
            }
            {
                let mut index = lock(&self.index);
                // 無効化されていたら、空にした索引へ書き戻さない。
                self.ensure_valid()?;
                index.extend(&found, state.scanned_to, state.line);
                if at_end {
                    index.finish();
                }
            }
            ctx.report(state.scanned_to, Some(self.len));
            if at_end {
                return Ok(());
            }
        }
    }

    /// [`build_index`](Self::build_index) をワーカーで実行し、終わったら `done` を
    /// （ワーカースレッド上で）呼ぶ。`done` の中で [`UiSender`](crate::core::UiSender) に
    /// 結果と [`DocStamp`](crate::core::DocStamp) を送り、UI スレッドで照合してから反映する。
    pub fn spawn_index(
        self: &Arc<Self>,
        pool: &WorkerPool,
        ctx: TaskContext,
        done: impl FnOnce(Result<()>) + Send + 'static,
    ) {
        let file = Arc::clone(self);
        pool.spawn(move || done(file.build_index(&ctx)));
    }

    /// `[start, end)` を前から順に読み、`visit(バイト列, 直後のバイト, バイト列の先頭位置)`
    /// を呼ぶ。`visit` が `false` を返したらやめる。キャッシュ越し。
    fn scan_cached(
        &self,
        start: u64,
        end: u64,
        mut visit: impl FnMut(&[u8], Option<u8>, u64) -> bool,
    ) -> Result<()> {
        let end = end.min(self.len);
        let mut buf = vec![0u8; LOOKUP_CHUNK_BYTES.min((end.saturating_sub(start)) as usize) + 1];
        let mut pos = start;
        while pos < end {
            let want = ((end - pos) as usize).min(buf.len() - 1);
            // 直後の 1 バイトも（あれば）読む。
            let n = self.read_cached(pos, &mut buf[..want + 1])?;
            let after = (n > want).then(|| buf[want]);
            if !visit(&buf[..want], after, pos) {
                break;
            }
            pos += want as u64;
        }
        Ok(())
    }

    /// `offset` を含む行の行番号（0 始まり）。索引がまだその位置まで済んでいなければ `None`。
    ///
    /// `\r\n` の `\n` を指す位置は、その区切りで終わる行に属する。読む量は索引の控えの
    /// 間隔程度（行の長さによらない）。
    pub fn line_of_offset(&self, offset: u64) -> Result<Option<u64>> {
        self.ensure_valid()?;
        let offset = offset.min(self.len);
        let (checkpoint, bound) = {
            let index = lock(&self.index);
            match index.checkpoint_for_offset(offset) {
                Some(c) => (c, index.scan_bound()),
                None => return Ok(None),
            }
        };
        // 控えから `bound` より先には、次の控えまで行の先頭がない。
        let end = offset.min(checkpoint.offset.saturating_add(bound));
        let mut line = checkpoint.line;
        self.scan_cached(checkpoint.offset, end, |bytes, after, _| {
            line += count_breaks(bytes, after);
            true
        })?;
        Ok(Some(line))
    }

    fn line_start_from(&self, checkpoint: Checkpoint, bound: u64, line: u64) -> Result<u64> {
        if checkpoint.line == line {
            return Ok(checkpoint.offset);
        }
        let mut remaining = line - checkpoint.line;
        let mut result = None;
        let end = checkpoint.offset.saturating_add(bound).saturating_add(1);
        self.scan_cached(checkpoint.offset, end, |bytes, after, base| {
            let mut from = 0;
            while let Some(i) = find_break_end(bytes, from, after) {
                remaining -= 1;
                if remaining == 0 {
                    result = Some(base + i as u64 + 1);
                    return false;
                }
                from = i + 1;
            }
            true
        })?;
        result.ok_or_else(|| Error::Internal("行索引と本文が食い違っています".into()))
    }
}

impl TextSource for BigFile {
    fn len(&self) -> u64 {
        self.len
    }

    /// キャッシュ越しに読む。ブロック 4 個分以上の読み取りはキャッシュを通さない
    /// （大きな範囲コピーで表示用のキャッシュを追い出さないため）。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.len() as u64 >= 4 * self.config.block_bytes as u64 {
            self.read_direct(offset, buf)
        } else {
            self.read_cached(offset, buf)
        }
    }

    fn line_count(&self) -> LineCount {
        let index = lock(&self.index);
        if index.is_complete() {
            LineCount::Exact(index.lines_seen())
        } else {
            LineCount::AtLeast(index.lines_seen())
        }
    }

    /// 索引がまだその行まで済んでいなければ `None`（索引の作成が進めば求められる）。
    /// 完成後は、存在しない行に対して `None`。
    fn line_start(&self, line: u64) -> Result<Option<u64>> {
        self.ensure_valid()?;
        let (checkpoint, bound) = {
            let index = lock(&self.index);
            match index.checkpoint_for_line(line) {
                Some(c) => (c, index.scan_bound()),
                None => return Ok(None),
            }
        };
        self.line_start_from(checkpoint, bound, line).map(Some)
    }
}
