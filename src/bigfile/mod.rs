//! 巨大ファイルの中核（タスク J の担当。`src/bigfile/` 配下）。
//!
//! 責務: バッファ付きの部分読み込み、疎な行索引、裏での索引作成、メモリ上限、
//! 切り詰め・置換を検知したときの無効化。GUI に依存せず、`windows` crate を使わない。
//! 表示は `ui::bigview`（タスク L、DirectWrite）。
//!
//! 取り決め:
//! - 本文は UTF-8 のファイル上にあり、読み取り専用
//!   （[`crate::document::DocumentKind::LargeFile`]。機能は何も持たない）。
//! - [`crate::document::TextSource`] を実装する。バックグラウンドの検索（タスク K）からも
//!   読めるよう `Send + Sync` にする。索引の作成中は
//!   [`crate::document::LineCount::AtLeast`] を返す。
//! - 索引の作成は [`crate::core::WorkerPool`] で行い、[`crate::core::TaskContext`] で
//!   中止・進捗を扱う。先頭の表示は索引の完成を待たない。
//! - 索引・キャッシュは、短い行が大量にある場合も、改行がほぼない長大な 1 行の場合も、
//!   メモリ使用量が決めた上限を超えないこと。上限は定数として公開する。
//! - 元ファイルの切り詰め・置換は [`crate::file::FileStamp`] の比較で検知し、索引と
//!   検索結果を無効にする（文書の版を進めて、古い結果が照合で捨てられるようにする）。
//! - 通常編集と巨大モードの切り替え条件（容量・行数・最大行長）は実測で決め、
//!   判定関数をここに置く。
//!
//! # 使い方（UI・検索から）
//!
//! ```text
//! let file = Arc::new(BigFile::open(path, BigFileConfig::default())?);   // メタデータだけ読む
//! let rows = file.rows_forward(0, 60)?;                                  // 先頭の画面分（先頭のブロックだけ読む）
//! file.spawn_index(WorkerPool::global(), ctx, |result| ...);             // 残りの索引は裏で
//! file.line_start(line)? / file.line_of_offset(offset)?                  // 行ジャンプ・行番号
//! file.read_range(range, DEFAULT_COPY_MAX_BYTES)?                        // 範囲コピー
//! file.find(&pattern, from, Direction::Forward, &ctx)?                   // 検索（ワーカーで）
//! file.check_external_change()?                                          // 前面に来たときなど
//! ```
//!
//! # 構成
//!
//! - [`BigFile`] — 開いた巨大ファイル 1 つ。`Arc` で UI とワーカーが共有する。
//! - 表示上の行（[`Row`]）— 行を、長すぎる場合は一定バイトで区切ったもの。
//! - 検索（[`SearchPattern`]、[`BigFile::find`]）。
//! - 巨大モードへの切り替え判定（[`large_mode_reason`]）。
//!
//! # メモリの上限
//!
//! | 対象 | 上限 |
//! |---|---|
//! | ブロックキャッシュ | [`BigFileConfig::cache_budget_bytes`]（最低 1 ブロック） |
//! | 行索引 | [`BigFileConfig::index_budget_bytes`] |
//! | 索引作成の作業領域 | [`INDEX_CHUNK_BYTES`] + 1（作成中だけ） |
//! | 検索の作業領域 | 検索 1 回につき [`SearchPattern::working_bytes`]（検索中だけ） |
//! | 表示上の行の計算 | 1 回につき `segment_bytes` の 8 倍程度（呼び出しの間だけ） |
//!
//! どれもファイルの大きさ・行数・行の長さに依存しない。
//!
//! # 外部変更（切り詰め・置換）
//!
//! ディスクから読むたびにハンドルの長さ・更新時刻を、一定量を読むたび・長い処理の
//! 始めと終わり・[`BigFile::check_external_change`] でパスの状態（置換・削除）を、開いた
//! ときの控えと比べる。食い違えばその [`BigFile`] は**無効**になり、キャッシュと索引を
//! 捨て、以後の操作はすべて [`Error::ExternalChange`](crate::core::Error::ExternalChange)
//! を返す（進行中の索引作成・検索もこのエラーで終わる）。追記も変更として扱う。
//!
//! 無効になったら UI は文書の版を進め（古い検索結果・索引の進捗が照合で捨てられる）、
//! 利用者に知らせ、[`BigFile::reopen`] で開き直す。
//!
//! 検知できないもの: 長さも更新時刻も変わらない上書き（Windows では書き込み側が
//! ハンドルを閉じるまで更新時刻が変わらないことがある）。

