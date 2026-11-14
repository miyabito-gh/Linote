//! 表示上の行（[`Row`]）。行を、長すぎる場合は一定バイトで区切ったもの。
//!
//! # 区切りの規則
//!
//! 区切りの単位を `seg` バイトとする。ファイル先頭からの位置が `seg` の倍数である点 `G`
//! （格子点）について、**`[G - seg, G]` に行区切りのバイトが 1 つもなければ**、`G` で行を
//! 分ける。分ける位置は UTF-8 の文字の途中にならないよう、高々 3 バイト前へ寄せる
//! （不正な UTF-8 で境界が見つからなければ `G` のまま）。
//!
//! この規則の性質:
//!
//! - 長さが `seg` 未満の行は分かれない。表示上の行は `2 * seg + 4` バイトを超えない。
//! - 分かれ目はファイル上の絶対位置だけで決まる。どこから計算を始めても同じ結果になり、
//!   前方へも後方へも、手前を高々 `2 * seg + 4` バイト読めば求められる（行の先頭まで
//!   戻らなくてよい。数百 MiB の 1 行の途中でも読む量は一定）。
//! - `\r\n` の間では分かれない（`\r` が区切りのバイトなので）。
//!
//! 検索（[`super::search`]）も、同じ規則を大きな `seg` で使って長大な行を分ける。

use std::collections::VecDeque;

use crate::core::{Error, Result};
use crate::document::{TextRange, TextSource};

use super::lines::{find_terminator, is_continuation, is_terminator, rfind_terminator};
use super::source::BigFile;

/// 表示上の行 1 つ。位置はすべてファイル先頭からのバイト数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// 内容の先頭。
    pub start: u64,
    /// 内容の末尾（含まない）。行区切りは含まない。
    pub end: u64,
    /// 次の表示上の行の先頭（行区切りの次）。最後の行ではファイルの長さ。
    pub next: u64,
    /// 行の先頭から始まるか（偽なら、長い行の途中から始まる）。
    pub starts_line: bool,
    /// 行区切りで終わるか（偽なら、長い行の途中で終わるか、ファイルの末尾）。
    pub ends_line: bool,
    /// ファイルの最後の表示上の行か。
    pub last: bool,
}

impl Row {
    /// 内容の範囲。
    pub fn range(&self) -> TextRange {
        TextRange::new(self.start, self.end)
    }

    /// 内容の長さ（バイト）。
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// `offset` がこの行（内容または行区切り）にあるか。最後の行は末尾の位置も含む。
    pub fn contains(&self, offset: u64) -> bool {
        self.start <= offset && (offset < self.next || self.last)
    }
}

/// 表示上の行を前から順に求めるための状態。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowState {
    /// 次の行の先頭。
    pub pos: u64,
    /// ここから `pos` までに行区切りのバイトがないと分かっている位置。
    pub clean_from: u64,
    pub starts_line: bool,
}

impl RowState {
    pub(crate) fn line_start(pos: u64) -> Self {
        Self {
            pos,
            clean_from: pos,
            starts_line: true,
        }
    }
}

/// ファイルの一部を読み込んでおく作業領域。
pub(crate) struct Window<'a> {
    file: &'a BigFile,
    buf: Vec<u8>,
    start: u64,
    filled: usize,
    /// キャッシュを通さずに読むか（検索用。表示用のキャッシュを追い出さない）。
    direct: bool,
}

impl<'a> Window<'a> {
    pub(crate) fn new(file: &'a BigFile, capacity: usize, direct: bool) -> Self {
        // ファイルより大きな領域は要らない。
        let capacity = capacity.min(
            usize::try_from(file.len())
                .unwrap_or(usize::MAX)
                .saturating_add(8),
        );
        Self {
            file,
            buf: vec![0u8; capacity],
            start: 0,
            filled: 0,
            direct,
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// `start` から容量いっぱい（または末尾）まで読み込む。
    pub(crate) fn load(&mut self, start: u64) -> Result<()> {
        self.load_len(start, self.buf.len())
    }

    /// `start` から高々 `max` バイトを読み込む。
    pub(crate) fn load_len(&mut self, start: u64, max: usize) -> Result<()> {
        let want = max.min(self.buf.len());
        self.filled = 0;
        self.start = start;
        self.filled = if self.direct {
            self.file.read_direct(start, &mut self.buf[..want])?
        } else {
            self.file.read_at(start, &mut self.buf[..want])?
        };
        Ok(())
    }

    pub(crate) fn start(&self) -> u64 {
        self.start
    }

    pub(crate) fn end(&self) -> u64 {
        self.start + self.filled as u64
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.buf[..self.filled]
    }

    /// `[start, end)` のバイト列。範囲は読み込み済みであること。
    pub(crate) fn slice(&self, start: u64, end: u64) -> &[u8] {
        &self.buf[(start - self.start) as usize..(end - self.start) as usize]
    }
}

/// 格子点 `grid` を、文字の途中でない位置まで前へ寄せる。`grid - 3 ..= grid` は `bytes`
/// の中にあること。
fn adjust_split(bytes: &[u8], window_start: u64, grid: u64) -> u64 {
    for back in 0..4u64 {
        if !is_continuation(bytes[(grid - back - window_start) as usize]) {
            return grid - back;
        }
    }
    grid
}

/// `state` の位置から始まる表示上の行を 1 つ求める。読み込み済みの範囲（`bytes`、先頭位置
/// `window_start`）で足りなければ `None`（`state.pos` から読み込み直して呼び直す）。
fn step(
    bytes: &[u8],
    window_start: u64,
    file_len: u64,
    seg: u64,
    state: &RowState,
) -> Option<(Row, RowState)> {
    let window_end = window_start + bytes.len() as u64;
    let pos = state.pos;
    if pos < window_start || pos > window_end {
        return None;
    }
    if pos == file_len {
        // 空の最後の行（空のファイル、または行区切りで終わるファイル）。
        let row = Row {
            start: pos,
            end: pos,
            next: pos,
            starts_line: state.starts_line,
            ends_line: false,
            last: true,
        };
        return Some((row, *state));
    }
    let at = |p: u64| bytes[(p - window_start) as usize];
    // 行区切りで終わる行。`\r` は次のバイトを見て `\r\n` かどうかを決める。
    let line_end = |t: u64| -> Option<(Row, RowState)> {
        let next = if at(t) == b'\n' {
            t + 1
        } else if t + 1 < window_end {
            if at(t + 1) == b'\n' {
                t + 2
            } else {
                t + 1
            }
        } else if t + 1 == file_len {
            t + 1
        } else {
            return None;
        };
        let row = Row {
            start: pos,
            end: t,
            next,
            starts_line: state.starts_line,
            ends_line: true,
            last: false,
        };
        Some((row, RowState::line_start(next)))
    };

    // 次に分かれうる格子点: `[grid - seg, grid)` に区切りがないと言える最初の点。
    // `pos + 4` は、直前の分かれ目（格子点から高々 3 バイト前）と同じ点を選ばないため。
    let lower = (pos + 4).max(state.clean_from.saturating_add(seg));
    let grid = lower.div_ceil(seg).saturating_mul(seg);
    let limit = grid.min(file_len);
    let scan_end = limit.min(window_end);
    if let Some(i) =
        find_terminator(&bytes[(pos - window_start) as usize..(scan_end - window_start) as usize])
    {
        return line_end(pos + i as u64);
    }
    if scan_end < limit {
        return None;
    }
    if limit == file_len {
        let row = Row {
            start: pos,
            end: file_len,
            next: file_len,
            starts_line: state.starts_line,
            ends_line: false,
            last: true,
        };
        return Some((row, RowState::line_start(file_len)));
    }
    if grid >= window_end {
        return None;
    }
    if is_terminator(at(grid)) {
        return line_end(grid);
    }
    let cut = adjust_split(bytes, window_start, grid);
    let row = Row {
        start: pos,
        end: cut,
        next: cut,
        starts_line: state.starts_line,
        ends_line: false,
        last: false,
    };
    let next = RowState {
        pos: cut,
        clean_from: state.clean_from,
        starts_line: false,
    };
    Some((row, next))
}

/// `state` の位置から始まる表示上の行を求め、`state` を次の行へ進める。
/// 行の内容は `window.slice(row.start, row.end)` で読める。
pub(crate) fn next_row(window: &mut Window<'_>, seg: u64, state: &mut RowState) -> Result<Row> {
    let file_len = window.file.len();
    let mut reloaded = false;
    loop {
        if let Some((row, next)) = step(window.bytes(), window.start(), file_len, seg, state) {
            *state = next;
            return Ok(row);
        }
        if reloaded {
            return Err(Error::Internal("表示上の行が作業領域に収まりません".into()));
        }
        window.load(state.pos)?;
        reloaded = true;
    }
}

/// [`next_row`] と同じだが、読み込み直さない。行が読み込み済みの範囲に収まっている
/// ことを呼び出し側が保証する。
pub(crate) fn row_in_window(window: &Window<'_>, seg: u64, state: &mut RowState) -> Result<Row> {
    let file_len = window.file.len();
    match step(window.bytes(), window.start(), file_len, seg, state) {
        Some((row, next)) => {
            *state = next;
            Ok(row)
        }
        None => Err(Error::Internal(
            "表示上の行が読み込み済みの範囲に収まりません".into(),
        )),
    }
}

/// `offset` を含む表示上の行と、その行の先頭での状態を求める。
///
/// 読む量は手前の高々 `2 * seg + 4` バイトと、その行自身。
pub(crate) fn locate(window: &mut Window<'_>, seg: u64, offset: u64) -> Result<(RowState, Row)> {
    let file = window.file;
    let len = file.len();
    let mut offset = offset.min(len);
    // `\r\n` の `\n` を指していたら `\r` に寄せる（どちらも同じ行の行区切り）。
    if offset > 0 && offset < len {
        window.load_len(offset - 1, 2)?;
        if window.bytes() == b"\r\n" {
            offset -= 1;
        }
    }
    let mut state = line_start_before(window, seg, offset)?;
    loop {
        let before = state;
        let row = next_row(window, seg, &mut state)?;
        if row.contains(offset) {
            return Ok((before, row));
        }
    }
}

/// `offset` 以前の、表示上の行の先頭を 1 つ求める（`offset` を含む行か、その少し手前）。
fn line_start_before(window: &mut Window<'_>, seg: u64, offset: u64) -> Result<RowState> {
    // まず近くだけを見る。普通の行ならここで行の先頭が見つかる。
    let near = offset.min(256);
    let full = offset.min(2 * seg + 4);
    for back in [near, full] {
        let from = offset - back;
        window.load_len(from, back as usize)?;
        if let Some(i) = rfind_terminator(window.bytes()) {
            return Ok(RowState::line_start(from + i as u64 + 1));
        }
        if from == 0 {
            return Ok(RowState::line_start(0));
        }
        if back == full {
            // `[from, offset)` に行区切りがない。長い行の途中なので、手前の格子点で
            // 分かれている: `grid - seg >= from` かつ `grid < offset`。
            let grid = (offset - 1) / seg * seg;
            debug_assert!(grid >= from + seg);
            return Ok(RowState {
                pos: adjust_split(window.bytes(), from, grid),
                clean_from: from,
                starts_line: false,
            });
        }
    }
    unreachable!()
}

impl BigFile {
    fn display_window(&self) -> Window<'_> {
        let seg = self.config().segment_bytes as usize;
        Window::new(self, 8 * seg + 64, false)
    }

    /// `offset` を含む表示上の行。`offset` は任意の位置でよい（文字の途中、行区切りの上、
    /// 末尾でも）。
    pub fn row_at(&self, offset: u64) -> Result<Row> {
        let seg = self.config().segment_bytes as u64;
        let mut window = self.display_window();
        Ok(locate(&mut window, seg, offset)?.1)
    }

    /// `offset` を含む表示上の行から、前方へ高々 `max_rows` 行。
    ///
    /// 続きを読むには、最後の行の [`Row::next`] を渡す。最後の行（[`Row::last`]）まで
    /// 返したら、それ以上はない。読む量は返す行の分だけで、索引の完成を待たない。
    pub fn rows_forward(&self, offset: u64, max_rows: usize) -> Result<Vec<Row>> {
        let seg = self.config().segment_bytes as u64;
        let mut window = self.display_window();
        let mut rows = Vec::with_capacity(max_rows.min(1024));
        if max_rows == 0 {
            return Ok(rows);
        }
        let (mut state, _) = locate(&mut window, seg, offset)?;
        while rows.len() < max_rows {
            let row = next_row(&mut window, seg, &mut state)?;
            rows.push(row);
            if row.last {
                break;
            }
        }
        Ok(rows)
    }

    /// 先頭が `before` より前にある表示上の行のうち、最後の高々 `max_rows` 行（昇順）。
    /// 上方向のスクロールに使う。`before` に `u64::MAX` を渡すとファイルの末尾の行まで含む
    /// （[`last_rows`](Self::last_rows)）。
    pub fn rows_backward(&self, before: u64, max_rows: usize) -> Result<Vec<Row>> {
        let seg = self.config().segment_bytes as u64;
        let mut window = self.display_window();
        let mut rows: VecDeque<Row> = VecDeque::with_capacity(max_rows.min(1024));
        if max_rows == 0 || before == 0 {
            return Ok(Vec::new());
        }
        let anchor = before.min(self.len());
        // 行が短ければ少し戻るだけで足りる。足りなければ戻る量を増やす。
        let widest = (max_rows as u64).saturating_mul(2 * seg + 8);
        let mut span = (max_rows as u64).saturating_mul(128).clamp(seg, widest);
        loop {
            rows.clear();
            let (mut state, _) = locate(&mut window, seg, anchor.saturating_sub(span))?;
            let reached_top = state.pos == 0;
            loop {
                let row = next_row(&mut window, seg, &mut state)?;
                if row.start >= before {
                    break;
                }
                if rows.len() == max_rows {
                    rows.pop_front();
                }
                rows.push_back(row);
                if row.last {
                    break;
                }
            }
            if rows.len() == max_rows || reached_top || span >= widest {
                return Ok(rows.into());
            }
            span = span.saturating_mul(4).min(widest);
        }
    }

    /// ファイルの最後の高々 `max_rows` 行（昇順）。末尾への移動に使う。
    pub fn last_rows(&self, max_rows: usize) -> Result<Vec<Row>> {
        self.rows_backward(u64::MAX, max_rows)
    }

    /// 表示上の行の内容を文字列として読む。不正な UTF-8 は U+FFFD にする。
    pub fn row_text(&self, row: &Row) -> Result<String> {
        self.read_text(row.range(), row.len())
    }
}