mod cache;
mod index;
mod lines;
mod raw;
mod rows;
mod search;
mod source;
#[cfg(test)]
mod tests;

pub use rows::Row;
pub use search::{Direction, SearchKind, SearchNotes, SearchPattern, SearchResult};
pub use source::{BigFile, IndexStatus, MemoryUsage};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// ブロックキャッシュの既定のメモリ予算。
pub const DEFAULT_CACHE_BUDGET_BYTES: u64 = 32 * MIB;
/// 行索引の既定のメモリ予算。既定の間隔なら 8 GiB まで間引かずに収まる。
pub const DEFAULT_INDEX_BUDGET_BYTES: u64 = 4 * MIB;
/// キャッシュの 1 ブロックの既定の長さ。
pub const DEFAULT_BLOCK_BYTES: u32 = 64 * KIB as u32;
/// 行索引の控えの既定の間隔。行ジャンプ 1 回の走査量の目安でもある。
pub const DEFAULT_INDEX_STRIDE_BYTES: u64 = 64 * KIB;
/// 表示上の行の既定の区切り。1 つの表示上の行は、この 2 倍 + 4 バイトを超えない。
pub const DEFAULT_SEGMENT_BYTES: u32 = 4 * KIB as u32;
/// 正規表現検索で、長大な行を分ける既定の単位。
pub const DEFAULT_SEARCH_PIECE_BYTES: u32 = 256 * KIB as u32;
/// 索引作成が一度に読む量。進捗の報告と中止の確認もこの単位。
pub const INDEX_CHUNK_BYTES: usize = MIB as usize;
/// 範囲コピーの上限の既定値（[`TextSource::read_range`](crate::document::TextSource::read_range)
/// の `max_bytes` に渡す）。
pub const DEFAULT_COPY_MAX_BYTES: u64 = 64 * MIB;
/// 検索文字列の長さの上限（バイト）。
pub const MAX_PATTERN_BYTES: usize = 16 * KIB as usize;

/// [`BigFile`] の設定。メモリ予算は利用者の設定値から渡す。
///
/// 範囲外の値は [`BigFile::open`] が丸める（[`normalized`](Self::normalized)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigFileConfig {
    /// ブロックキャッシュが保持してよいバイト数。1 ブロックに満たなければ 1 ブロック。
    pub cache_budget_bytes: u64,
    /// 行索引が使ってよいバイト数。足りなくなると控えの間隔を広げる。
    pub index_budget_bytes: u64,
    /// キャッシュの 1 ブロックの長さ。
    pub block_bytes: u32,
    /// 行索引の控えの間隔（最初の値）。
    pub index_stride_bytes: u64,
    /// 表示上の行の区切り。
    pub segment_bytes: u32,
    /// 正規表現検索で長大な行を分ける単位。
    pub search_piece_bytes: u32,
}

impl Default for BigFileConfig {
    fn default() -> Self {
        Self {
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
            index_budget_bytes: DEFAULT_INDEX_BUDGET_BYTES,
            block_bytes: DEFAULT_BLOCK_BYTES,
            index_stride_bytes: DEFAULT_INDEX_STRIDE_BYTES,
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            search_piece_bytes: DEFAULT_SEARCH_PIECE_BYTES,
        }
    }
}

impl BigFileConfig {
    /// キャッシュのメモリ予算だけを指定した設定。
    pub fn with_cache_budget(cache_budget_bytes: u64) -> Self {
        Self {
            cache_budget_bytes,
            ..Self::default()
        }
    }

    /// 動作できる範囲に丸めた設定。
    pub fn normalized(mut self) -> Self {
        self.block_bytes = self.block_bytes.clamp(64, 16 * MIB as u32);
        self.segment_bytes = self.segment_bytes.clamp(16, MIB as u32);
        self.search_piece_bytes = self.search_piece_bytes.clamp(64, 16 * MIB as u32);
        self.index_stride_bytes = self.index_stride_bytes.max(16);
        self.cache_budget_bytes = self.cache_budget_bytes.max(self.block_bytes as u64);
        self.index_budget_bytes = self.index_budget_bytes.max(4 * KIB);
        self
    }

    /// キャッシュが保持するブロック数の上限。
    pub fn max_cached_blocks(&self) -> usize {
        ((self.cache_budget_bytes / self.block_bytes.max(1) as u64).max(1)).min(1 << 24) as usize
    }

    /// 行索引の控えの数の上限。領域を伸ばすときに一時的に古い領域と新しい領域が
    /// 並ぶので、予算の半分までを控えに使う。
    pub fn max_index_entries(&self) -> usize {
        ((self.index_budget_bytes / (2 * index::CHECKPOINT_BYTES)).max(2)).min(1 << 28) as usize
    }
}

/// 通常編集（Scintilla に全文を読み込む）をやめて巨大モードにする条件。
///
/// 既定値は暫定（Scintilla 側の実測は統合時に行う）。根拠:
/// 通常編集は全文をメモリに持ち、変換・保存・復元のたびに写しを作るので、容量は
/// 数百 MiB が限度。Scintilla は 1 行が極端に長いと配置の計算が遅くなる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LargeModeThresholds {
    /// これを超える容量（バイト）なら巨大モード。
    pub max_bytes: u64,
    /// これを超える行数なら巨大モード。
    pub max_lines: u64,
    /// これを超える長さ（バイト）の行があれば巨大モード。
    pub max_line_bytes: u64,
}

impl LargeModeThresholds {
    pub const DEFAULT: LargeModeThresholds = LargeModeThresholds {
        max_bytes: 256 * MIB,
        max_lines: 10_000_000,
        max_line_bytes: 8 * MIB,
    };
}

/// ファイルの概形。容量は開く前に分かる。行数と最大行長は読んでみないと分からないので、
/// 分かった時点で埋める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileProfile {
    pub bytes: u64,
    pub lines: Option<u64>,
    pub max_line_bytes: Option<u64>,
}

impl FileProfile {
    /// 容量だけが分かっている状態。
    pub fn of_size(bytes: u64) -> Self {
        Self {
            bytes,
            lines: None,
            max_line_bytes: None,
        }
    }

    /// メモリ上の本文（UTF-8）から求める。通常の読み込みのあと、Scintilla に渡す前の
    /// 判定に使う。
    pub fn of_text(text: &[u8]) -> Self {
        let mut lines = 1u64;
        let mut max_line = 0u64;
        let mut start = 0usize;
        while let Some(end) = lines::find_break_end(text, start, None) {
            let terminator = if text[end] == b'\n' && end > start && text[end - 1] == b'\r' {
                2
            } else {
                1
            };
            max_line = max_line.max((end + 1 - terminator - start) as u64);
            lines += 1;
            start = end + 1;
        }
        max_line = max_line.max((text.len() - start) as u64);
        Self {
            bytes: text.len() as u64,
            lines: Some(lines),
            max_line_bytes: Some(max_line),
        }
    }
}

/// 巨大モードにする理由。画面の案内に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LargeModeReason {
    /// 容量が大きい。
    Size,
    /// 行数が多い。
    LineCount,
    /// 長大な行がある。
    LongLine,
}

/// 巨大モードで開くべきなら理由を返す。通常編集でよければ `None`。
///
/// 開く前は容量だけで判定し（[`FileProfile::of_size`]）、読み込んだあとに行数と最大行長を
/// 加えてもう一度判定する（[`FileProfile::of_text`]）。
pub fn large_mode_reason(
    profile: &FileProfile,
    thresholds: &LargeModeThresholds,
) -> Option<LargeModeReason> {
    if profile.bytes > thresholds.max_bytes {
        Some(LargeModeReason::Size)
    } else if profile.lines.is_some_and(|n| n > thresholds.max_lines) {
        Some(LargeModeReason::LineCount)
    } else if profile
        .max_line_bytes
        .is_some_and(|n| n > thresholds.max_line_bytes)
    {
        Some(LargeModeReason::LongLine)
    } else {
        None
    }
}
